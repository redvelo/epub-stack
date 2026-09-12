//! Searchable text, resolved links, media details, accessibility facts, and editing insights.
//!
//! [`PublicationAnalysis`] gives applications one coherent view of extracted content,
//! resource metadata, authored relationships, media overlays, and accessibility observations.
//! [`PublicationAnalysis::coverage`] reports which expected results are complete, partial, or
//! unavailable because of limits, missing bytes, or unsupported formats.
//!
//! Building a full analysis may read every selected publication resource, subject to
//! [`AnalysisLimits`]. The result is an owned snapshot: its ordinals and borrowed graph views
//! describe only that snapshot, and after the publication is edited it still describes the version
//! that was analyzed. Results contain the modeled facts exposed by these APIs, not complete
//! source bytes or unknown document structure.

pub mod coverage;
pub mod dependency;
pub mod fingerprint;
pub mod impact;
pub mod inspection;
pub(crate) mod orchestration;
pub mod reference;
mod relationships;
pub mod search;

use crate::accessibility::{
    AccessibilityCertifierReport, AccessibilityFacts, AccessibilityObservationRef,
};
use crate::content::text::TextStream;
use crate::content::{ContentFacts, XhtmlFacts};
use crate::media_overlay::{MediaOverlayFacts, SmilNodeRef};
use crate::resource::{
    IndexRowError, OrdinalOutOfBounds, ResourceIndex, ResourceOrdinal, ResourceRow,
};
use coverage::Coverage;
use fingerprint::Blake3Hash;
use reference::{AuthoredReference, HrefReference, XhtmlReferenceIndex};
use std::collections::{HashMap, HashSet};

/// Resource-count, byte, and SMIL structural budgets for one full publication analysis.
///
/// Each `None` disables that individual limit. Reaching a limit produces partial or unavailable
/// results recorded by [`AnalysisOutcome`] and [`Coverage`]; it does not imply that the EPUB is
/// invalid. The applied limits are retained by [`PublicationAnalysis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisLimits {
    max_analyzed_resources: Option<usize>,
    max_resource_analysis_bytes: Option<u64>,
    max_total_analysis_bytes: Option<u64>,
    max_total_fingerprint_bytes: Option<u64>,
    max_smil_nodes: Option<usize>,
    max_smil_nesting: Option<usize>,
}

impl AnalysisLimits {
    /// Creates a set of independent resource-count, byte, and SMIL structural budgets.
    ///
    /// The byte limits apply, in order, per analyzed resource, across content analysis,
    /// and across fingerprinting. `None` disables the corresponding budget.
    pub fn new(
        max_analyzed_resources: Option<usize>,
        max_resource_analysis_bytes: Option<u64>,
        max_total_analysis_bytes: Option<u64>,
        max_total_fingerprint_bytes: Option<u64>,
        max_smil_nodes: Option<usize>,
        max_smil_nesting: Option<usize>,
    ) -> Self {
        Self {
            max_analyzed_resources,
            max_resource_analysis_bytes,
            max_total_analysis_bytes,
            max_total_fingerprint_bytes,
            max_smil_nodes,
            max_smil_nesting,
        }
    }

    /// Returns the maximum number of resources selected for byte analysis.
    pub fn max_analyzed_resources(&self) -> Option<usize> {
        self.max_analyzed_resources
    }

    /// Returns the maximum analyzed bytes for any one resource.
    pub fn max_resource_analysis_bytes(&self) -> Option<u64> {
        self.max_resource_analysis_bytes
    }

    /// Returns the aggregate byte budget for content analysis and inspection.
    pub fn max_total_analysis_bytes(&self) -> Option<u64> {
        self.max_total_analysis_bytes
    }

    /// Returns the aggregate byte budget for fingerprinting.
    pub fn max_total_fingerprint_bytes(&self) -> Option<u64> {
        self.max_total_fingerprint_bytes
    }

    /// Returns the maximum number of XML nodes accepted in one SMIL document.
    pub fn max_smil_nodes(&self) -> Option<usize> {
        self.max_smil_nodes
    }

    /// Returns the maximum element nesting depth accepted in one SMIL document.
    pub fn max_smil_nesting(&self) -> Option<usize> {
        self.max_smil_nesting
    }

    /// Replaces the resource-count budget.
    pub fn with_max_analyzed_resources(mut self, value: Option<usize>) -> Self {
        self.max_analyzed_resources = value;
        self
    }

    /// Replaces the per-resource analysis byte budget.
    pub fn with_max_resource_analysis_bytes(mut self, value: Option<u64>) -> Self {
        self.max_resource_analysis_bytes = value;
        self
    }

    /// Replaces the aggregate content-analysis byte budget.
    pub fn with_max_total_analysis_bytes(mut self, value: Option<u64>) -> Self {
        self.max_total_analysis_bytes = value;
        self
    }

    /// Replaces the aggregate fingerprint byte budget.
    pub fn with_max_total_fingerprint_bytes(mut self, value: Option<u64>) -> Self {
        self.max_total_fingerprint_bytes = value;
        self
    }

    /// Replaces the per-document SMIL node-count budget.
    pub fn with_max_smil_nodes(mut self, value: Option<usize>) -> Self {
        self.max_smil_nodes = value;
        self
    }

    /// Replaces the per-document SMIL nesting-depth budget.
    pub fn with_max_smil_nesting(mut self, value: Option<usize>) -> Self {
        self.max_smil_nesting = value;
        self
    }
}

impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            max_analyzed_resources: Some(10_000),
            max_resource_analysis_bytes: Some(32 * 1024 * 1024),
            max_total_analysis_bytes: Some(512 * 1024 * 1024),
            max_total_fingerprint_bytes: Some(8 * 1024 * 1024 * 1024),
            max_smil_nodes: Some(100_000),
            max_smil_nesting: Some(256),
        }
    }
}

/// Availability of one analysis result.
///
/// This reports whether the result could be produced, not whether the EPUB is valid. Missing or
/// ambiguous authored targets can therefore be part of `T`, while budget and read failures are
/// represented by this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisOutcome<T> {
    /// This result does not apply to the resource or source.
    NotApplicable,
    /// The full result is available.
    Complete(T),
    /// A usable result is available but is incomplete for one operational reason.
    Partial {
        /// The usable portion of the result.
        value: T,
        /// The single operational reason the result is incomplete.
        issue: AnalysisIssue,
    },
    /// No usable result is available for the stated operational reason.
    Unavailable(AnalysisIssue),
}

impl<T> AnalysisOutcome<T> {
    /// Borrows the result from complete and partial outcomes.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Complete(value) | Self::Partial { value, .. } => Some(value),
            Self::NotApplicable | Self::Unavailable(_) => None,
        }
    }

    /// Returns the reason a result is partial or unavailable.
    pub fn issue(&self) -> Option<AnalysisIssue> {
        match self {
            Self::Partial { issue, .. } | Self::Unavailable(issue) => Some(*issue),
            Self::NotApplicable | Self::Complete(_) => None,
        }
    }

    /// Returns whether the operation completed without an issue.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }

    /// Returns whether the operation was inapplicable rather than attempted.
    pub fn is_not_applicable(&self) -> bool {
        matches!(self, Self::NotApplicable)
    }

    pub(crate) fn value_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Complete(value) | Self::Partial { value, .. } => Some(value),
            Self::NotApplicable | Self::Unavailable(_) => None,
        }
    }
}

/// The reason an analysis result is incomplete or unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalysisIssue {
    /// The run reached its resource-count budget.
    ResourceLimit,
    /// A resource exceeded the per-resource analysis byte budget.
    PerResourceAnalysisLimit,
    /// The run reached its aggregate analysis byte budget.
    TotalAnalysisLimit,
    /// The run reached its aggregate fingerprint byte budget.
    TotalFingerprintLimit,
    /// A SMIL document exceeded its configured XML node-count budget.
    SmilNodeLimit,
    /// A SMIL document exceeded its configured element nesting-depth budget.
    SmilNestingLimit,
    /// Expected local bytes were absent.
    Missing,
    /// The provider could not read the resource bytes.
    Unreadable,
    /// The resource format or operation is not supported.
    Unsupported,
    /// The bytes could not produce the supported semantic result.
    Malformed,
    /// Inspection required byte access the provider could not supply.
    RandomAccessUnavailable,
    /// A supported parser failed before producing a result.
    ParserFailure,
}

/// A content format recognized for semantic extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticFormat {
    /// XHTML content.
    Xhtml,
    /// Cascading Style Sheets.
    Css,
    /// Standalone SVG content.
    Svg,
    /// SMIL media-overlay content.
    Smil,
}

/// Classification of a resource as a supported semantic document format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceClassification {
    /// No supported semantic format was identified.
    Unknown,
    /// Exactly one supported semantic format was identified.
    Identified(SemanticFormat),
    /// Package declarations identify the resource as multiple incompatible formats.
    Conflict(Vec<SemanticFormat>),
}

/// Whether one physical resource is conservatively safe to prepare in a foreground frame.
///
/// Eligibility requires complete analysis of a supported XHTML or standalone SVG document and no
/// executable content or supported active early-lifecycle construct. `Unknown` means eligibility
/// could not be established from a complete supported analysis; it is not equivalent to
/// ineligibility. XHTML support and hazards reflect the extractor's normalized HTML parse rather
/// than separate strict XML validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForegroundPreparationEligibility {
    /// Complete supported XHTML or standalone SVG analysis found no excluded construct.
    Eligible,
    /// Complete analysis established that the resource is not eligible.
    Ineligible,
    /// Analysis could not establish eligibility or ineligibility.
    Unknown,
}

impl ResourceClassification {
    pub(crate) fn from_formats(semantic_formats: impl IntoIterator<Item = SemanticFormat>) -> Self {
        let mut formats = semantic_formats.into_iter().collect::<Vec<_>>();
        formats.sort_unstable_by_key(|format| match format {
            SemanticFormat::Xhtml => 0,
            SemanticFormat::Css => 1,
            SemanticFormat::Svg => 2,
            SemanticFormat::Smil => 3,
        });
        formats.dedup();
        match formats.as_slice() {
            [] => Self::Unknown,
            [format] => Self::Identified(*format),
            _ => Self::Conflict(formats),
        }
    }
}

/// Extracted content, inspection metadata, classification, and fingerprint for one resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFacts {
    resource: ResourceRow,
    classification: AnalysisOutcome<ResourceClassification>,
    fingerprint: AnalysisOutcome<Blake3Hash>,
    inspection: AnalysisOutcome<inspection::ResourceInspection>,
    content: AnalysisOutcome<ContentFacts>,
}

impl ResourceFacts {
    /// Returns the resource position in this analysis snapshot.
    pub fn resource(&self) -> ResourceOrdinal {
        ResourceOrdinal::from_index(self.resource.0)
    }

    pub(crate) fn resource_row(&self) -> ResourceRow {
        self.resource
    }

    /// Returns semantic-format classification and its execution state.
    pub fn classification(&self) -> &AnalysisOutcome<ResourceClassification> {
        &self.classification
    }

    /// Returns the whole-resource BLAKE3 fingerprint outcome.
    pub fn fingerprint(&self) -> &AnalysisOutcome<Blake3Hash> {
        &self.fingerprint
    }

    /// Returns decoded image, media, font, or text metadata and its availability.
    pub fn inspection(&self) -> &AnalysisOutcome<inspection::ResourceInspection> {
        &self.inspection
    }

    /// Returns extracted semantic content and its execution state.
    pub fn content(&self) -> &AnalysisOutcome<ContentFacts> {
        &self.content
    }

    /// Classifies this physical resource for conservative foreground frame preparation.
    pub fn foreground_preparation_eligibility(&self) -> ForegroundPreparationEligibility {
        match &self.content {
            AnalysisOutcome::Complete(content) => {
                if let Some(facts) = content.as_xhtml() {
                    if !facts.foreground_preparation_document_supported {
                        ForegroundPreparationEligibility::Unknown
                    } else if facts.supports_foreground_preparation() {
                        ForegroundPreparationEligibility::Eligible
                    } else {
                        ForegroundPreparationEligibility::Ineligible
                    }
                } else if let Some(facts) = content.as_svg() {
                    if facts.supports_foreground_preparation() {
                        ForegroundPreparationEligibility::Eligible
                    } else {
                        ForegroundPreparationEligibility::Ineligible
                    }
                } else {
                    ForegroundPreparationEligibility::Ineligible
                }
            }
            AnalysisOutcome::Partial { .. } | AnalysisOutcome::Unavailable(_) => {
                ForegroundPreparationEligibility::Unknown
            }
            AnalysisOutcome::NotApplicable => match &self.classification {
                AnalysisOutcome::Complete(_) => ForegroundPreparationEligibility::Ineligible,
                AnalysisOutcome::Partial { .. }
                | AnalysisOutcome::Unavailable(_)
                | AnalysisOutcome::NotApplicable => ForegroundPreparationEligibility::Unknown,
            },
        }
    }

    pub(crate) fn content_mut(&mut self) -> &mut AnalysisOutcome<ContentFacts> {
        &mut self.content
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        resource: ResourceRow,
        classification: AnalysisOutcome<ResourceClassification>,
        fingerprint: AnalysisOutcome<Blake3Hash>,
        inspection: AnalysisOutcome<inspection::ResourceInspection>,
        content: AnalysisOutcome<ContentFacts>,
    ) -> Self {
        Self {
            resource,
            classification,
            fingerprint,
            inspection,
            content,
        }
    }
}

/// An owned snapshot of searchable content, resource details, links, and publication facts.
///
/// Use its query methods to connect extracted text and media to resource records, inspect broken
/// links and dependencies, and enumerate accessibility and media-overlay observations. The value
/// remains usable after an edit, but it continues to describe the publication version from which
/// it was built. Ordinals obtained from it must be interpreted against the same snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationAnalysis {
    limits: AnalysisLimits,
    resources: ResourceIndex,
    resource_facts: Vec<ResourceFacts>,
    resource_fact_slots: HashMap<ResourceRow, usize>,
    references: Vec<AuthoredReference>,
    media_overlay_references: HashMap<crate::resource::ManifestOrdinal, usize>,
    xhtml_references: HashMap<ResourceRow, XhtmlReferenceIndex>,
    media_overlays: MediaOverlayFacts,
    accessibility: AccessibilityFacts,
    coverage: Coverage,
    fingerprint_index: HashMap<Blake3Hash, Vec<ResourceRow>>,
    duplicate_fingerprints: Vec<Blake3Hash>,
}

impl PublicationAnalysis {
    /// Returns the operational limits applied to this analysis run.
    pub fn limits(&self) -> &AnalysisLimits {
        &self.limits
    }

    /// Returns the resources and package declarations represented by this snapshot.
    ///
    /// Ordinals from this index must be used only with queries on the same snapshot.
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    /// Iterates analysis results for every resource in resource-index order.
    pub fn resource_facts(&self) -> impl Iterator<Item = &ResourceFacts> {
        self.resource_facts.iter()
    }

    /// Returns facts for a resource position in this snapshot.
    pub fn facts_for(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<&ResourceFacts, OrdinalOutOfBounds> {
        self.facts_for_row(ResourceRow(ordinal.index()))
            .map_err(|_| OrdinalOutOfBounds)
    }

    pub(crate) fn facts_for_row(&self, key: ResourceRow) -> Result<&ResourceFacts, IndexRowError> {
        self.resources.validate_resource_row(key)?;
        self.resource_fact_slots
            .get(&key)
            .and_then(|slot| self.resource_facts.get(*slot))
            .ok_or(IndexRowError)
    }

    /// Returns the semantic content outcome for a snapshot resource.
    pub fn content_for(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<&AnalysisOutcome<ContentFacts>, OrdinalOutOfBounds> {
        self.facts_for(ordinal).map(ResourceFacts::content)
    }

    pub(crate) fn content_for_row(
        &self,
        key: ResourceRow,
    ) -> Result<&AnalysisOutcome<ContentFacts>, IndexRowError> {
        self.facts_for_row(key).map(ResourceFacts::content)
    }

    /// Returns the byte-inspection outcome for a snapshot resource.
    pub fn inspection_for(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<&AnalysisOutcome<inspection::ResourceInspection>, OrdinalOutOfBounds> {
        self.facts_for(ordinal).map(ResourceFacts::inspection)
    }

    /// Returns the XHTML text stream when complete or partial XHTML facts are available.
    pub fn text_stream_for(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<Option<&TextStream>, OrdinalOutOfBounds> {
        Ok(self
            .content_for(ordinal)?
            .value()
            .and_then(ContentFacts::as_xhtml)
            .map(XhtmlFacts::text_stream))
    }

    /// Returns analyzed media-overlay associations for this snapshot.
    pub fn media_overlays(&self) -> &MediaOverlayFacts {
        &self.media_overlays
    }

    /// Returns the policy-free accessibility projection for this snapshot.
    pub fn accessibility(&self) -> &AccessibilityFacts {
        &self.accessibility
    }

    /// Returns the resolved authored link for a certifier report from this snapshot.
    ///
    /// Returns `None` when `report` comes from another analysis or has no retained link
    /// reference.
    pub fn accessibility_certifier_report_reference(
        &self,
        report: &AccessibilityCertifierReport,
    ) -> Option<&HrefReference> {
        if !self
            .accessibility
            .metadata()
            .contains_certifier_report(report)
        {
            return None;
        }
        let slot = report.reference_slot()?;
        match self.references.get(slot.index())? {
            AuthoredReference::Href(reference) => Some(reference),
            AuthoredReference::Manifest(_) => None,
        }
    }

    /// Iterates accessibility-relevant navigation, content, structure, overlays, and media.
    ///
    /// The observations are factual inputs for application policy, not conformance verdicts.
    /// Consult [`Self::coverage`] before treating the set as complete.
    pub fn accessibility_observations(
        &self,
    ) -> impl Iterator<Item = AccessibilityObservationRef<'_>> {
        let navigation = self
            .accessibility
            .navigation_observations()
            .map(|observation| AccessibilityObservationRef::Navigation {
                observation,
                resource: observation
                    .resource_key()
                    .and_then(|key| self.resources.resource(key.into()).ok()),
            });
        let content = self.accessibility.content_occurrences().map(|occurrence| {
            AccessibilityObservationRef::Content {
                resource: self
                    .resources
                    .resource(occurrence.resource().into())
                    .expect("accessibility content resource must remain valid"),
                fact: occurrence.fact(),
            }
        });
        let resource_facts = || self.resources.resources().zip(&self.resource_facts);
        let structure = resource_facts().flat_map(|(resource, facts)| {
            let structure = facts
                .content()
                .value()
                .and_then(ContentFacts::as_xhtml)
                .map_or(&[][..], XhtmlFacts::structure);
            structure
                .iter()
                .map(move |fact| AccessibilityObservationRef::Structure { resource, fact })
        });
        let smil = resource_facts().filter_map(|(resource, facts)| {
            facts
                .content()
                .value()
                .and_then(ContentFacts::as_smil)
                .map(|facts| AccessibilityObservationRef::Smil { resource, facts })
        });
        let media_overlays = self
            .media_overlay_associations()
            .map(AccessibilityObservationRef::MediaOverlay);
        let media_tracks = resource_facts().flat_map(|(resource, facts)| {
            let tracks = match facts.inspection().value().map(|facts| facts.kind()) {
                Some(inspection::InspectionKind::Media(media)) => media.tracks(),
                _ => &[],
            };
            tracks
                .iter()
                .map(move |track| AccessibilityObservationRef::MediaTrack { resource, track })
        });
        let webvtt = resource_facts().filter_map(|(resource, facts)| {
            let inspection::InspectionKind::WebVtt(webvtt) = facts.inspection().value()?.kind()
            else {
                return None;
            };
            Some(AccessibilityObservationRef::WebVtt { resource, webvtt })
        });
        navigation
            .chain(content)
            .chain(structure)
            .chain(smil)
            .chain(media_overlays)
            .chain(media_tracks)
            .chain(webvtt)
    }

    /// Returns root playback nodes and their text and audio references for a SMIL resource.
    ///
    /// Returns `Ok(None)` when no complete or partial SMIL facts are available.
    pub fn smil_roots_for(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<Option<impl Iterator<Item = SmilNodeRef<'_>>>, OrdinalOutOfBounds> {
        let resource = ResourceRow(ordinal.index());
        let facts = self
            .content_for_row(resource)
            .map_err(|_| OrdinalOutOfBounds)?
            .value()
            .and_then(ContentFacts::as_smil);
        Ok(facts.map(move |facts| {
            facts.roots().iter().filter_map(move |node| {
                SmilNodeRef::new(resource.into(), *node, facts, &self.references)
            })
        }))
    }

    /// Returns execution coverage for all analysis producer families.
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    pub(crate) fn new(
        limits: AnalysisLimits,
        resources: ResourceIndex,
        resource_facts: Vec<ResourceFacts>,
        references: Vec<AuthoredReference>,
        xhtml_references: HashMap<ResourceRow, XhtmlReferenceIndex>,
        media_overlays: MediaOverlayFacts,
        accessibility: AccessibilityFacts,
        coverage: Coverage,
    ) -> Self {
        assert_eq!(resources.len(), resource_facts.len());
        assert!(
            resources
                .resources()
                .zip(&resource_facts)
                .all(|(resource, facts)| resource.key() == facts.resource_row())
        );
        let resource_fact_slots = resource_facts
            .iter()
            .enumerate()
            .map(|(slot, facts)| (facts.resource_row(), slot))
            .collect();
        let media_overlay_references = references
            .iter()
            .enumerate()
            .filter_map(|(index, reference)| match reference {
                AuthoredReference::Manifest(reference)
                    if reference.role() == reference::ManifestRole::MediaOverlay =>
                {
                    Some((reference.source(), index))
                }
                AuthoredReference::Href(_) | AuthoredReference::Manifest(_) => None,
            })
            .collect();
        let mut fingerprint_index = HashMap::<Blake3Hash, Vec<ResourceRow>>::new();
        for facts in &resource_facts {
            if let AnalysisOutcome::Complete(hash) = facts.fingerprint() {
                fingerprint_index
                    .entry(*hash)
                    .or_default()
                    .push(facts.resource_row());
            }
        }
        let mut seen_duplicate_fingerprints = HashSet::new();
        let duplicate_fingerprints = resource_facts
            .iter()
            .filter_map(|facts| match facts.fingerprint() {
                AnalysisOutcome::Complete(hash) => Some(*hash),
                AnalysisOutcome::NotApplicable
                | AnalysisOutcome::Partial { .. }
                | AnalysisOutcome::Unavailable(_) => None,
            })
            .filter(|hash| {
                fingerprint_index
                    .get(hash)
                    .is_some_and(|keys| keys.len() > 1)
            })
            .filter(|hash| seen_duplicate_fingerprints.insert(*hash))
            .collect();
        Self {
            limits,
            resources,
            resource_facts,
            resource_fact_slots,
            references,
            media_overlay_references,
            xhtml_references,
            media_overlays,
            accessibility,
            coverage,
            fingerprint_index,
            duplicate_fingerprints,
        }
    }
}
