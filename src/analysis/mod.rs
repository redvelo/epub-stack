//! Extracted content, resolved links, byte inspection, and edit impact.
//!
//! Start with [`PublicationAnalysis::analyzed_resources`]; each [`ResourceAnalysisRef`] connects
//! a resource's content and inspection results to its inventory record and outgoing references.
//!
//! ```no_run
//! use epub_stack::EpubZip;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("book.epub")?.default_rendition()?;
//! let analysis = book.analyze();
//! for resource in analysis.analyzed_resources() {
//!     println!("{}", resource.resource().address().display_value());
//!     if let Some(content) = resource.content().value() {
//!         println!("{:?}", content);
//!     }
//!     if let Some(inspection) = resource.inspection().value() {
//!         println!("{:?}", inspection.data());
//!     }
//!     for reference in resource.references() {
//!         println!("{:?}: {:?}", reference.role(), reference.target());
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`AnalysisOutcome::value`] borrows available results, including partial results.
//! [`PublicationAnalysis::coverage`] reports which expected results are complete, partial, or
//! unavailable because of limits, missing bytes, or unsupported formats.
//!
//! Analysis reads resources subject to [`AnalysisLimits`]. Results and ordinals belong to
//! that snapshot; rerun analysis after edits for current results.

pub mod coverage;
pub mod dependency;
pub mod fingerprint;
pub mod impact;
pub mod inspection;
pub(crate) mod orchestration;
pub mod reference;
mod relationships;

use crate::accessibility::{
    AccessibilityCertifierReport, AccessibilityFacts, AccessibilityObservationRef,
};
use crate::content::{ContentFacts, XhtmlFacts};
use crate::media_overlay::{MediaOverlayFacts, SmilNodeRef};
use crate::resource::{
    ManifestDeclarationRef, ManifestOrdinal, ResourceIndex, ResourceOrdinal, ResourceRef,
};
use coverage::{Coverage, RelationshipCoverage, ResourceCompleteness};
use fingerprint::Blake3Hash;
use reference::{AuthoredReference, HrefReference, ManifestReference, XhtmlReferenceIndex};
use std::collections::{HashMap, HashSet};

/// How much work analysis may do before it stops: how many resources, how many bytes, how deep
/// a SMIL tree.
///
/// `None` means no limit. Whatever a budget cuts short is reported as incomplete, never
/// silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AnalysisLimits {
    /// Maximum number of resources selected for byte analysis.
    pub max_analyzed_resources: Option<usize>,
    /// Maximum analyzed bytes for any one resource.
    pub max_resource_analysis_bytes: Option<u64>,
    /// Aggregate byte budget for content analysis and inspection.
    pub max_total_analysis_bytes: Option<u64>,
    /// Aggregate byte budget for fingerprinting.
    pub max_total_fingerprint_bytes: Option<u64>,
    /// Maximum number of XML nodes accepted in one SMIL document.
    pub max_smil_nodes: Option<usize>,
    /// Maximum element nesting depth accepted in one SMIL document.
    pub max_smil_nesting: Option<usize>,
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

/// Whether an answer is whole, partial, or missing.
///
/// Complete means analysis finished, not that the book is sound: a complete result can report
/// plenty of broken links.
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
    /// The run reached one of its [`AnalysisLimits`].
    Limit(AnalysisLimit),
    /// Expected local bytes were absent.
    Missing,
    /// The provider could not read the resource bytes.
    Unreadable,
    /// The resource format or operation is not supported.
    Unsupported,
    /// The bytes do not conform to their format; any partial result was recovered before or
    /// around the malformation.
    Malformed,
}

/// One [`AnalysisLimits`] budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalysisLimit {
    /// [`AnalysisLimits::max_analyzed_resources`].
    AnalyzedResources,
    /// [`AnalysisLimits::max_resource_analysis_bytes`].
    ResourceAnalysisBytes,
    /// [`AnalysisLimits::max_total_analysis_bytes`].
    TotalAnalysisBytes,
    /// [`AnalysisLimits::max_total_fingerprint_bytes`].
    TotalFingerprintBytes,
    /// [`AnalysisLimits::max_smil_nodes`].
    SmilNodes,
    /// [`AnalysisLimits::max_smil_nesting`].
    SmilNesting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SemanticFormat {
    Xhtml,
    Css,
    Svg,
    Smil,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceClassification {
    Unknown,
    Identified(SemanticFormat),
    Conflict(Vec<SemanticFormat>),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceAnalysis {
    resource: ResourceOrdinal,
    fingerprint: AnalysisOutcome<Blake3Hash>,
    inspection: AnalysisOutcome<inspection::ResourceInspection>,
    content: AnalysisOutcome<ContentFacts>,
}

impl ResourceAnalysis {
    pub(crate) fn resource(&self) -> ResourceOrdinal {
        self.resource
    }

    pub(crate) fn fingerprint(&self) -> &AnalysisOutcome<Blake3Hash> {
        &self.fingerprint
    }

    pub(crate) fn inspection(&self) -> &AnalysisOutcome<inspection::ResourceInspection> {
        &self.inspection
    }

    pub(crate) fn content(&self) -> &AnalysisOutcome<ContentFacts> {
        &self.content
    }

    pub(crate) fn content_mut(&mut self) -> &mut AnalysisOutcome<ContentFacts> {
        &mut self.content
    }

    pub(crate) fn new(
        resource: ResourceOrdinal,
        fingerprint: AnalysisOutcome<Blake3Hash>,
        inspection: AnalysisOutcome<inspection::ResourceInspection>,
        content: AnalysisOutcome<ContentFacts>,
    ) -> Self {
        Self {
            resource,
            fingerprint,
            inspection,
            content,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ReferenceIndex {
    from: Vec<usize>,
    to: Vec<usize>,
}

/// What a publication's resources turned out to contain: their text, their formats, and the
/// links between them.
///
/// These are the answers from one moment. Edit the book and they describe the version that was
/// analyzed, not the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationAnalysis {
    limits: AnalysisLimits,
    resources: ResourceIndex,
    resource_facts: Vec<ResourceAnalysis>,
    references: Vec<AuthoredReference>,
    resource_references: Vec<ReferenceIndex>,
    declaration_references: Vec<ReferenceIndex>,
    xhtml_references: HashMap<ResourceOrdinal, XhtmlReferenceIndex>,
    media_overlays: MediaOverlayFacts,
    accessibility: AccessibilityFacts,
    relationship_coverage: Vec<RelationshipCoverage>,
    fragment_coverage: Vec<ResourceCompleteness>,
    fingerprint_index: HashMap<Blake3Hash, Vec<ResourceOrdinal>>,
    duplicate_fingerprints: Vec<Blake3Hash>,
}

impl PublicationAnalysis {
    /// Every resource that was analyzed.
    pub fn analyzed_resources(&self) -> impl ExactSizeIterator<Item = ResourceAnalysisRef<'_>> {
        self.resource_facts.iter().map(|facts| ResourceAnalysisRef {
            analysis: self,
            facts,
        })
    }

    /// The analysis of one resource, or `None` if the position is past the end.
    ///
    /// Positions belong to the analysis that produced them. Once you hold a handle, everything
    /// you ask it answers without failing.
    pub fn resource(&self, ordinal: ResourceOrdinal) -> Option<ResourceAnalysisRef<'_>> {
        self.resource_facts
            .get(ordinal.index())
            .map(|facts| ResourceAnalysisRef {
                analysis: self,
                facts,
            })
    }

    /// The analysis of one declaration, or `None` if the position is past the end.
    ///
    /// Positions belong to the analysis that produced them.
    pub fn declaration(&self, ordinal: ManifestOrdinal) -> Option<DeclarationAnalysisRef<'_>> {
        self.resources
            .declaration(ordinal)
            .map(|declaration| DeclarationAnalysisRef {
                analysis: self,
                declaration,
            })
    }

    /// Returns the operational limits applied to this analysis run.
    pub fn limits(&self) -> &AnalysisLimits {
        &self.limits
    }

    /// What the analyzed publication is made of.
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    pub(crate) fn resource_analysis(&self, ordinal: ResourceOrdinal) -> ResourceAnalysisRef<'_> {
        self.resource(ordinal)
            .expect("analysis resources are aligned with the resource index")
    }

    pub(crate) fn references_at<'a>(
        &'a self,
        slots: &'a [usize],
    ) -> impl Iterator<Item = &'a AuthoredReference> + 'a {
        slots.iter().map(|slot| &self.references[*slot])
    }

    /// Which documents have narration, and what drives it.
    pub fn media_overlays(&self) -> &MediaOverlayFacts {
        &self.media_overlays
    }

    /// What the book states and shows about its own accessibility.
    pub fn accessibility(&self) -> &AccessibilityFacts {
        &self.accessibility
    }

    /// Where the accessibility certifier's report points, if the book names one.
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
                    .and_then(|key| self.resources.resource(key)),
            });
        let content = self.accessibility.content_occurrences().map(|occurrence| {
            AccessibilityObservationRef::Content {
                resource: self
                    .resources
                    .resource(occurrence.resource())
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
            let tracks = match facts.inspection().value().map(|facts| facts.data()) {
                Some(inspection::InspectionData::Media(media)) => media.tracks(),
                _ => &[],
            };
            tracks
                .iter()
                .map(move |track| AccessibilityObservationRef::MediaTrack { resource, track })
        });
        let webvtt = resource_facts().filter_map(|(resource, facts)| {
            let inspection::InspectionData::WebVtt(webvtt) = facts.inspection().value()?.data()
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

    /// How much of the book analysis actually got through, and what it could not finish.
    ///
    /// Check this before reading a negative answer as fact: "no broken links" means less when
    /// half the resources could not be read.
    pub fn coverage(&self) -> Coverage<'_> {
        Coverage::new(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        limits: AnalysisLimits,
        resources: ResourceIndex,
        resource_facts: Vec<ResourceAnalysis>,
        references: Vec<AuthoredReference>,
        xhtml_references: HashMap<ResourceOrdinal, XhtmlReferenceIndex>,
        media_overlays: MediaOverlayFacts,
        accessibility: AccessibilityFacts,
        relationship_coverage: Vec<RelationshipCoverage>,
        fragment_coverage: Vec<ResourceCompleteness>,
    ) -> Self {
        debug_assert_eq!(
            relationship_coverage
                .iter()
                .map(|coverage| coverage.source)
                .collect::<HashSet<_>>()
                .len(),
            relationship_coverage.len()
        );
        debug_assert_eq!(resources.resources().len(), resource_facts.len());
        debug_assert!(
            resources
                .resources()
                .zip(&resource_facts)
                .all(|(resource, facts)| resource.ordinal() == facts.resource())
        );
        let mut resource_references = vec![ReferenceIndex::default(); resources.resources().len()];
        let mut declaration_references =
            vec![ReferenceIndex::default(); resources.declarations().len()];
        for (slot, reference) in references.iter().enumerate() {
            match reference {
                AuthoredReference::Href(reference) => {
                    resource_references[reference.source().index()]
                        .from
                        .push(slot);
                    let target = match reference.target() {
                        reference::HrefTarget::Resource { resource, .. }
                        | reference::HrefTarget::Fragment { resource, .. } => Some(*resource),
                        reference::HrefTarget::Remote {
                            declared_resource, ..
                        } => *declared_resource,
                        reference::HrefTarget::Data(_)
                        | reference::HrefTarget::External(_)
                        | reference::HrefTarget::MissingLocal(_)
                        | reference::HrefTarget::Invalid(_) => None,
                    };
                    if let Some(target) = target {
                        resource_references[target.index()].to.push(slot);
                    }
                }
                AuthoredReference::Manifest(reference) => {
                    declaration_references[reference.source().index()]
                        .from
                        .push(slot);
                    if let reference::ManifestTarget::Declaration {
                        declaration,
                        resource,
                    } = reference.target()
                    {
                        declaration_references[declaration.index()].to.push(slot);
                        if let Some(resource) = resource {
                            resource_references[resource.index()].to.push(slot);
                        }
                    }
                }
            }
        }
        let mut fingerprint_index = HashMap::<Blake3Hash, Vec<ResourceOrdinal>>::new();
        for facts in &resource_facts {
            if let AnalysisOutcome::Complete(hash) = facts.fingerprint() {
                fingerprint_index
                    .entry(*hash)
                    .or_default()
                    .push(facts.resource());
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
            references,
            resource_references,
            declaration_references,
            xhtml_references,
            media_overlays,
            accessibility,
            relationship_coverage,
            fragment_coverage,
            fingerprint_index,
            duplicate_fingerprints,
        }
    }
}

/// What one resource contains, and the links into and out of it.
#[derive(Debug, Clone, Copy)]
pub struct ResourceAnalysisRef<'a> {
    analysis: &'a PublicationAnalysis,
    facts: &'a ResourceAnalysis,
}

impl<'a> ResourceAnalysisRef<'a> {
    /// The resource this describes: its address, presence and declarations.
    pub fn resource(self) -> ResourceRef<'a> {
        self.analysis
            .resources
            .resource(self.facts.resource())
            .expect("analysis resources are aligned")
    }

    /// The text, structure and links extracted from this resource.
    ///
    /// Only XHTML, SVG, SMIL and CSS carry content; for an image or a font it does not apply.
    /// It is unavailable, rather than absent, when the resource could not be read or its format
    /// could not be determined.
    pub fn content(self) -> &'a AnalysisOutcome<ContentFacts> {
        self.facts.content()
    }

    /// What the bytes themselves say: image dimensions, media duration, font names.
    pub fn inspection(self) -> &'a AnalysisOutcome<inspection::ResourceInspection> {
        self.facts.inspection()
    }

    /// The BLAKE3 hash of the whole resource, for spotting duplicates.
    pub fn fingerprint(self) -> &'a AnalysisOutcome<Blake3Hash> {
        self.facts.fingerprint()
    }

    /// Every link written in this resource, and where each one resolves.
    pub fn references(self) -> impl Iterator<Item = &'a HrefReference> + 'a {
        self.analysis
            .references_at(&self.reference_index().from)
            .filter_map(|reference| match reference {
                AuthoredReference::Href(reference) => Some(reference),
                AuthoredReference::Manifest(_) => None,
            })
    }

    /// Every link elsewhere in the book that points at this resource.
    ///
    /// Use it to ask who depends on a file before removing or moving it.
    pub fn incoming_references(self) -> impl Iterator<Item = &'a AuthoredReference> + 'a {
        self.analysis.references_at(&self.reference_index().to)
    }

    /// The images, audio and video this document embeds, with the links they were written as.
    ///
    /// Empty for anything that is not readable XHTML; [`Self::content`] says which.
    pub fn xhtml_media(self) -> impl Iterator<Item = reference::XhtmlMediaOccurrence<'a>> + 'a {
        let resource = self.facts.resource();
        let index = self.analysis.xhtml_references.get(&resource);
        self.xhtml()
            .into_iter()
            .flat_map(|facts| facts.media().iter().enumerate())
            .map(move |(position, fact)| {
                let slots = index
                    .and_then(|index| index.media.get(position))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                reference::XhtmlMediaOccurrence::new(
                    fact,
                    resource,
                    slots,
                    &self.analysis.references,
                )
            })
    }

    /// The forms and controls this document declares.
    ///
    /// Empty for anything that is not readable XHTML; [`Self::content`] says which.
    pub fn xhtml_forms(self) -> impl Iterator<Item = reference::XhtmlFormOccurrence<'a>> + 'a {
        let index = self.analysis.xhtml_references.get(&self.facts.resource());
        self.xhtml()
            .into_iter()
            .flat_map(|facts| facts.forms().iter().enumerate())
            .map(move |(position, fact)| {
                let slot = index
                    .and_then(|index| index.forms.get(position))
                    .copied()
                    .flatten();
                reference::XhtmlFormOccurrence::new(fact, slot.and_then(|slot| self.href(slot)))
            })
    }

    /// The scripts this document carries, inline or linked.
    ///
    /// Empty for anything that is not readable XHTML; [`Self::content`] says which.
    pub fn xhtml_scripts(self) -> impl Iterator<Item = reference::XhtmlScriptOccurrence<'a>> + 'a {
        let index = self.analysis.xhtml_references.get(&self.facts.resource());
        self.xhtml()
            .into_iter()
            .flat_map(|facts| facts.scripts().iter().enumerate())
            .map(move |(position, fact)| {
                let slot = index
                    .and_then(|index| index.scripts.get(position))
                    .copied()
                    .flatten();
                reference::XhtmlScriptOccurrence::new(fact, slot.and_then(|slot| self.href(slot)))
            })
    }

    /// The narration this overlay drives, pairing each passage of text with its audio.
    ///
    /// Empty for anything that is not readable SMIL; [`Self::content`] says which.
    pub fn smil_roots(self) -> impl Iterator<Item = SmilNodeRef<'a>> + 'a {
        let resource = self.facts.resource();
        let references = &self.analysis.references;
        self.content()
            .value()
            .and_then(ContentFacts::as_smil)
            .into_iter()
            .flat_map(move |facts| {
                facts
                    .roots()
                    .iter()
                    .filter_map(move |node| SmilNodeRef::new(resource, *node, facts, references))
            })
    }

    fn xhtml(self) -> Option<&'a XhtmlFacts> {
        self.content().value().and_then(ContentFacts::as_xhtml)
    }

    fn href(self, slot: reference::ReferenceSlot) -> Option<&'a HrefReference> {
        match self.analysis.references.get(slot.index()) {
            Some(AuthoredReference::Href(reference))
                if reference.source() == self.facts.resource() =>
            {
                Some(reference)
            }
            _ => None,
        }
    }

    fn reference_index(self) -> &'a ReferenceIndex {
        &self.analysis.resource_references[self.facts.resource().index()]
    }
}

/// One manifest declaration, and the `fallback` and `media-overlay` links around it.
#[derive(Debug, Clone, Copy)]
pub struct DeclarationAnalysisRef<'a> {
    analysis: &'a PublicationAnalysis,
    declaration: ManifestDeclarationRef<'a>,
}

impl<'a> DeclarationAnalysisRef<'a> {
    /// The declaration this describes.
    pub fn declaration(self) -> ManifestDeclarationRef<'a> {
        self.declaration
    }

    /// The analysis of the file this declaration points at, when its href resolves.
    pub fn resource(self) -> Option<ResourceAnalysisRef<'a>> {
        let resource = self.declaration.resource()?;
        Some(self.analysis.resource_analysis(resource.ordinal()))
    }

    /// The `fallback` and `media-overlay` links this declaration writes.
    pub fn references(self) -> impl Iterator<Item = &'a ManifestReference> + 'a {
        self.manifest_references(&self.reference_index().from)
    }

    /// Iterates manifest relationships that resolve to this declaration.
    ///
    /// Missing and ambiguous ID references do not match a declaration.
    pub fn incoming_references(self) -> impl Iterator<Item = &'a ManifestReference> + 'a {
        self.manifest_references(&self.reference_index().to)
    }

    fn manifest_references(
        self,
        slots: &'a [usize],
    ) -> impl Iterator<Item = &'a ManifestReference> + 'a {
        self.analysis
            .references_at(slots)
            .filter_map(|reference| match reference {
                AuthoredReference::Manifest(reference) => Some(reference),
                AuthoredReference::Href(_) => None,
            })
    }

    fn reference_index(self) -> &'a ReferenceIndex {
        &self.analysis.declaration_references[self.declaration.ordinal().index()]
    }
}
