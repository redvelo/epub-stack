mod budget;
mod ingest;
mod navigation;
mod references;
mod resolution;

use super::coverage::{
    Coverage, CoverageState, IncompleteResource, RelationshipCoverage, RelationshipSource,
    ResourceCoverage,
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
    AnalysisIssue, AnalysisLimits, AnalysisOutcome, PublicationAnalysis, ResourceClassification,
    ResourceFacts, SemanticFormat,
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
use crate::navigation::{Navigation, NavigationDocument, NavigationSource, parse};
use crate::package::{Package, collection::Collection};
use crate::publication::Epub;
use crate::resource::provider::ResourceProvider;
use crate::resource::{
    AuthoredHref, AuthoredIdRef, DeclarationTargetRow, EpubPath, ManifestDeclarationRef,
    ParsedHref, ProviderPresence, ResolvedHref, ResourceAddress, ResourceIndex, ResourceRef,
    ResourceRow, parse_href,
};
use crate::xml::XmlUtf8Reader;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor, Read};

#[cfg(not(test))]
use ingest::ExtractionOutcome;
#[cfg(test)]
pub(crate) use ingest::{ExtractionOutcome, IngestPlan, StreamBudget, ingest_resource_reader};
use ingest::{
    ResourceIngest, classification_failure, css_candidate, ingest_analysis_resource,
    record_coverage, semantic_formats_for,
};
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
    let mut facts = Vec::with_capacity(resources.len());
    let mut classification_expected = Vec::new();
    let mut classification_completed = Vec::new();
    let mut classification_partial = Vec::new();
    let mut classification_unavailable = Vec::new();
    let mut content_expected = Vec::new();
    let mut content_completed = Vec::new();
    let mut content_partial = Vec::new();
    let mut content_unavailable = Vec::new();
    let mut fingerprint_expected = Vec::new();
    let mut fingerprint_completed = Vec::new();
    let mut fingerprint_unavailable = Vec::new();
    let mut inspection_expected = Vec::new();
    let mut inspection_completed = Vec::new();
    let mut inspection_partial = Vec::new();
    let mut inspection_unavailable = Vec::new();
    let mut analyzed_resources = 0usize;
    let mut analyzed_bytes = 0u64;
    let mut fingerprint_bytes = 0u64;
    let mut xhtml_sources = HashSet::new();
    let mut xhtml_pending = HashMap::new();
    let mut css_sources = HashSet::new();
    let mut css_references =
        HashMap::<ResourceRow, Vec<crate::content::extraction::css::CssPendingReference>>::new();
    let mut css_outcomes = HashMap::<ResourceRow, AnalysisOutcome<()>>::new();
    let mut svg_sources = HashSet::new();
    let mut svg_references = HashMap::<ResourceRow, Vec<SvgPendingRef>>::new();
    let mut smil_sources = HashSet::new();
    let mut smil_references = HashMap::<ResourceRow, Vec<SmilPendingReference>>::new();
    let mut accessibility_occurrences = Vec::new();
    let selected_ncx = publication
        .navigation()
        .ncx()
        .map(|document| document.path().clone());
    let mut secondary_ncx_keys = HashSet::new();
    for declaration in resources.declarations().filter(|declaration| {
        declaration
            .media_type()
            .is_some_and(crate::resource::MediaType::is_ncx)
    }) {
        let DeclarationTargetRow::Resource(key) = declaration.target_row() else {
            continue;
        };
        let Ok(record) = resources.resource((*key).into()) else {
            continue;
        };
        if record.local_path() != selected_ncx.as_ref() {
            secondary_ncx_keys.insert(*key);
        }
    }
    let mut secondary_ncx = HashMap::new();

    for record in resources.resources() {
        let key = record.key();
        if record.local_path().is_none() {
            facts.push(ResourceFacts::new(
                key,
                AnalysisOutcome::NotApplicable,
                AnalysisOutcome::NotApplicable,
                AnalysisOutcome::NotApplicable,
                AnalysisOutcome::NotApplicable,
            ));
            continue;
        }

        classification_expected.push(key);
        fingerprint_expected.push(key);
        inspection_expected.push(key);
        if limits
            .max_analyzed_resources()
            .is_some_and(|limit| analyzed_resources >= limit)
        {
            let classification = ResourceClassification::from_formats(semantic_formats_for(record));
            let content_format = match &classification {
                ResourceClassification::Identified(
                    format @ (SemanticFormat::Xhtml
                    | SemanticFormat::Css
                    | SemanticFormat::Smil
                    | SemanticFormat::Svg),
                ) => Some(*format),
                _ if css_candidate(record) => Some(SemanticFormat::Css),
                _ => None,
            };
            let classification =
                classification_failure(classification, AnalysisIssue::ResourceLimit);
            record_coverage(
                key,
                &classification,
                &mut classification_completed,
                &mut classification_partial,
                &mut classification_unavailable,
            );
            let issue = AnalysisIssue::ResourceLimit;
            let content = if let Some(format) = content_format {
                match format {
                    SemanticFormat::Xhtml => {
                        xhtml_sources.insert(key);
                    }
                    SemanticFormat::Smil => {
                        smil_sources.insert(key);
                    }
                    SemanticFormat::Css => {
                        css_sources.insert(key);
                        css_outcomes.insert(key, AnalysisOutcome::Unavailable(issue));
                    }
                    SemanticFormat::Svg => {
                        svg_sources.insert(key);
                    }
                }
                if format == SemanticFormat::Css {
                    AnalysisOutcome::NotApplicable
                } else {
                    content_expected.push(key);
                    content_unavailable.push(IncompleteResource::new(key, issue));
                    AnalysisOutcome::Unavailable(issue)
                }
            } else {
                AnalysisOutcome::NotApplicable
            };
            facts.push(ResourceFacts::new(
                key,
                classification,
                AnalysisOutcome::Unavailable(AnalysisIssue::ResourceLimit),
                AnalysisOutcome::Unavailable(AnalysisIssue::ResourceLimit),
                content,
            ));
            inspection_unavailable.push(IncompleteResource::new(key, AnalysisIssue::ResourceLimit));
            fingerprint_unavailable
                .push(IncompleteResource::new(key, AnalysisIssue::ResourceLimit));
            if secondary_ncx_keys.contains(&key) {
                secondary_ncx.insert(key, Err(AnalysisIssue::ResourceLimit));
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

        record_coverage(
            key,
            &classification,
            &mut classification_completed,
            &mut classification_partial,
            &mut classification_unavailable,
        );
        match &fingerprint {
            AnalysisOutcome::Complete(_) => fingerprint_completed.push(key),
            AnalysisOutcome::Unavailable(issue) => {
                fingerprint_unavailable.push(IncompleteResource::new(key, *issue));
            }
            AnalysisOutcome::NotApplicable | AnalysisOutcome::Partial { .. } => {
                unreachable!("local fingerprints are complete or unavailable")
            }
        }
        record_coverage(
            key,
            &inspection,
            &mut inspection_completed,
            &mut inspection_partial,
            &mut inspection_unavailable,
        );
        let mut content = AnalysisOutcome::NotApplicable;
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
                            AnalysisOutcome::Complete(ContentFacts::Xhtml(facts))
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::Smil(result) => {
                    smil_sources.insert(key);
                    content = match result {
                        Ok(extraction) => {
                            smil_references.insert(key, extraction.references);
                            AnalysisOutcome::Complete(ContentFacts::Smil(extraction.facts))
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                }
                ExtractionOutcome::Css(result, partial_issue) => {
                    css_sources.insert(key);
                    let outcome = match result {
                        Ok(extraction) => {
                            let issue = partial_issue.or(extraction.issue);
                            css_references.insert(key, extraction.references);
                            if let Some(issue) = issue {
                                AnalysisOutcome::Partial { value: (), issue }
                            } else {
                                AnalysisOutcome::Complete(())
                            }
                        }
                        Err(issue) => AnalysisOutcome::Unavailable(issue),
                    };
                    css_outcomes.insert(key, outcome);
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
                            let value = ContentFacts::Svg(extraction.facts);
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
        if !content.is_not_applicable() {
            content_expected.push(key);
            record_coverage(
                key,
                &content,
                &mut content_completed,
                &mut content_partial,
                &mut content_unavailable,
            );
        }
        facts.push(ResourceFacts::new(
            key,
            classification,
            fingerprint,
            inspection,
            content,
        ));
    }

    let mut references = Vec::new();
    let mut relationship_coverage = vec![RelationshipCoverage::new(
        RelationshipSource::Package,
        CoverageState::Complete,
    )];
    collect_manifest_references(&resources, &mut references);
    let package_link_references =
        collect_package_href_references(publication.package(), &resources, &facts, &mut references);
    collect_navigation_references(
        publication.navigation(),
        &resources,
        &facts,
        &xhtml_pending,
        &mut references,
        &mut relationship_coverage,
    );
    let secondary_navigation = facts
        .iter()
        .filter_map(|facts| secondary_ncx.get(&facts.resource_row())?.as_ref().ok())
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
        &mut facts,
        &css_sources,
        &css_outcomes,
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
    let coverage = Coverage::new(
        relationship_coverage,
        ResourceCoverage::new(
            classification_expected,
            classification_completed,
            classification_partial,
            classification_unavailable,
        ),
        fragment_coverage,
        ResourceCoverage::new(
            content_expected,
            content_completed,
            content_partial,
            content_unavailable,
        ),
        ResourceCoverage::new(
            inspection_expected,
            inspection_completed,
            inspection_partial,
            inspection_unavailable,
        ),
        ResourceCoverage::new(
            fingerprint_expected,
            fingerprint_completed,
            Vec::new(),
            fingerprint_unavailable,
        ),
    );
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
        coverage,
    )
}
