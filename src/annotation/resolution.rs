use crate::analysis::PublicationAnalysis;
use crate::resource::{
    AuthoredHref, MediaType, ParsedHref, ResolvedHref, ResourceAddress, ResourceIndex, ResourceRef,
    parse_href,
};

use super::{AnnotationSelector, AnnotationTarget, FragmentConformsTo, FragmentSelector};

/// Why an annotation target source is not a present publication resource.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnnotationSourceError {
    /// No matching manifest resource exists, or the authored source is absent or empty.
    #[error("annotation target source does not match a manifest resource")]
    Missing,
    /// The manifest resource exists but is absent from the provider inventory.
    #[error("annotation target source is missing from the provider: {address:?}")]
    ProviderMissing {
        /// Resolved publication address.
        address: ResourceAddress,
    },
    /// The source address kind is not supported for annotation resolution.
    #[error("annotation target source is not a local publication resource")]
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

/// The result of resolving one selector alternative against analyzed content.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectorResolution {
    /// A fragment selected exactly one element.
    Fragment(
        /// Percent-decoded fragment identifier.
        String,
    ),
    /// A fragment identifier matched multiple elements.
    Ambiguous {
        /// Number of distinct matching elements.
        matches: usize,
    },
    /// The selector was valid but did not identify existing content.
    Broken,
    /// Content analysis for the source was unavailable.
    Unavailable,
    /// Resolution requires browser-host layout or DOM behavior.
    HostRequired {
        /// Selector to resolve in the host.
        selector: AnnotationSelector,
        /// Minimum host capability required.
        requirement: HostRequirement,
    },
    /// A selector lacked required values or contradicted its own invariants.
    Invalid,
    /// A well-shaped selector or conformance is not supported for this source.
    Unsupported,
}

/// An annotation target whose source resolved to a present publication resource.
///
/// A target without selectors resolves to its resource alone, so [`Self::selectors`] is empty.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTarget<'a> {
    analysis: &'a PublicationAnalysis,
    record: ResourceRef<'a>,
    selectors: &'a [AnnotationSelector],
}

impl<'a> ResolvedTarget<'a> {
    /// The resolved publication address of the target source.
    pub fn source(&self) -> &'a ResourceAddress {
        self.record.address()
    }

    /// The source resource in the analyzed snapshot.
    pub fn resource(&self) -> ResourceRef<'a> {
        self.record
    }

    /// Resolves each authored selector alternative in order.
    pub fn selectors(&self) -> impl Iterator<Item = SelectorResolution> + 'a {
        let analysis = self.analysis;
        let record = self.record;
        self.selectors
            .iter()
            .map(move |selector| analysis.resolve_annotation_selector(record, selector))
    }
}

impl AnnotationTarget {
    /// Resolves the target source against a resource inventory.
    ///
    /// To resolve selectors, use [`PublicationAnalysis::resolve_annotation_target`].
    pub fn resolve_source(
        &self,
        resources: &ResourceIndex,
    ) -> Result<ResourceAddress, AnnotationSourceError> {
        resolve_target_source(resources, self.source().unwrap_or_default())
            .map(|record| record.address().clone())
    }
}

impl PublicationAnalysis {
    /// Resolves a target source against analyzed publication content.
    ///
    /// Selector alternatives are resolved lazily through [`ResolvedTarget::selectors`]. Missing
    /// analysis and browser-only work produce `Unavailable` and `HostRequired`.
    pub fn resolve_annotation_target<'a>(
        &'a self,
        target: &'a AnnotationTarget,
    ) -> Result<ResolvedTarget<'a>, AnnotationSourceError> {
        let record = resolve_target_source(self.resources(), target.source().unwrap_or_default())?;
        Ok(ResolvedTarget {
            analysis: self,
            record,
            selectors: target.selectors(),
        })
    }

    fn resolve_annotation_selector(
        &self,
        source_record: ResourceRef<'_>,
        selector: &AnnotationSelector,
    ) -> SelectorResolution {
        let requirement = match selector_requirement(selector, source_record) {
            Ok(requirement) => requirement,
            Err(SelectorValidationError::Invalid) => return SelectorResolution::Invalid,
            Err(SelectorValidationError::Unsupported) => return SelectorResolution::Unsupported,
        };
        match selector {
            AnnotationSelector::Fragment(fragment)
                if fragment.refined_by().is_empty()
                    && !matches!(
                        fragment.conforms_to(),
                        Some(FragmentConformsTo::TextFragment)
                    ) =>
            {
                self.resolve_fragment(source_record, fragment.value().unwrap_or_default())
            }
            _ => SelectorResolution::HostRequired {
                selector: selector.clone(),
                requirement,
            },
        }
    }

    fn resolve_fragment(&self, source_record: ResourceRef<'_>, value: &str) -> SelectorResolution {
        let Some(facts) = self
            .resource_analysis(source_record.ordinal())
            .content()
            .value()
            .and_then(crate::content::ContentFacts::as_xhtml)
        else {
            return SelectorResolution::Unavailable;
        };
        let Some(fragment) = decode_fragment_selector_value(value) else {
            return SelectorResolution::Broken;
        };
        let matches = facts
            .fragments()
            .iter()
            .filter(|fact| fact.id() == fragment)
            .map(crate::content::FragmentFact::element_ordinal)
            .collect::<std::collections::HashSet<_>>()
            .len();
        match matches {
            0 => SelectorResolution::Broken,
            1 => SelectorResolution::Fragment(fragment),
            matches => SelectorResolution::Ambiguous { matches },
        }
    }
}

fn resolve_target_source<'a>(
    resources: &'a ResourceIndex,
    source: &str,
) -> Result<ResourceRef<'a>, AnnotationSourceError> {
    if source.is_empty() {
        return Err(AnnotationSourceError::Missing);
    }
    let address = match resources.resolve_manifest_href(source) {
        Ok(ResolvedHref {
            address: address @ ResourceAddress::Local(_),
            fragment: None,
            ..
        }) => address,
        _ => return Err(AnnotationSourceError::Unsupported),
    };
    let Some(record) = resources.resource_at(&address) else {
        return Err(AnnotationSourceError::Missing);
    };
    if record.declarations().len() == 0 {
        return Err(AnnotationSourceError::Missing);
    }
    if !record.presence().is_present() {
        return Err(AnnotationSourceError::ProviderMissing { address });
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
            if selector.value().is_none_or(|value| value.trim().is_empty()) {
                return Err(SelectorValidationError::Invalid);
            }
            if !(source.declares(MediaType::is_xhtml) || source.declares(MediaType::is_svg)) {
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
            if !source.declares(MediaType::is_xhtml) {
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
    let Some(value) = selector.value().filter(|value| !value.trim().is_empty()) else {
        return Err(SelectorValidationError::Invalid);
    };
    if decode_fragment_selector_value(value).is_none() {
        return Err(SelectorValidationError::Invalid);
    }
    match selector.conforms_to() {
        None | Some(FragmentConformsTo::Html) if source.declares(MediaType::is_xhtml) => {
            Ok(HostRequirement::Dom)
        }
        Some(FragmentConformsTo::TextFragment) if source.declares(MediaType::is_xhtml) => {
            Ok(HostRequirement::RenderedText)
        }
        _ => Err(SelectorValidationError::Unsupported),
    }
}

/// Strictly decodes an authored FragmentSelector value without accepting a raw delimiter.
pub(crate) fn decode_fragment_selector_value(value: &str) -> Option<String> {
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
    use crate::resource::EpubPath;
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
        Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap()
    }

    fn target(source: &str, selectors: Value) -> AnnotationTarget {
        AnnotationTarget::from_value(json!({"source":source,"selector":selectors})).unwrap()
    }

    fn resolve(source: &str, selectors: Value) -> Vec<SelectorResolution> {
        let analysis = epub().analyze();
        let target = target(source, selectors);
        analysis
            .resolve_annotation_target(&target)
            .expect("source resolves")
            .selectors()
            .collect()
    }

    #[test]
    fn source_resolution_distinguishes_inventory_outcomes() {
        let epub = epub();
        for (source, expected) in [
            ("unknown.xhtml", "missing"),
            ("missing.xhtml", "provider-missing"),
            ("https://example.com/chapter.xhtml", "unsupported"),
            ("chapter.xhtml", "resolved"),
        ] {
            let state = target(source, json!([])).resolve_source(epub.resources());
            assert!(
                matches!(
                    (&state, expected),
                    (Err(AnnotationSourceError::Missing), "missing")
                        | (
                            Err(AnnotationSourceError::ProviderMissing { .. }),
                            "provider-missing"
                        )
                        | (Err(AnnotationSourceError::Unsupported), "unsupported")
                        | (Ok(_), "resolved")
                ),
                "{source}: {state:?}"
            );
        }
    }

    #[test]
    fn target_source_resolution_matches_manifest_href_identity() {
        let epub = epub();
        let resources = epub.resources();
        let chapter = ResourceAddress::Local(EpubPath::new("EPUB/chapter.xhtml").unwrap());
        for reference in [
            "chapter.xhtml",
            "./chapter.xhtml",
            "text/../chapter.xhtml",
            "chapt%65r.xhtml",
            "chapter.xhtml?view=reader",
            "chapter.xhtml/",
        ] {
            assert_eq!(
                target(reference, json!([])).resolve_source(resources),
                Ok(chapter.clone()),
                "{reference:?}"
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
            ".",
            "text/..",
        ] {
            assert!(
                target(reference, json!([]))
                    .resolve_source(resources)
                    .is_err(),
                "{reference:?}"
            );
        }
    }

    #[test]
    fn source_failures_and_selectorless_targets_project_consistently() {
        let analysis = epub().analyze();
        for (source, expected) in [
            ("unknown.xhtml", AnnotationSourceError::Missing),
            (
                "missing.xhtml",
                AnnotationSourceError::ProviderMissing {
                    address: ResourceAddress::Local(EpubPath::new("EPUB/missing.xhtml").unwrap()),
                },
            ),
            (
                "https://example.com/chapter.xhtml",
                AnnotationSourceError::Unsupported,
            ),
        ] {
            let target = target(source, json!([]));
            assert_eq!(
                analysis.resolve_annotation_target(&target).err(),
                Some(expected),
                "{source}"
            );
        }

        let analysis = epub().analyze();
        let selectorless = target("chapter.xhtml", json!([]));
        let resolved = analysis
            .resolve_annotation_target(&selectorless)
            .expect("source resolves");
        assert!(matches!(resolved.source(), ResourceAddress::Local(_)));
        assert_eq!(resolved.selectors().count(), 0);
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
        assert!(matches!(results[0], SelectorResolution::Invalid));
        assert!(matches!(results[1], SelectorResolution::Unsupported));

        assert!(matches!(
            resolve("image.png", json!([{"type":"CssSelector","value":"body"}]))[0],
            SelectorResolution::Unsupported
        ));
    }

    #[test]
    fn fragment_resolution_reports_broken_ambiguous_and_unavailable_content() {
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"absent"}])
            )[0],
            SelectorResolution::Broken
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"duplicate"}])
            )[0],
            SelectorResolution::Ambiguous { matches: 2, .. }
        ));
        assert!(matches!(
            resolve(
                "broken.xhtml",
                json!([{"type":"FragmentSelector","value":"one"}])
            )[0],
            SelectorResolution::Unavailable
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
                SelectorResolution::Invalid
            ));
        }

        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"%6fne"}])
            )[0],
            SelectorResolution::Fragment(ref value) if value == "one"
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"FragmentSelector","value":"one%23two"}])
            )[0],
            SelectorResolution::Broken
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
            SelectorResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"TextPositionSelector","start":4,"end":4}]),
            )[0],
            SelectorResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert!(matches!(
            resolve(
                "chapter.xhtml",
                json!([{"type":"TextPositionSelector","start":5,"end":4}]),
            )[0],
            SelectorResolution::Invalid
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

        assert!(matches!(results[0], SelectorResolution::Fragment { .. }));
        assert!(matches!(
            results[1],
            SelectorResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
        assert!(matches!(
            results[2],
            SelectorResolution::HostRequired {
                requirement: HostRequirement::Dom,
                ..
            }
        ));
        assert!(matches!(
            results[3],
            SelectorResolution::HostRequired {
                requirement: HostRequirement::RenderedText,
                ..
            }
        ));
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
            SelectorResolution::HostRequired {
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
            SelectorResolution::Invalid
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
            SelectorResolution::Unsupported
        ));
    }
}
