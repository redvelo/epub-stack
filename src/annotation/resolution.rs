use crate::analysis::PublicationAnalysis;
use crate::content::text::{TextRange, TextRangeError};
use crate::resource::{
    ProviderPresence, ResolvedHref, ResourceAddress, ResourceIndex, ResourceRef,
};

use super::{AnnotationSelector, AnnotationTarget, FragmentConformsTo, FragmentSelector};

/// Reports whether an annotation target source is available in a publication inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationSourceState {
    /// A present manifest resource was found.
    Resolved(
        /// Resolved publication address.
        ResourceAddress,
    ),
    /// No matching manifest resource exists, or the authored source is empty.
    Missing,
    /// The manifest resource exists but is absent from the provider inventory.
    ProviderMissing(
        /// Resolved publication address.
        ResourceAddress,
    ),
    /// The source address kind is not supported for annotation resolution.
    Unsupported,
}

/// Identifies the browser capability needed to finish resolving a selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRequirement {
    /// DOM traversal or selector matching.
    Dom,
    /// Rendered-text matching is a browser capability and therefore implies DOM access.
    RenderedText,
}

/// Reports the result of resolving a target source or one selector alternative.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationResolution {
    /// The target has no matching manifest source.
    MissingSource,
    /// The manifest source is absent from the provider snapshot.
    ProviderMissing {
        /// Resolved publication address.
        source: ResourceAddress,
    },
    /// The target uses an unsupported source address kind.
    UnsupportedSource,
    /// A selectorless target resolved to its resource.
    Resource {
        /// Resolved publication address.
        source: ResourceAddress,
    },
    /// A fragment selected exactly one element.
    Fragment {
        /// Resolved publication address.
        source: ResourceAddress,
        /// Percent-decoded fragment identifier.
        value: String,
    },
    /// A text-position selector selected source text.
    Text {
        /// Resolved publication address.
        source: ResourceAddress,
        /// Selected normalized source-text snapshot.
        text: String,
    },
    /// A fragment identifier matched multiple elements.
    Ambiguous {
        /// Resolved publication address.
        source: ResourceAddress,
        /// Number of distinct matching elements.
        matches: usize,
    },
    /// The selector was valid but did not identify existing content.
    Broken {
        /// Resolved publication address.
        source: ResourceAddress,
    },
    /// Content analysis for the source was unavailable.
    Unavailable {
        /// Resolved publication address.
        source: ResourceAddress,
    },
    /// Resolution requires browser-host layout or DOM behavior.
    HostRequired {
        /// Resolved publication address.
        source: ResourceAddress,
        /// Selector to resolve in the host.
        selector: AnnotationSelector,
        /// Minimum host capability required.
        requirement: HostRequirement,
    },
    /// A selector lacked required values or contradicted its own invariants.
    InvalidSelector {
        /// Resolved publication address.
        source: ResourceAddress,
    },
    /// A well-shaped selector or conformance is not supported for this source.
    UnsupportedSelector {
        /// Resolved publication address.
        source: ResourceAddress,
    },
}

impl AnnotationResolution {
    /// Returns the resolved publication resource, when resolution reached one.
    pub fn source(&self) -> Option<&ResourceAddress> {
        match self {
            Self::MissingSource | Self::UnsupportedSource => None,
            Self::ProviderMissing { source }
            | Self::Resource { source }
            | Self::Fragment { source, .. }
            | Self::Text { source, .. }
            | Self::Ambiguous { source, .. }
            | Self::Broken { source }
            | Self::Unavailable { source }
            | Self::HostRequired { source, .. }
            | Self::InvalidSelector { source }
            | Self::UnsupportedSelector { source } => Some(source),
        }
    }

    /// Returns resolved text when this result selected a text range.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

impl AnnotationTarget {
    /// Checks whether the target source is present in a resource inventory.
    ///
    /// Use this for inventory checks, including edit previews. It does not read content or resolve
    /// selectors; use [`PublicationAnalysis::resolve_annotation_target`] for those tasks.
    pub fn source_state(&self, resources: &ResourceIndex) -> AnnotationSourceState {
        resolve_target_source(resources, self.source())
            .map(|record| AnnotationSourceState::Resolved(record.address().clone()))
            .unwrap_or_else(|state| state)
    }
}

impl PublicationAnalysis {
    /// Resolves a target against analyzed publication content.
    ///
    /// Source failures produce one result, and a selectorless target produces one `Resource`
    /// result. Otherwise, results follow authored selector order. Resolution uses this analysis
    /// snapshot; missing analysis and browser-only work produce `Unavailable` and `HostRequired`.
    pub fn resolve_annotation_target(
        &self,
        target: &AnnotationTarget,
    ) -> Vec<AnnotationResolution> {
        let source = match target.source_state(self.resources()) {
            AnnotationSourceState::Resolved(source) => source,
            AnnotationSourceState::Missing => return vec![AnnotationResolution::MissingSource],
            AnnotationSourceState::ProviderMissing(source) => {
                return vec![AnnotationResolution::ProviderMissing { source }];
            }
            AnnotationSourceState::Unsupported => {
                return vec![AnnotationResolution::UnsupportedSource];
            }
        };
        let Some(source_record) = self.resources().resources_at(&source).next() else {
            return vec![AnnotationResolution::Broken { source }];
        };
        if target.selectors().is_empty() {
            return vec![AnnotationResolution::Resource { source }];
        }
        target
            .selectors()
            .iter()
            .map(|selector| self.resolve_annotation_selector(source_record, selector))
            .collect()
    }

    fn resolve_annotation_selector(
        &self,
        source_record: ResourceRef<'_>,
        selector: &AnnotationSelector,
    ) -> AnnotationResolution {
        let source = source_record.address().clone();
        let requirement = match selector_requirement(selector, source_record) {
            Ok(requirement) => requirement,
            Err(SelectorValidationError::Invalid) => {
                return AnnotationResolution::InvalidSelector { source };
            }
            Err(SelectorValidationError::Unsupported) => {
                return AnnotationResolution::UnsupportedSelector { source };
            }
        };
        match selector {
            AnnotationSelector::Fragment(fragment)
                if fragment.refined_by().is_empty()
                    && !matches!(
                        fragment.conforms_to(),
                        Some(FragmentConformsTo::TextFragment)
                    ) =>
            {
                self.resolve_fragment(source_record, fragment.value())
            }
            AnnotationSelector::TextPosition(position) if position.refined_by().is_empty() => {
                let Some(range) = position
                    .start()
                    .zip(position.end())
                    .and_then(|(start, end)| TextRange::new(start, end))
                else {
                    return AnnotationResolution::InvalidSelector { source };
                };
                let Ok(Some(stream)) = self.text_stream_for_row(source_record.key()) else {
                    return AnnotationResolution::Unavailable { source };
                };
                match stream.text_for_range(range) {
                    Ok(text) => AnnotationResolution::Text {
                        source,
                        text: text.to_string(),
                    },
                    Err(TextRangeError::OutOfBounds) => AnnotationResolution::Broken { source },
                }
            }
            _ => AnnotationResolution::HostRequired {
                source,
                selector: selector.clone(),
                requirement,
            },
        }
    }

    fn resolve_fragment(
        &self,
        source_record: ResourceRef<'_>,
        value: &str,
    ) -> AnnotationResolution {
        let source = source_record.address().clone();
        let Ok(Some(facts)) = self.content_for_row(source_record.key()).map(|outcome| {
            outcome
                .value()
                .and_then(crate::content::ContentFacts::as_xhtml)
        }) else {
            return AnnotationResolution::Unavailable { source };
        };
        let fragment = percent_encoding::percent_decode_str(value)
            .decode_utf8()
            .ok()
            .map(|value| value.into_owned())
            .unwrap_or_else(|| value.to_string());
        let matches = facts
            .fragments()
            .iter()
            .filter(|fact| fact.id() == fragment)
            .map(crate::content::FragmentFact::element_ordinal)
            .collect::<std::collections::HashSet<_>>()
            .len();
        match matches {
            0 => AnnotationResolution::Broken { source },
            1 => AnnotationResolution::Fragment {
                source,
                value: fragment,
            },
            matches => AnnotationResolution::Ambiguous { source, matches },
        }
    }
}

fn resolve_target_source<'a>(
    resources: &'a ResourceIndex,
    source: &str,
) -> Result<ResourceRef<'a>, AnnotationSourceState> {
    if source.is_empty() {
        return Err(AnnotationSourceState::Missing);
    }
    let address = match resources.resolve_manifest_href(source) {
        ResolvedHref::Resource(ResourceAddress::Local(address)) => ResourceAddress::Local(address),
        ResolvedHref::MissingPath(_) | ResolvedHref::MissingManifestId(_) => {
            return Err(AnnotationSourceState::Missing);
        }
        _ => return Err(AnnotationSourceState::Unsupported),
    };
    let Some(record) = resources.resources_at(&address).next() else {
        return Err(AnnotationSourceState::Missing);
    };
    if !record.is_manifest_resource() {
        return Err(AnnotationSourceState::Missing);
    }
    if record.presence() != ProviderPresence::Present {
        return Err(AnnotationSourceState::ProviderMissing(address));
    }
    Ok(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorValidationError {
    Invalid,
    Unsupported,
}

fn selector_requirement(
    selector: &AnnotationSelector,
    source: ResourceRef<'_>,
) -> Result<HostRequirement, SelectorValidationError> {
    if matches!(
        selector,
        AnnotationSelector::Fragment(fragment)
            if matches!(fragment.conforms_to(), Some(FragmentConformsTo::Unknown(_)))
    ) {
        return Err(SelectorValidationError::Unsupported);
    }
    let own = match selector {
        AnnotationSelector::Fragment(selector) => fragment_requirement(selector, source)?,
        AnnotationSelector::Css(selector) => {
            if selector.value().trim().is_empty() {
                return Err(SelectorValidationError::Invalid);
            }
            if !(source.has_xhtml_declaration() || source.has_svg_declaration()) {
                return Err(SelectorValidationError::Unsupported);
            }
            HostRequirement::Dom
        }
        AnnotationSelector::TextPosition(selector) => {
            let (Some(start), Some(end)) = (selector.start(), selector.end()) else {
                return Err(SelectorValidationError::Invalid);
            };
            if start >= end {
                return Err(SelectorValidationError::Invalid);
            }
            if !source.has_xhtml_declaration() {
                return Err(SelectorValidationError::Unsupported);
            }
            HostRequirement::Dom
        }
        AnnotationSelector::Unknown(selector)
            if selector
                .selector_type()
                .and_then(serde_json::Value::as_str)
                .is_none() =>
        {
            return Err(SelectorValidationError::Invalid);
        }
        AnnotationSelector::Unknown(_) => return Err(SelectorValidationError::Unsupported),
    };
    let refinements = selector_refinements(selector);
    if refinements.is_empty() {
        return Ok(own);
    }
    let mut best = None;
    let mut invalid = false;
    for refinement in refinements {
        if matches!(refinement, AnnotationSelector::Unknown(_)) {
            invalid = true;
            continue;
        }
        match selector_requirement(refinement, source) {
            Ok(requirement) => {
                let branch = greater_requirement(own, requirement);
                best = Some(match best {
                    Some(current) => lesser_requirement(current, branch),
                    None => branch,
                });
            }
            Err(SelectorValidationError::Invalid) => invalid = true,
            Err(SelectorValidationError::Unsupported) => {}
        }
    }
    if invalid {
        Err(SelectorValidationError::Invalid)
    } else {
        best.ok_or(SelectorValidationError::Unsupported)
    }
}

fn fragment_requirement(
    selector: &FragmentSelector,
    source: ResourceRef<'_>,
) -> Result<HostRequirement, SelectorValidationError> {
    if selector.value().trim().is_empty() {
        return Err(SelectorValidationError::Invalid);
    }
    match selector.conforms_to() {
        None | Some(FragmentConformsTo::Html) if source.has_xhtml_declaration() => {
            Ok(HostRequirement::Dom)
        }
        Some(FragmentConformsTo::TextFragment) if source.has_xhtml_declaration() => {
            Ok(HostRequirement::RenderedText)
        }
        _ => Err(SelectorValidationError::Unsupported),
    }
}

fn selector_refinements(selector: &AnnotationSelector) -> &[AnnotationSelector] {
    match selector {
        AnnotationSelector::Fragment(selector) => selector.refined_by(),
        AnnotationSelector::Css(selector) => selector.refined_by(),
        AnnotationSelector::TextPosition(selector) => selector.refined_by(),
        AnnotationSelector::Unknown(_) => &[],
    }
}

fn greater_requirement(left: HostRequirement, right: HostRequirement) -> HostRequirement {
    if left == HostRequirement::RenderedText || right == HostRequirement::RenderedText {
        HostRequirement::RenderedText
    } else {
        HostRequirement::Dom
    }
}

fn lesser_requirement(left: HostRequirement, right: HostRequirement) -> HostRequirement {
    if left == HostRequirement::Dom || right == HostRequirement::Dom {
        HostRequirement::Dom
    } else {
        HostRequirement::RenderedText
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::annotation::AnnotationTarget;
    use crate::publication::Epub;
    use crate::resource::provider::MemoryResourceProvider;

    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>Annotations</dc:title><dc:identifier id="uid">urn:test</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="missing" href="missing.xhtml" media-type="application/xhtml+xml"/><item id="broken" href="broken.xhtml" media-type="application/xhtml+xml"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
    const CHAPTER: &[u8] = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p id="one">Hello world</p><i id="duplicate"/><b id="duplicate"/></body></html>"#;

    fn epub() -> Epub<MemoryResourceProvider> {
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", PACKAGE.to_vec()),
            ("EPUB/chapter.xhtml", CHAPTER.to_vec()),
            ("EPUB/broken.xhtml", vec![0xff]),
            ("EPUB/image.png", vec![0]),
        ])
        .unwrap();
        Epub::from_provider(provider, "EPUB/package.opf").unwrap()
    }

    fn target(source: &str, selectors: Value) -> AnnotationTarget {
        AnnotationTarget::from_value(json!({"source":source,"selector":selectors})).unwrap()
    }

    fn resolve(source: &str, selectors: Value) -> Vec<AnnotationResolution> {
        epub()
            .analyze()
            .resolve_annotation_target(&target(source, selectors))
    }

    #[test]
    fn source_state_distinguishes_inventory_outcomes() {
        let epub = epub();
        for (source, expected) in [
            ("unknown.xhtml", "missing"),
            ("missing.xhtml", "provider-missing"),
            ("https://example.com/chapter.xhtml", "unsupported"),
            ("chapter.xhtml", "resolved"),
        ] {
            let state = target(source, json!([])).source_state(epub.resources());
            assert!(
                matches!(
                    (&state, expected),
                    (AnnotationSourceState::Missing, "missing")
                        | (
                            AnnotationSourceState::ProviderMissing(_),
                            "provider-missing"
                        )
                        | (AnnotationSourceState::Unsupported, "unsupported")
                        | (AnnotationSourceState::Resolved(_), "resolved")
                ),
                "{source}: {state:?}"
            );
        }
    }

    #[test]
    fn source_failures_and_selectorless_targets_project_consistently() {
        assert!(matches!(
            resolve("unknown.xhtml", json!([])).as_slice(),
            [AnnotationResolution::MissingSource]
        ));
        assert!(matches!(
            resolve("missing.xhtml", json!([])).as_slice(),
            [AnnotationResolution::ProviderMissing { .. }]
        ));
        assert!(matches!(
            resolve("https://example.com/chapter.xhtml", json!([])).as_slice(),
            [AnnotationResolution::UnsupportedSource]
        ));

        let resolution = resolve("chapter.xhtml", json!([])).pop().unwrap();
        assert!(matches!(resolution, AnnotationResolution::Resource { .. }));
        assert!(resolution.source().is_some());
        assert_eq!(resolution.text(), None);
        assert_eq!(AnnotationResolution::MissingSource.source(), None);
    }

    #[test]
    fn invalid_and_unsupported_selectors_are_distinct() {
        let results = resolve(
            "chapter.xhtml",
            json!([
                {"type":"CssSelector","value":""},
                {"type":"FutureSelector","value":"x"}
            ]),
        );
        assert!(matches!(
            results[0],
            AnnotationResolution::InvalidSelector { .. }
        ));
        assert!(matches!(
            results[1],
            AnnotationResolution::UnsupportedSelector { .. }
        ));

        assert!(matches!(
            resolve("image.png", json!([{"type":"CssSelector","value":"body"}]))[0],
            AnnotationResolution::UnsupportedSelector { .. }
        ));
    }

    #[test]
    fn fragment_resolution_reports_broken_ambiguous_and_unavailable_content() {
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"absent"}])
            )[0],
            AnnotationResolution::Broken { .. }
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"duplicate"}])
            )[0],
            AnnotationResolution::Ambiguous { matches: 2, .. }
        ));
        assert!(matches!(
            resolve(
                "broken.xhtml",
                json!([{"type":"FragmentSelector","value":"one"}])
            )[0],
            AnnotationResolution::Unavailable { .. }
        ));
    }

    #[test]
    fn text_ranges_project_text_and_report_out_of_bounds() {
        let selected = resolve(
            "chapter.xhtml",
            json!([{"type":"TextPositionSelector","start":0,"end":5}]),
        )
        .pop()
        .unwrap();
        assert_eq!(selected.text(), Some("Hello"));
        assert!(selected.source().is_some());

        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"TextPositionSelector","start":0,"end":500}])
            )[0],
            AnnotationResolution::Broken { .. }
        ));
    }

    #[test]
    fn host_requirements_include_refinements_and_preserve_selector_order() {
        let results = resolve(
            "chapter.xhtml",
            json!([
                {"type":"FragmentSelector","value":"one"},
                {"type":"TextPositionSelector","start":0,"end":5},
                {"type":"CssSelector","value":"#one"},
                {"type":"CssSelector","value":"body","refinedBy":{
                    "type":"FragmentSelector","value":"text=Hello",
                    "conformsTo":"https://wicg.github.io/scroll-to-text-fragment/"
                }}
            ]),
        );

        assert!(matches!(results[0], AnnotationResolution::Fragment { .. }));
        assert!(matches!(results[1], AnnotationResolution::Text { .. }));
        assert!(matches!(
            results[2],
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::Dom,
                ..
            }
        ));
        assert!(matches!(
            results[3],
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert_eq!(results[0].text(), None);
        assert!(results.iter().all(|result| result.source().is_some()));
    }
}
