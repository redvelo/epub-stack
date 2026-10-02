use super::*;

pub(super) struct FingerprintingReader<'a> {
    inner: &'a mut dyn Read,
    hasher: Option<blake3::Hasher>,
    remaining: Option<u64>,
    bytes_hashed: u64,
    pub(crate) exceeded: bool,
    eof: bool,
    read_failed: bool,
}

impl<'a> FingerprintingReader<'a> {
    pub(super) fn new(inner: &'a mut dyn Read, enabled: bool, remaining: Option<u64>) -> Self {
        Self {
            inner,
            hasher: enabled.then(blake3::Hasher::new),
            remaining,
            bytes_hashed: 0,
            exceeded: false,
            eof: false,
            read_failed: false,
        }
    }

    pub(super) fn read_failed(&self) -> bool {
        self.read_failed
    }

    pub(super) fn eof(&self) -> bool {
        self.eof
    }

    pub(super) fn needs_fingerprint_drain(&self) -> bool {
        self.hasher.is_some() && !self.exceeded && !self.eof && !self.read_failed
    }

    pub(super) fn finish(
        mut self,
        exceeded_issue: AnalysisIssue,
    ) -> (AnalysisOutcome<Blake3Hash>, u64) {
        let outcome = if self.exceeded {
            AnalysisOutcome::Unavailable(exceeded_issue)
        } else if self.read_failed || !self.eof {
            AnalysisOutcome::Unavailable(AnalysisIssue::Unreadable)
        } else {
            AnalysisOutcome::Complete(Blake3Hash::from_bytes(
                *self
                    .hasher
                    .take()
                    .expect("enabled fingerprint reader has a hasher")
                    .finalize()
                    .as_bytes(),
            ))
        };
        (outcome, self.bytes_hashed)
    }
}

impl Read for FingerprintingReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let read_limit = if self.hasher.is_some() && !self.exceeded {
            self.remaining.map_or(buffer.len(), |remaining| {
                buffer
                    .len()
                    .min(remaining.saturating_add(1).try_into().unwrap_or(usize::MAX))
            })
        } else {
            buffer.len()
        };
        let read = match self.inner.read(&mut buffer[..read_limit]) {
            Ok(read) => read,
            Err(error) => {
                self.read_failed = true;
                return Err(error);
            }
        };
        if read == 0 {
            self.eof = true;
            return Ok(0);
        }
        if let Some(hasher) = &mut self.hasher
            && !self.exceeded
        {
            let hash_bytes = self.remaining.map_or(read, |remaining| {
                read.min(remaining.try_into().unwrap_or(usize::MAX))
            });
            hasher.update(&buffer[..hash_bytes]);
            self.bytes_hashed = self.bytes_hashed.saturating_add(hash_bytes as u64);
            if let Some(remaining) = &mut self.remaining {
                *remaining = remaining.saturating_sub(hash_bytes as u64);
                if hash_bytes < read {
                    self.exceeded = true;
                }
            }
        }
        Ok(read)
    }
}

pub(super) struct SemanticReader<R> {
    inner: R,
    remaining: Option<u64>,
    bytes_read: u64,
    crossed: bool,
    captured: Option<Vec<u8>>,
}

impl<R: Read> SemanticReader<R> {
    pub(super) fn new(inner: R, limit: Option<u64>, capture: bool) -> Self {
        Self {
            inner,
            remaining: limit,
            bytes_read: 0,
            crossed: false,
            captured: capture.then(Vec::new),
        }
    }

    pub(super) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub(super) fn crossed_limit(&self) -> bool {
        self.crossed
    }

    pub(super) fn check_crossing(&mut self) {
        if self.remaining == Some(0) && !self.crossed {
            let mut sentinel = [0u8; 1];
            match self.inner.read(&mut sentinel) {
                Ok(0) => {}
                Ok(_) => self.crossed = true,
                Err(_) => {}
            }
        }
    }

    pub(super) fn drain_to_boundary(&mut self) {
        let _ = std::io::copy(self, &mut std::io::sink());
        self.check_crossing();
    }

    pub(super) fn into_captured(self) -> Vec<u8> {
        self.captured.unwrap_or_default()
    }
}

impl<R: Read> Read for SemanticReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() || self.crossed {
            return Ok(0);
        }
        if self.remaining == Some(0) {
            self.check_crossing();
            return Ok(0);
        }
        let read_limit = self.remaining.map_or(buffer.len(), |remaining| {
            buffer.len().min(remaining.try_into().unwrap_or(usize::MAX))
        });
        let read = self.inner.read(&mut buffer[..read_limit])?;
        self.bytes_read = self.bytes_read.saturating_add(read as u64);
        if let Some(remaining) = &mut self.remaining {
            *remaining = remaining.saturating_sub(read as u64);
        }
        if let Some(captured) = &mut self.captured {
            captured.extend_from_slice(&buffer[..read]);
        }
        Ok(read)
    }
}

pub(super) fn read_prefix(reader: &mut FingerprintingReader<'_>, limit: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut limited = reader.take(limit);
    let _ = limited.read_to_end(&mut bytes);
    bytes
}

pub(super) fn drain_for_fingerprint(reader: &mut FingerprintingReader<'_>) {
    let mut buffer = [0u8; 64 * 1024];
    while reader.needs_fingerprint_drain() {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

pub(super) fn analysis_read_limit(
    limits: &AnalysisLimits,
    analyzed_bytes: u64,
) -> (Option<u64>, AnalysisIssue) {
    match (
        limits.max_resource_analysis_bytes,
        limits
            .max_total_analysis_bytes
            .map(|total| total.saturating_sub(analyzed_bytes)),
    ) {
        (Some(per_resource), Some(total)) if per_resource <= total => (
            Some(per_resource),
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
        ),
        (Some(_), Some(total)) | (None, Some(total)) => (
            Some(total),
            AnalysisIssue::Limit(AnalysisLimit::TotalAnalysisBytes),
        ),
        (Some(per_resource), None) => (
            Some(per_resource),
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
        ),
        (None, None) => (
            None,
            AnalysisIssue::Limit(AnalysisLimit::TotalAnalysisBytes),
        ),
    }
}
