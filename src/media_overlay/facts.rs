use crate::analysis::reference::ReferenceSlot;

/// A parsed media-clock value represented at millisecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaTime {
    milliseconds: u64,
}

impl MediaTime {
    pub(crate) fn new(milliseconds: u64) -> Self {
        Self { milliseconds }
    }

    /// Returns the nonnegative media time in whole milliseconds.
    pub fn milliseconds(self) -> u64 {
        self.milliseconds
    }

    /// Returns the media time in seconds.
    pub fn seconds(self) -> f64 {
        self.milliseconds as f64 / 1000.0
    }
}

/// An authored media clock and its recognized millisecond value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmilTime {
    authored: String,
    parsed: Option<MediaTime>,
}

impl SmilTime {
    pub(crate) fn new(authored: impl Into<String>) -> Self {
        let authored = authored.into();
        let parsed = parse_media_time(authored.trim());
        Self { authored, parsed }
    }

    /// Returns the exact authored clock text.
    pub fn authored(&self) -> &str {
        &self.authored
    }

    /// Returns the recognized millisecond value, or `None` for unsupported clock syntax.
    pub fn parsed(&self) -> Option<MediaTime> {
        self.parsed
    }
}

/// Identifies a playback node within one [`SmilFacts`] value.
///
/// IDs carry no owner identity and must not be mixed between [`SmilFacts`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmilNodeId(usize);

impl SmilNodeId {
    pub(crate) fn new(slot: usize) -> Self {
        Self(slot)
    }

    pub(crate) fn slot(self) -> usize {
        self.0
    }
}

/// One extracted SMIL sequence, parallel group, text target, or audio clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmilNodeFact {
    /// A SMIL `seq` node with ordered child IDs and authored `epub:type` tokens.
    Sequence {
        /// Child node IDs in authored order.
        children: Vec<SmilNodeId>,
        /// Whitespace-separated `epub:type` tokens in authored order.
        epub_types: Vec<String>,
    },
    /// A SMIL `par` node with ordered child IDs and authored `epub:type` tokens.
    Parallel {
        /// Child node IDs in authored order.
        children: Vec<SmilNodeId>,
        /// Whitespace-separated `epub:type` tokens in authored order.
        epub_types: Vec<String>,
    },
    /// A SMIL `text` node with authored `epub:type` tokens.
    ///
    /// Use [`crate::media_overlay::SmilNodeRef::text_reference`] to retrieve its authored target.
    Text {
        /// Whitespace-separated `epub:type` tokens in authored order.
        epub_types: Vec<String>,
    },
    /// A SMIL `audio` node with optional authored clip times and `epub:type` tokens.
    ///
    /// A present [`SmilTime`] whose [`SmilTime::parsed`] is `None` distinguishes an unrecognized
    /// authored clock from an absent clip attribute. Use
    /// [`crate::media_overlay::SmilNodeRef::audio_reference`] to retrieve its authored target.
    Audio {
        /// The optional parsed or unrecognized `clipBegin` value.
        clip_begin: Option<SmilTime>,
        /// The optional parsed or unrecognized `clipEnd` value.
        clip_end: Option<SmilTime>,
        /// Whitespace-separated `epub:type` tokens in authored order.
        epub_types: Vec<String>,
    },
}

impl SmilNodeFact {
    /// Returns ordered child IDs for sequence and parallel nodes.
    ///
    /// Text and audio nodes return an empty slice.
    pub fn children(&self) -> &[SmilNodeId] {
        match self {
            Self::Sequence { children, .. } | Self::Parallel { children, .. } => children,
            Self::Text { .. } | Self::Audio { .. } => &[],
        }
    }

    /// Returns authored `epub:type` tokens in source order.
    pub fn epub_types(&self) -> &[String] {
        match self {
            Self::Sequence { epub_types, .. }
            | Self::Parallel { epub_types, .. }
            | Self::Text { epub_types, .. }
            | Self::Audio { epub_types, .. } => epub_types,
        }
    }

    /// Returns the authored clip-begin result for an audio node.
    ///
    /// `None` means either that this is not an audio node or that no clip-begin value was
    /// authored; a [`SmilTime`] without a parsed value records a present but unrecognized clock.
    pub fn clip_begin(&self) -> Option<&SmilTime> {
        match self {
            Self::Audio { clip_begin, .. } => clip_begin.as_ref(),
            _ => None,
        }
    }

    /// Returns the authored clip-end result for an audio node.
    ///
    /// `None` means either that this is not an audio node or that no clip-end value was
    /// authored; a [`SmilTime`] without a parsed value records a present but unrecognized clock.
    pub fn clip_end(&self) -> Option<&SmilTime> {
        match self {
            Self::Audio { clip_end, .. } => clip_end.as_ref(),
            _ => None,
        }
    }
}

/// Playback hierarchy, clip times, and skippable or escapable semantics from one SMIL document.
///
/// Use [`crate::media_overlay::SmilNodeRef`] to access resolved text and audio links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmilFacts {
    roots: Vec<SmilNodeId>,
    nodes: Vec<SmilNodeFact>,
    text_references: Vec<Option<ReferenceSlot>>,
    audio_references: Vec<Option<ReferenceSlot>>,
    skippable: Vec<String>,
    escapable: Vec<String>,
}

impl SmilFacts {
    pub(crate) fn new(
        roots: Vec<SmilNodeId>,
        nodes: Vec<SmilNodeFact>,
        skippable: Vec<String>,
        escapable: Vec<String>,
    ) -> Self {
        Self {
            text_references: vec![None; nodes.len()],
            audio_references: vec![None; nodes.len()],
            roots,
            nodes,
            skippable,
            escapable,
        }
    }

    /// Returns root node IDs in source order.
    pub fn roots(&self) -> &[SmilNodeId] {
        &self.roots
    }

    /// Returns all playback nodes in ID order.
    pub fn nodes(&self) -> &[SmilNodeFact] {
        &self.nodes
    }

    /// Returns the node identified by `id` when it exists in this value.
    ///
    /// `id` must come from this [`SmilFacts`] value.
    pub fn node(&self, id: SmilNodeId) -> Option<&SmilNodeFact> {
        self.nodes.get(id.slot())
    }

    /// Returns skippable `epub:type` tokens from SMIL head metadata in authored order.
    pub fn skippable(&self) -> &[String] {
        &self.skippable
    }

    /// Returns escapable `epub:type` tokens from SMIL head metadata in authored order.
    pub fn escapable(&self) -> &[String] {
        &self.escapable
    }

    pub(crate) fn set_text_reference(&mut self, node: SmilNodeId, reference: ReferenceSlot) {
        self.text_references[node.slot()] = Some(reference);
    }

    pub(crate) fn set_audio_reference(&mut self, node: SmilNodeId, reference: ReferenceSlot) {
        self.audio_references[node.slot()] = Some(reference);
    }

    pub(crate) fn text_reference_slot(&self, node: SmilNodeId) -> Option<ReferenceSlot> {
        self.text_references.get(node.slot()).copied().flatten()
    }

    pub(crate) fn audio_reference_slot(&self, node: SmilNodeId) -> Option<ReferenceSlot> {
        self.audio_references.get(node.slot()).copied().flatten()
    }
}

pub(crate) fn parse_media_time(value: &str) -> Option<MediaTime> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('P') {
        return None;
    }
    if value.contains(':') {
        return parse_colon_clock(value).map(MediaTime::new);
    }
    parse_timecount(value).map(MediaTime::new)
}

fn parse_colon_clock(value: &str) -> Option<u64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let seconds_source = parts[parts.len() - 1];
    let seconds_whole = seconds_source
        .split_once('.')
        .map_or(seconds_source, |(whole, _)| whole);
    let minutes_source = parts[parts.len() - 2];
    if seconds_whole.len() != 2 || minutes_source.len() != 2 {
        return None;
    }
    let seconds = parse_decimal_thousandths(seconds_source)?;
    let minutes = minutes_source.parse::<u64>().ok()?;
    if minutes >= 60 || seconds >= 60_000 {
        return None;
    }
    let hours = if parts.len() == 3 {
        parts[0].parse::<u64>().ok()?
    } else {
        0
    };
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds)
}

fn parse_timecount(value: &str) -> Option<u64> {
    let (value, multiplier) = if let Some(value) = value.strip_suffix("min") {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix("ms") {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else {
        (value, 1_000)
    };
    parse_decimal_thousandths(value)?
        .checked_mul(multiplier)?
        .checked_div(1_000)
}

fn parse_decimal_thousandths(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || value.ends_with('.')
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut fraction = fraction.chars().take(3).collect::<String>();
    while fraction.len() < 3 {
        fraction.push('0');
    }
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000)?
        .checked_add(fraction.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_time_retains_supported_clock_values() {
        assert_eq!(parse_media_time("1.25s").unwrap().milliseconds(), 1_250);
        assert_eq!(parse_media_time("01:02.5").unwrap().milliseconds(), 62_500);
        assert!(parse_media_time("PT1S").is_none());
        assert!(parse_media_time("1:02").is_none());
        assert!(parse_media_time("01:2").is_none());
        assert_eq!(
            parse_media_time("1:02:03").unwrap().milliseconds(),
            3_723_000
        );
        assert_eq!(
            parse_media_time("123:02:03").unwrap().milliseconds(),
            442_923_000
        );
        assert!(parse_media_time("1.").is_none());
    }
}
