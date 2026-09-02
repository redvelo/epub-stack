use crate::analysis::PublicationAnalysis;
use crate::resource::{
    AuthoredHref, EpubPath, ParsedHref, ProviderPresence, ResolvedHref, ResourceAddress,
    ResourceIndex, ResourceRef, parse_href, resolve_local_href_from_source,
};

use super::{AnnotationSelector, AnnotationTarget, FragmentConformsTo, FragmentSelector};

/// Resolves an authored unfragmented annotation reference relative to a package document.
///
/// The reference must be a local relative href. Queries do not participate in resource identity,
/// matching [`ResourceIndex::resolve_manifest_href`]. Invalid syntax, fragments, absolute paths,
/// root escapes, encoded separators, and references with a scheme return `None`.
pub fn resolve_annotation_reference(reference: &str, package_path: &EpubPath) -> Option<EpubPath> {
    let target = reference
        .split_once('#')
        .map_or(reference, |(target, _)| target);
    let authored_path = target.split_once('?').map_or(target, |(path, _)| path);
    if !authored_path.is_empty() {
        let decoded = percent_encoding::percent_decode_str(authored_path)
            .decode_utf8()
            .ok()?;
        let mut segments = decoded.split('/');
        if segments.clone().any(str::is_empty)
            || segments
                .next_back()
                .is_some_and(|segment| matches!(segment, "." | ".."))
        {
            return None;
        }
    }
    let (path, fragment) =
        resolve_local_href_from_source(&AuthoredHref::new(reference), package_path)?;
    fragment.is_none().then_some(path)
}

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
            | Self::Ambiguous { source, .. }
            | Self::Broken { source }
            | Self::Unavailable { source }
            | Self::HostRequired { source, .. }
            | Self::InvalidSelector { source }
            | Self::UnsupportedSelector { source } => Some(source),
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
        let Some(fragment) = decode_fragment_selector_value(value) else {
            return AnnotationResolution::Broken { source };
        };
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
            if start > end {
                return Err(SelectorValidationError::Invalid);
            }
            if !source.has_xhtml_declaration() {
                return Err(SelectorValidationError::Unsupported);
            }
            HostRequirement::RenderedText
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
    match best {
        Some(requirement) => Ok(requirement),
        None if invalid => Err(SelectorValidationError::Invalid),
        None => Err(SelectorValidationError::Unsupported),
    }
}

fn fragment_requirement(
    selector: &FragmentSelector,
    source: ResourceRef<'_>,
) -> Result<HostRequirement, SelectorValidationError> {
    if selector.value().trim().is_empty() {
        return Err(SelectorValidationError::Invalid);
    }
    if decode_fragment_selector_value(selector.value()).is_none() {
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

/// Strictly decodes an authored FragmentSelector value without accepting a raw delimiter.
pub fn decode_fragment_selector_value(value: &str) -> Option<String> {
    if value.contains('#') {
        return None;
    }
    match parse_href(AuthoredHref::new(format!("#{value}"))) {
        ParsedHref::SameDocument { fragment, .. } => Some(fragment),
        _ => None,
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
    fn annotation_reference_resolution_matches_manifest_href_identity() {
        let epub = epub();
        let package_path = epub.package_path();
        for reference in [
            "chapter.xhtml",
            "./chapter.xhtml",
            "text/../chapter.xhtml",
            "chapt%65r.xhtml",
            "chapter.xhtml?view=reader",
            "?view=reader",
        ] {
            let resolved = resolve_annotation_reference(reference, package_path).unwrap();
            assert_eq!(
                epub.resources().resolve_manifest_href(reference),
                ResolvedHref::Resource(ResourceAddress::Local(resolved))
            );
        }

        for reference in [
            "",
            "chapter.xhtml#one",
            "https://example.com/chapter.xhtml",
            "//example.com/chapter.xhtml",
            "data:text/plain,chapter",
            "urn:example:chapter",
            "/chapter.xhtml",
            "../../chapter.xhtml",
            "text%2Fchapter.xhtml",
            "text%5cchapter.xhtml",
            "chapter%",
            "chapter%GG.xhtml",
            " chapter.xhtml",
            "chapter.xhtml\n",
            "chapter\\name.xhtml",
            "text//chapter.xhtml",
            "chapter.xhtml/",
            ".",
            "text/..",
        ] {
            assert_eq!(
                resolve_annotation_reference(reference, package_path),
                None,
                "{reference:?}"
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
    fn malformed_fragment_percent_encoding_is_invalid() {
        for value in ["literal%", "literal%GG", "%FF", "#one", "one#two"] {
            assert!(matches!(
                resolve(
                    "chapter.xhtml",
                    json!([{"type":"FragmentSelector","value":value}])
                )[0],
                AnnotationResolution::InvalidSelector { .. }
            ));
        }

        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"%6fne"}])
            )[0],
            AnnotationResolution::Fragment { ref value, .. } if value == "one"
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"one%23two"}])
            )[0],
            AnnotationResolution::Broken { .. }
        ));
    }

    #[test]
    fn text_positions_require_rendered_text_host_resolution() {
        let resolution = resolve(
            "chapter.xhtml",
            json!([{"type":"TextPositionSelector","start":0,"end":5}]),
        )
        .pop()
        .unwrap();
        assert!(matches!(
            &resolution,
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert!(resolution.source().is_some());

        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"TextPositionSelector","start":4,"end":4}]),
            )[0],
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"TextPositionSelector","start":5,"end":4}]),
            )[0],
            AnnotationResolution::InvalidSelector { .. }
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
        assert!(matches!(
            results[1],
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
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
        assert!(results.iter().all(|result| result.source().is_some()));
    }

    #[test]
    fn valid_refinement_branch_survives_invalid_and_unsupported_siblings() {
        let resolution = resolve(
            "chapter.xhtml",
            json!([{
                "type":"CssSelector",
                "value":"body",
                "refinedBy":[
                    {"value":"missing type"},
                    {"type":"FutureSelector","value":"future"},
                    {"type":"CssSelector","value":"#one"}
                ]
            }]),
        )
        .pop()
        .unwrap();

        assert!(matches!(
            resolution,
            AnnotationResolution::HostRequired {
                requirement: HostRequirement::Dom,
                ..
            }
        ));

        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{
                    "type":"CssSelector",
                    "value":"body",
                    "refinedBy":[{"value":"missing type"}]
                }])
            )[0],
            AnnotationResolution::InvalidSelector { .. }
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{
                    "type":"CssSelector",
                    "value":"body",
                    "refinedBy":[{"type":"FutureSelector","value":"future"}]
                }])
            )[0],
            AnnotationResolution::UnsupportedSelector { .. }
        ));
    }
}
