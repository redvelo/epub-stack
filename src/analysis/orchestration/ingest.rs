use super::budget::*;
use super::resolution::probe_root_format;
use super::*;

pub(super) fn ingest_analysis_resource<R: ResourceProvider>(
    publication: &Epub<R>,
    record: ResourceRef<'_>,
    resources: &ResourceIndex,
    secondary_ncx: Option<EpubPath>,
    limits: &AnalysisLimits,
    analyzed_bytes: u64,
    fingerprint_bytes: u64,
) -> ResourceIngest {
    let classification = ResourceClassification::from_formats(semantic_formats_for(record));
    let css_candidate = css_candidate(record);
    if !record.presence().is_present() {
        let classification = classification_failure(classification, AnalysisIssue::Missing);
        return ResourceIngest {
            extractions: unavailable_extractions(
                &classification,
                css_candidate,
                secondary_ncx.clone(),
                AnalysisIssue::Missing,
            ),
            classification,
            fingerprint: AnalysisOutcome::Unavailable(AnalysisIssue::Missing),
            inspection: AnalysisOutcome::Unavailable(AnalysisIssue::Missing),
            analysis_bytes: 0,
            fingerprint_bytes: 0,
        };
    }

    let fingerprint_remaining = limits
        .max_total_fingerprint_bytes
        .map(|limit| limit.saturating_sub(fingerprint_bytes));
    let fingerprint_preflight = match (record.presence().size_bytes(), fingerprint_remaining) {
        (Some(size), Some(remaining)) if size > remaining => {
            Some(AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes))
        }
        _ => None,
    };
    let size = record.presence().size_bytes();
    let inspection_hint = inspection_hint_for(record, resources).or_else(|| {
        (css_candidate
            || matches!(
                classification,
                ResourceClassification::Identified(
                    SemanticFormat::Xhtml | SemanticFormat::Css | SemanticFormat::Smil
                )
            ))
        .then_some(MediaTypeClassification::GenericText)
    });
    let (semantic_limit, semantic_limit_issue) = analysis_read_limit(limits, analyzed_bytes);
    let semantic_preflight = size.and_then(|size| {
        if limits
            .max_resource_analysis_bytes
            .is_some_and(|limit| size > limit)
        {
            Some(AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes))
        } else if limits
            .max_total_analysis_bytes
            .is_some_and(|limit| analyzed_bytes.saturating_add(size) > limit)
        {
            Some(AnalysisIssue::Limit(AnalysisLimit::TotalAnalysisBytes))
        } else {
            None
        }
    });
    let path = record.local_path().expect("present resources are local");
    let result = publication.read_committed(path, |reader| {
        ingest_resource_reader_with_smil_limits(
            reader,
            IngestPlan {
                inspection_hint,
                css_candidate,
                classification,
                known_empty: size == Some(0),
                secondary_ncx: secondary_ncx.clone(),
                semantic_budget: StreamBudget {
                    limit: semantic_limit,
                    preflight: semantic_preflight,
                    exceeded: semantic_limit_issue,
                },
                fingerprint_budget: StreamBudget {
                    limit: fingerprint_remaining,
                    preflight: fingerprint_preflight,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
            crate::media_overlay::smil::SmilParseLimits::new(
                limits.max_smil_nodes.unwrap_or(usize::MAX),
                limits.max_smil_nesting.unwrap_or(usize::MAX),
            ),
        )
    });
    result.unwrap_or_else(|_| {
        let classification = classification_failure(
            ResourceClassification::from_formats(semantic_formats_for(record)),
            AnalysisIssue::Unreadable,
        );
        ResourceIngest {
            extractions: unavailable_extractions(
                &classification,
                css_candidate,
                secondary_ncx,
                AnalysisIssue::Unreadable,
            ),
            classification,
            fingerprint: AnalysisOutcome::Unavailable(AnalysisIssue::Unreadable),
            inspection: AnalysisOutcome::Unavailable(AnalysisIssue::Unreadable),
            analysis_bytes: 0,
            fingerprint_bytes: 0,
        }
    })
}

pub(super) fn semantic_formats_for(record: ResourceRef<'_>) -> Vec<SemanticFormat> {
    record
        .declarations()
        .filter_map(|declaration| declaration.media_type())
        .filter_map(|media_type| {
            if media_type.is_xhtml() {
                Some(SemanticFormat::Xhtml)
            } else if media_type.is_css() {
                Some(SemanticFormat::Css)
            } else if media_type.is_svg() {
                Some(SemanticFormat::Svg)
            } else if media_type.is_smil() {
                Some(SemanticFormat::Smil)
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn css_candidate(record: ResourceRef<'_>) -> bool {
    record.declarations().next().is_none()
        && record
            .local_path()
            .and_then(EpubPath::extension)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("css"))
}

fn inspection_hint_for(
    record: ResourceRef<'_>,
    resources: &ResourceIndex,
) -> Option<MediaTypeClassification> {
    if resources.package() == record {
        return Some(MediaTypeClassification::GenericText);
    }
    let mut hints = record
        .declarations()
        .filter_map(|declaration| declaration.media_type()?.classification());
    let first = hints.next()?;
    hints.all(|hint| hint == first).then_some(first)
}

pub(crate) struct ResourceIngest {
    pub(crate) classification: AnalysisOutcome<ResourceClassification>,
    pub(crate) fingerprint: AnalysisOutcome<Blake3Hash>,
    pub(crate) inspection: AnalysisOutcome<ResourceInspection>,
    pub(crate) extractions: Vec<ExtractionOutcome>,
    pub(crate) analysis_bytes: u64,
    pub(crate) fingerprint_bytes: u64,
}

pub(crate) enum ExtractionOutcome {
    Xhtml(std::result::Result<Box<XhtmlExtraction>, AnalysisIssue>),
    Css(
        std::result::Result<crate::content::extraction::css::CssExtraction, AnalysisIssue>,
        Option<AnalysisIssue>,
    ),
    Svg(
        std::result::Result<SvgExtraction, AnalysisIssue>,
        Option<AnalysisIssue>,
    ),
    Smil(std::result::Result<crate::media_overlay::smil::SmilExtraction, AnalysisIssue>),
    SecondaryNcx(std::result::Result<NavigationDocument, AnalysisIssue>),
}

pub(crate) struct SvgExtraction {
    pub(crate) facts: SvgFacts,
    pub(crate) accessibility: Vec<crate::accessibility::AccessibilityFact>,
    pub(crate) references: Vec<SvgPendingRef>,
    pub(crate) issue: Option<AnalysisIssue>,
}

#[derive(Clone, Copy)]
pub(crate) struct StreamBudget {
    pub(crate) limit: Option<u64>,
    pub(crate) preflight: Option<AnalysisIssue>,
    pub(crate) exceeded: AnalysisIssue,
}

pub(crate) struct IngestPlan {
    pub(crate) inspection_hint: Option<MediaTypeClassification>,
    pub(crate) css_candidate: bool,
    pub(crate) classification: ResourceClassification,
    pub(crate) known_empty: bool,
    pub(crate) secondary_ncx: Option<EpubPath>,
    pub(crate) semantic_budget: StreamBudget,
    pub(crate) fingerprint_budget: StreamBudget,
}

#[cfg(test)]
pub(crate) fn ingest_resource_reader(reader: &mut dyn Read, plan: IngestPlan) -> ResourceIngest {
    ingest_resource_reader_with_smil_limits(
        reader,
        plan,
        crate::media_overlay::smil::SmilParseLimits::default(),
    )
}

fn ingest_resource_reader_with_smil_limits(
    reader: &mut dyn Read,
    plan: IngestPlan,
    smil_limits: crate::media_overlay::smil::SmilParseLimits,
) -> ResourceIngest {
    const PROBE_BYTES: u64 = 64 * 1024;

    let IngestPlan {
        inspection_hint,
        css_candidate,
        classification: initial_classification,
        known_empty,
        secondary_ncx,
        semantic_budget,
        fingerprint_budget,
    } = plan;
    let requires_semantic_probe = matches!(
        initial_classification,
        ResourceClassification::Unknown | ResourceClassification::Conflict(_)
    );

    let mut reader = FingerprintingReader::new(
        reader,
        fingerprint_budget.preflight.is_none(),
        fingerprint_budget.limit,
    );
    let probe_limit = semantic_budget
        .limit
        .unwrap_or(PROBE_BYTES)
        .min(PROBE_BYTES);
    let mut prefix = Vec::new();
    let mut detected = None;
    let mut inspection_detected = None;
    let mut probe_failed = None;
    if probe_limit == 0 {
        if known_empty {
            prefix = read_prefix(&mut reader, 1);
            if reader.read_failed() {
                probe_failed = Some(AnalysisIssue::Unreadable);
            } else if !prefix.is_empty() {
                probe_failed = Some(semantic_budget.exceeded);
            }
        } else if requires_semantic_probe {
            probe_failed = Some(semantic_budget.exceeded);
        }
    } else {
        prefix = read_prefix(&mut reader, probe_limit.saturating_add(1));
        if reader.read_failed() {
            inspection_detected =
                detect_resource_format(&prefix[..prefix.len().min(probe_limit as usize)]);
            if requires_semantic_probe {
                probe_failed = Some(AnalysisIssue::Unreadable);
            }
        } else {
            let probe_truncated = prefix.len() as u64 > probe_limit;
            (detected, inspection_detected) =
                probe_root_format(&prefix[..prefix.len().min(probe_limit as usize)]);
            if requires_semantic_probe
                && detected.is_none()
                && probe_truncated
                && probe_limit < PROBE_BYTES
            {
                probe_failed = Some(semantic_budget.exceeded);
            }
        }
    }

    let classification_value = detected.map_or_else(
        || initial_classification.clone(),
        ResourceClassification::Identified,
    );
    let classification = probe_failed.map_or_else(
        || AnalysisOutcome::Complete(classification_value.clone()),
        |issue| classification_failure(initial_classification.clone(), issue),
    );

    let selected_format = match &classification_value {
        ResourceClassification::Identified(format) => Some(*format),
        ResourceClassification::Unknown | ResourceClassification::Conflict(_)
            if css_candidate && inspection_detected.is_none() =>
        {
            Some(SemanticFormat::Css)
        }
        ResourceClassification::Unknown | ResourceClassification::Conflict(_) => None,
    };
    let full_inspection_capture = match inspection_detected.as_ref() {
        Some(detection) => matches!(
            detection.format(),
            DetectedFormat::Font(_) | DetectedFormat::WebVtt | DetectedFormat::Media(_)
        ),
        None => matches!(
            inspection_hint,
            Some(
                MediaTypeClassification::Font(_)
                    | MediaTypeClassification::WebVtt
                    | MediaTypeClassification::Media(_)
            )
        ),
    };
    let prefix_complete = reader.eof() && !reader.read_failed();
    let mut extractions = Vec::new();
    let mut inspection_bytes = Vec::new();
    let mut shared_svg_scan = None;
    let mut shared_svg_complete = false;
    let mut semantic_bytes = 0u64;
    let mut semantic_crossed = false;
    let mut semantic_failed = reader.read_failed();
    let mut captured_inspection_complete = false;
    let zero_limit_blocked = semantic_budget.limit == Some(0)
        && !(known_empty && prefix.is_empty() && reader.eof() && !reader.read_failed());
    let parse_blocker = semantic_budget
        .preflight
        .or_else(|| zero_limit_blocked.then_some(semantic_budget.exceeded))
        .or_else(|| reader.read_failed().then_some(AnalysisIssue::Unreadable));
    let can_parse = parse_blocker.is_none();
    if can_parse {
        let replay_for_secondary_ncx = secondary_ncx.is_some() && selected_format.is_some();
        let input = Cursor::new(prefix.as_slice()).chain(&mut reader);
        let mut semantic = SemanticReader::new(
            input,
            semantic_budget.limit,
            replay_for_secondary_ncx || full_inspection_capture,
        );
        match selected_format {
            Some(SemanticFormat::Xhtml) => {
                let mut decoded = XmlUtf8Reader::new(BufReader::new(&mut semantic));
                let result = parse_xhtml_document_from_reader_counted(&mut decoded)
                    .map(|(facts, _)| Box::new(facts))
                    .map_err(|_| AnalysisIssue::Malformed);
                extractions.push(ExtractionOutcome::Xhtml(result));
            }
            Some(SemanticFormat::Smil) => {
                let result = crate::media_overlay::smil::extract_smil_facts_from_reader(
                    BufReader::new(&mut semantic),
                    smil_limits,
                )
                .map_err(smil_analysis_issue);
                extractions.push(ExtractionOutcome::Smil(result));
            }
            Some(SemanticFormat::Css) => {
                let mut bytes = Vec::new();
                let result = semantic
                    .read_to_end(&mut bytes)
                    .map_err(|_| AnalysisIssue::Unreadable)
                    .and_then(|_| crate::content::extraction::css::extract(&bytes));
                extractions.push(ExtractionOutcome::Css(result, None));
            }
            Some(SemanticFormat::Svg) => {
                let mut bytes = Vec::new();
                let result = semantic
                    .read_to_end(&mut bytes)
                    .map_err(|_| AnalysisIssue::Unreadable)
                    .map(|_| {
                        let mut scan = scan_svg(&bytes);
                        let issue = (scan.is_malformed() || !scan.is_svg() || !scan.root_closed())
                            .then_some(AnalysisIssue::Malformed)
                            .or_else(|| scan.semantic_issue());
                        let (facts, accessibility, references) = scan.take_analysis();
                        let extraction = SvgExtraction {
                            facts,
                            accessibility,
                            references,
                            issue,
                        };
                        shared_svg_scan = Some(scan);
                        extraction
                    });
                extractions.push(ExtractionOutcome::Svg(result, None));
            }
            _ if secondary_ncx.is_some() => {
                let result = parse::ncx_reader(
                    secondary_ncx.clone().expect("secondary NCX path exists"),
                    BufReader::new(&mut semantic),
                )
                .map_err(|_| AnalysisIssue::Malformed);
                extractions.push(ExtractionOutcome::SecondaryNcx(result));
            }
            None => {}
        }
        if replay_for_secondary_ncx || full_inspection_capture {
            semantic.drain_to_boundary();
        }
        semantic.check_crossing();
        semantic_bytes = semantic.bytes_read();
        semantic_crossed = semantic.crossed_limit();
        let replay = semantic.into_captured();
        semantic_failed = reader.read_failed();
        shared_svg_complete = shared_svg_scan.as_ref().is_some_and(|scan| scan.is_svg())
            && !semantic_crossed
            && !semantic_failed
            && reader.eof();
        if replay_for_secondary_ncx {
            let result = parse::ncx_reader(
                secondary_ncx.expect("secondary NCX path exists"),
                BufReader::new(Cursor::new(replay.as_slice())),
            )
            .map_err(|_| AnalysisIssue::Malformed);
            extractions.push(ExtractionOutcome::SecondaryNcx(result));
        }
        if full_inspection_capture {
            inspection_bytes = replay;
            captured_inspection_complete = !semantic_crossed && !semantic_failed && reader.eof();
        }
        if semantic_crossed {
            fail_extractions(&mut extractions, semantic_budget.exceeded);
        } else if semantic_failed {
            fail_extractions(&mut extractions, AnalysisIssue::Unreadable);
        }
    } else {
        extractions = unavailable_extractions(
            &classification,
            css_candidate,
            secondary_ncx,
            parse_blocker
                .or(probe_failed)
                .unwrap_or(AnalysisIssue::Malformed),
        );
    }
    if inspection_bytes.is_empty() {
        inspection_bytes.extend_from_slice(&prefix[..prefix.len().min(probe_limit as usize)]);
    }
    let inspection_bytes_complete = if shared_svg_complete {
        true
    } else if full_inspection_capture {
        captured_inspection_complete || prefix_complete
    } else {
        prefix_complete
    };
    let probe_crossed_limit = semantic_budget
        .limit
        .is_some_and(|limit| probe_limit == limit && prefix.len() as u64 > probe_limit);

    let inspection = if let Some(issue) = semantic_budget.preflight.filter(|_| prefix.is_empty()) {
        AnalysisOutcome::Unavailable(issue)
    } else if prefix.is_empty() && reader.read_failed() {
        AnalysisOutcome::Unavailable(AnalysisIssue::Unreadable)
    } else {
        if let Some(scan) = shared_svg_scan.filter(|scan| scan.is_svg()) {
            inspection_detected = Some(Detection::svg(scan));
        }
        let inspected = inspect_resource(
            &inspection_bytes,
            inspection_hint,
            inspection_detected,
            inspection_bytes_complete,
        );
        let inspection_issue = semantic_budget
            .preflight
            .or_else(|| probe_crossed_limit.then_some(semantic_budget.exceeded))
            .or_else(|| semantic_crossed.then_some(semantic_budget.exceeded))
            .or_else(|| semantic_failed.then_some(AnalysisIssue::Unreadable))
            .or(inspected.issue);
        match (inspected.facts, inspection_issue) {
            (Some(value), Some(issue)) => AnalysisOutcome::Partial { value, issue },
            (Some(value), None) => AnalysisOutcome::Complete(value),
            (None, issue) => {
                AnalysisOutcome::Unavailable(issue.unwrap_or(AnalysisIssue::Unreadable))
            }
        }
    };

    if reader.needs_fingerprint_drain() {
        drain_for_fingerprint(&mut reader);
    }

    let probe_bytes = (prefix.len() as u64).min(probe_limit);
    let analysis_bytes = semantic_bytes.max(probe_bytes);
    let (fingerprint, fingerprint_bytes) = if let Some(issue) = fingerprint_budget.preflight {
        (AnalysisOutcome::Unavailable(issue), 0)
    } else {
        reader.finish(fingerprint_budget.exceeded)
    };
    ResourceIngest {
        classification,
        fingerprint,
        inspection,
        extractions,
        analysis_bytes,
        fingerprint_bytes,
    }
}

fn unavailable_extractions(
    classification: &AnalysisOutcome<ResourceClassification>,
    css_candidate: bool,
    secondary_ncx: Option<EpubPath>,
    issue: AnalysisIssue,
) -> Vec<ExtractionOutcome> {
    let mut extractions = match classification.value() {
        Some(ResourceClassification::Identified(SemanticFormat::Xhtml)) => {
            vec![ExtractionOutcome::Xhtml(Err(issue))]
        }
        Some(ResourceClassification::Identified(SemanticFormat::Smil)) => {
            vec![ExtractionOutcome::Smil(Err(issue))]
        }
        Some(ResourceClassification::Identified(SemanticFormat::Css)) => {
            vec![ExtractionOutcome::Css(Err(issue), None)]
        }
        Some(ResourceClassification::Identified(SemanticFormat::Svg)) => {
            vec![ExtractionOutcome::Svg(Err(issue), None)]
        }
        _ if css_candidate => vec![ExtractionOutcome::Css(Err(issue), None)],
        _ => Vec::new(),
    };
    if secondary_ncx.is_some() {
        extractions.push(ExtractionOutcome::SecondaryNcx(Err(issue)));
    }
    extractions
}

fn fail_extractions(extractions: &mut [ExtractionOutcome], issue: AnalysisIssue) {
    for extraction in extractions {
        match extraction {
            ExtractionOutcome::Xhtml(result) => *result = Err(issue),
            ExtractionOutcome::Css(result, partial_issue) => {
                if result.is_ok() {
                    *partial_issue = Some(issue);
                } else {
                    *result = Err(issue);
                }
            }
            ExtractionOutcome::Svg(result, partial_issue) => {
                if result.is_ok() {
                    *partial_issue = Some(issue);
                } else {
                    *result = Err(issue);
                }
            }
            ExtractionOutcome::Smil(result) => *result = Err(issue),
            ExtractionOutcome::SecondaryNcx(result) => *result = Err(issue),
        }
    }
}

fn smil_analysis_issue(error: SmilError) -> AnalysisIssue {
    match error {
        SmilError::Io { source } if source.kind() == std::io::ErrorKind::InvalidData => {
            AnalysisIssue::Malformed
        }
        SmilError::Io { .. } => AnalysisIssue::Unreadable,
        SmilError::NodeLimitExceeded { .. } => AnalysisIssue::Limit(AnalysisLimit::SmilNodes),
        SmilError::NestingLimitExceeded { .. } => AnalysisIssue::Limit(AnalysisLimit::SmilNesting),
        _ => AnalysisIssue::Malformed,
    }
}

pub(super) fn classification_failure(
    classification: ResourceClassification,
    issue: AnalysisIssue,
) -> AnalysisOutcome<ResourceClassification> {
    match classification {
        ResourceClassification::Identified(_) => AnalysisOutcome::Complete(classification),
        ResourceClassification::Conflict(_) => AnalysisOutcome::Partial {
            value: classification,
            issue,
        },
        ResourceClassification::Unknown => AnalysisOutcome::Unavailable(issue),
    }
}
