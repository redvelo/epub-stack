mod budget;
mod ingest;
mod navigation;
mod references;
mod resolution;

use super::coverage::{
    Completeness, RelationshipCoverage, RelationshipSource, ResourceCompleteness,
};
use super::fingerprint::Blake3Hash;
use super::inspection::{
    DetectedFormat, Detection, ResourceInspection, detect as detect_resource_format,
    inspect as inspect_resource,
};
use super::reference::{
    AuthoredReference, CssContext, ElementAttribute, HrefRole, HrefTarget, ManifestRole,
    ManifestTarget, ReferenceContext, ReferenceSlot, XhtmlReferenceIndex, href_reference,
    manifest_reference,
};
use super::{
    AnalysisIssue, AnalysisLimit, AnalysisLimits, AnalysisOutcome, PublicationAnalysis,
    ResourceAnalysis, ResourceClassification, SemanticFormat,
};
use crate::accessibility::AccessibilityFacts;
use crate::content::extraction::svg::{SvgPendingRef, scan as scan_svg};
use crate::content::{
    ContentFacts, LinkFact, NavigationLinkKind, SvgFacts, XhtmlExtraction, XhtmlLinkAssociations,
    parse_xhtml_document_from_reader_counted,
};
use crate::media_overlay::MediaOverlayFacts;
use crate::media_overlay::smil::{SmilError, SmilPendingReference};
use crate::media_type::MediaTypeClassification;
use crate::navigation::{NavigationDocument, NavigationSource, parse};
use crate::package::{Package, collection::Collection};
use crate::publication::Epub;
use crate::resource::provider::ResourceProvider;
use crate::resource::{
    AuthoredHref, AuthoredIdRef, EpubPath, IdrefTarget, ResolvedHref, ResourceAddress,
    ResourceIndex, ResourceOrdinal, ResourceRef,
};
use crate::xml::XmlUtf8Reader;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor, Read};

#[cfg(not(test))]
use ingest::ExtractionOutcome;
#[cfg(test)]
pub(crate) use ingest::{ExtractionOutcome, IngestPlan, StreamBudget, ingest_resource_reader};
use ingest::{ResourceIngest, css_candidate, ingest_analysis_resource, semantic_formats_for};
use navigation::*;
#[cfg(test)]
pub(crate) use references::collect_smil_references;
use references::*;
use resolution::fragment_coverage;
#[cfg(test)]
pub(crate) use resolution::probe_root_format;

pub(crate) fn analyze<R: ResourceProvider>(
    publication: &Epub<R>,
    limits: AnalysisLimits,
) -> PublicationAnalysis {
    let resources = publication.resources().clone();
    let mut facts = Vec::with_capacity(resources.resources().len());
    let mut analyzed_resources = 0usize;
    let mut analyzed_bytes = 0u64;
    let mut fingerprint_bytes = 0u64;
    let mut xhtml_sources = HashSet::new();
    let mut xhtml_pending = HashMap::new();
    let mut css_sources = HashSet::new();
    let mut css_references =
        HashMap::<ResourceOrdinal, Vec<crate::content::extraction::css::CssPendingReference>>::new(
        );
    let mut svg_sources = HashSet::new();
    let mut svg_references = HashMap::<ResourceOrdinal, Vec<SvgPendingRef>>::new();
    let mut smil_sources = HashSet::new();
    let mut smil_references = HashMap::<ResourceOrdinal, Vec<SmilPendingReference>>::new();
    let mut accessibility_occurrences = Vec::new();
    let selected_ncx = publication
        .navigation()
        .as_ref()
        .filter(|document| document.is_ncx())
        .map(|document| document.path().clone());
    let mut secondary_ncx_keys = HashSet::new();
    for declaration in resources.declarations().filter(|declaration| {
        declaration
            .media_type()
            .is_some_and(crate::resource::MediaType::is_ncx)
    }) {
        let Some(record) = declaration.resource() else {
            continue;
        };
        if record.local_path() != selected_ncx.as_ref() {
            secondary_ncx_keys.insert(record.ordinal());
        }
    }
    let mut secondary_ncx = HashMap::new();
    let mut relationship_coverage = Vec::new();

    for record in resources.resources() {
        let key = record.ordinal();
        if record.local_path().is_none() {
            relationship_coverage.extend(remote_relationship_coverage(record));
            facts.push(ResourceAnalysis::new(
                key,
                AnalysisOutcome::NotApplicable,
                AnalysisOutcome::NotApplicable,
                AnalysisOutcome::NotApplicable,
            ));
            continue;
        }

        if limits
            .max_analyzed_resources
            .is_some_and(|limit| analyzed_resources >= limit)
        {
            let issue = AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources);
            let classification = ResourceClassification::from_formats(semantic_formats_for(record));
            let content_format = match &classification {
                ResourceClassification::Identified(format) => Some(*format),
                _ if css_candidate(record) => Some(SemanticFormat::Css),
                _ => None,
            };
            match content_format {
                Some(SemanticFormat::Xhtml) => {
                    xhtml_sources.insert(key);
                }
                Some(SemanticFormat::Smil) => {
                    smil_sources.insert(key);
                }
                Some(SemanticFormat::Css) => {
                    css_sources.insert(key);
                }
                Some(SemanticFormat::Svg) => {
                    svg_sources.insert(key);
                }
                None => {}
            }
            let content = match content_format {
                Some(_) => AnalysisOutcome::Unavailable(issue),
                None => AnalysisOutcome::NotApplicable,
            };
            facts.push(ResourceAnalysis::new(
                key,
                AnalysisOutcome::Unavailable(issue),
                AnalysisOutcome::Unavailable(issue),
                content,
            ));
            if secondary_ncx_keys.contains(&key) {
                secondary_ncx.insert(key, Err(issue));
            }
            continue;
        }
        analyzed_resources += 1;
        let secondary_ncx_path = secondary_ncx_keys
            .contains(&key)
            .then(|| record.local_path().cloned())
            .flatten();
        let ingest = ingest_analysis_resource(
            publication,
            record,
            &resources,
            secondary_ncx_path,
            &limits,
            analyzed_bytes,
            fingerprint_bytes,
        );
        let ResourceIngest {
            classification,
            fingerprint,
            inspection,
            extractions,
            analysis_bytes: resource_analysis_bytes,
            fingerprint_bytes: resource_fingerprint_bytes,
        } = ingest;
        analyzed_bytes = analyzed_bytes.saturating_add(resource_analysis_bytes);
        fingerprint_bytes = fingerprint_bytes.saturating_add(resource_fingerprint_bytes);

        let mut content = match classification {
            AnalysisOutcome::Partial { issue, .. } | AnalysisOutcome::Unavailable(issue) => {
                AnalysisOutcome::Unavailable(issue)
            }
            AnalysisOutcome::Complete(_) | AnalysisOutcome::NotApplicable => {
                AnalysisOutcome::NotApplicable
            }
        };
        for extraction in extractions {
            match extraction {
                ExtractionOutcome::Xhtml(result) => {
                    xhtml_sources.insert(key);
                    content = match result {
                        Ok(extraction) => {
                            let XhtmlExtraction {
                                facts,
                                accessibility,
                                links,
                                authored_base,
                                associations,
                            } = *extraction;
                            accessibility_occurrences
                                .extend(accessibility.into_iter().map(|fact| (key, fact)));
                            xhtml_pending.insert(key, (links, authored_base, associations));
                            AnalysisOutcome::Complete(ContentFacts::from_xhtml(facts))
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::Smil(result) => {
                    smil_sources.insert(key);
                    content = match result {
                        Ok(extraction) => {
                            smil_references.insert(key, extraction.references);
                            AnalysisOutcome::Complete(ContentFacts::from_smil(extraction.facts))
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::Css(result, partial_issue) => {
                    css_sources.insert(key);
                    content = match result {
                        Ok(extraction) => {
                            css_references.insert(key, extraction.references);
                            match partial_issue.or(extraction.issue) {
                                Some(issue) => AnalysisOutcome::Partial {
                                    value: ContentFacts::Css,
                                    issue,
                                },
                                None => AnalysisOutcome::Complete(ContentFacts::Css),
                            }
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::Svg(result, partial_issue) => {
                    svg_sources.insert(key);
                    content = match result {
                        Ok(extraction) => {
                            let issue = partial_issue.or(extraction.issue);
                            svg_references.insert(key, extraction.references);
                            accessibility_occurrences.extend(
                                extraction.accessibility.into_iter().map(|fact| (key, fact)),
                            );
                            let value = ContentFacts::from_svg(extraction.facts);
                            if let Some(issue) = issue {
                                AnalysisOutcome::Partial { value, issue }
                            } else {
                                AnalysisOutcome::Complete(value)
                            }
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::SecondaryNcx(result) => {
                    secondary_ncx.insert(key, result);
                }
            }
        }
        facts.push(ResourceAnalysis::new(key, fingerprint, inspection, content));
    }

    let mut references = Vec::new();
    collect_manifest_references(&resources, &mut references);
    let package_link_references =
        collect_package_href_references(publication.package(), &resources, &facts, &mut references);
    collect_navigation_references(
        publication.navigation(),
        &resources,
        &facts,
        &mut references,
        &mut relationship_coverage,
    );
    let secondary_navigation = facts
        .iter()
        .filter_map(|facts| secondary_ncx.get(&facts.resource())?.as_ref().ok())
        .cloned()
        .collect::<Vec<_>>();
    collect_secondary_ncx_references(
        &resources,
        &facts,
        secondary_ncx,
        &mut references,
        &mut relationship_coverage,
    );
    let xhtml_references = collect_xhtml_references(
        &resources,
        &facts,
        &xhtml_sources,
        xhtml_pending,
        &mut references,
        &mut relationship_coverage,
    );
    collect_css_references(
        &resources,
        &facts,
        &css_sources,
        css_references,
        &mut references,
        &mut relationship_coverage,
    );
    collect_svg_references(
        &resources,
        &facts,
        &svg_sources,
        svg_references,
        &mut references,
        &mut relationship_coverage,
    );
    collect_smil_references(
        &resources,
        &mut facts,
        &smil_sources,
        smil_references,
        &mut references,
        &mut relationship_coverage,
    );

    let fragment_coverage = fragment_coverage(&references, &facts);
    let media_overlays = MediaOverlayFacts::build(publication.package(), &resources);
    let accessibility = AccessibilityFacts::build(
        publication.package(),
        publication.navigation(),
        &secondary_navigation,
        &resources,
        &facts,
        accessibility_occurrences,
        &package_link_references,
    );
    PublicationAnalysis::new(
        limits,
        resources,
        facts,
        references,
        xhtml_references,
        media_overlays,
        accessibility,
        relationship_coverage,
        fragment_coverage,
    )
}

fn remote_relationship_coverage(
    record: ResourceRef<'_>,
) -> impl Iterator<Item = RelationshipCoverage> + '_ {
    let key = record.ordinal();
    let mut seen = HashSet::new();
    record
        .declarations()
        .filter_map(move |declaration| {
            let media_type = declaration.media_type()?;
            if media_type.is_xhtml() {
                Some(RelationshipSource::Xhtml(key))
            } else if media_type.is_css() {
                Some(RelationshipSource::Css(key))
            } else if media_type.is_svg() {
                Some(RelationshipSource::Svg(key))
            } else if media_type.is_smil() {
                Some(RelationshipSource::Smil(key))
            } else if media_type.is_ncx() {
                Some(RelationshipSource::Ncx(key))
            } else {
                None
            }
        })
        .filter(move |source| seen.insert(*source))
        .map(|source| RelationshipCoverage {
            source,
            completeness: Completeness::Unavailable(AnalysisIssue::Unsupported),
        })
}
