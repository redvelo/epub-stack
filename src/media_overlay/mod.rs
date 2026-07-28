//! Synchronized text-and-audio playback, SMIL documents, and package overlay metadata.
//!
//! Applications can read package durations and narrators, connect reading-order content to its
//! SMIL resource, walk playback sequences and parallels, and resolve each text or audio node to
//! its authored target. Missing and ambiguous overlay relationships remain visible through
//! [`MediaOverlayAssociationRef::reference`].
//!
//! Standalone [`SmilDocument`] values are normalized models; serialization does not promise
//! byte-for-byte output or preservation of unknown XML. Default parsing limits are 100,000 nodes
//! and 256 nested elements. Analyzed facts and associations belong to one publication-analysis
//! snapshot and continue to describe that version after the publication is edited.

mod facts;
pub(crate) mod smil;

use crate::analysis::ResourceFacts;
use crate::analysis::reference::{
    AuthoredReference, HrefReference, HrefRole, ManifestReference, ReferenceSlot,
};
use crate::package::{Package, metadata::Meta};
use crate::resource::{
    ManifestDeclarationRef, ManifestOrdinal, ReadingOrderOccurrenceRef, ResourceIndex,
    ResourceOrdinal, ResourceRef,
};

pub(crate) use facts::parse_media_time;
pub use facts::{MediaTime, SmilFacts, SmilNodeFact, SmilNodeId, SmilTime};
pub use smil::{
    SmilAudio, SmilBody, SmilDocument, SmilError, SmilHead, SmilMeta, SmilPar, SmilParallelChild,
    SmilSeq, SmilSequenceChild, SmilText,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Package-level media-overlay presence and authored playback metadata.
pub struct MediaOverlayFacts {
    present: bool,
    metadata: MediaOverlayMetadata,
}

impl MediaOverlayFacts {
    pub(crate) fn build(package: &Package, resources: &ResourceIndex) -> Self {
        Self {
            present: resources.declarations().any(|declaration| {
                declaration.media_overlay().is_some()
                    || declaration
                        .media_type()
                        .is_some_and(crate::resource::MediaType::is_smil)
            }),
            metadata: MediaOverlayMetadata::from_package(package, resources),
        }
    }

    /// Returns whether the package declares an overlay relationship or SMIL resource.
    pub fn present(&self) -> bool {
        self.present
    }

    /// Returns retained package media-overlay metadata.
    pub fn metadata(&self) -> &MediaOverlayMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Authored durations, playback classes, and narrators from package metadata.
pub struct MediaOverlayMetadata {
    total_durations: Vec<MediaOverlayDurationMeta>,
    item_durations: Vec<MediaOverlayDurationMeta>,
    active_classes: Vec<String>,
    playback_active_classes: Vec<String>,
    narrators: Vec<String>,
}

impl MediaOverlayMetadata {
    fn from_package(package: &Package, resources: &ResourceIndex) -> Self {
        let durations = package
            .metadata()
            .meta()
            .iter()
            .filter(|meta| is_property(meta, "duration"))
            .map(|meta| duration_meta(meta, resources));
        let (item_durations, total_durations) =
            durations.partition(|duration| duration.refines.is_some());
        Self {
            total_durations,
            item_durations,
            active_classes: collect_meta_values(package, "active-class"),
            playback_active_classes: collect_meta_values(package, "playback-active-class"),
            narrators: collect_meta_values(package, "narrator"),
        }
    }

    /// Returns unrefined total-duration values in authored order.
    pub fn total_durations(&self) -> &[MediaOverlayDurationMeta] {
        &self.total_durations
    }

    /// Returns duration values that refine manifest declarations.
    pub fn item_durations(&self) -> &[MediaOverlayDurationMeta] {
        &self.item_durations
    }

    /// Returns authored active-class values.
    pub fn active_classes(&self) -> &[String] {
        &self.active_classes
    }

    /// Returns authored playback-active-class values.
    pub fn playback_active_classes(&self) -> &[String] {
        &self.playback_active_classes
    }

    /// Returns authored narrator values.
    pub fn narrators(&self) -> &[String] {
        &self.narrators
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One authored media-overlay duration and its optional parsed time and refinement targets.
pub struct MediaOverlayDurationMeta {
    authored: String,
    parsed: Option<MediaTime>,
    refines: Option<String>,
    targets: Vec<ManifestOrdinal>,
}

impl MediaOverlayDurationMeta {
    /// Returns the exact authored duration text.
    pub fn authored(&self) -> &str {
        &self.authored
    }

    /// Returns the parsed millisecond value when recognized.
    pub fn parsed(&self) -> Option<MediaTime> {
        self.parsed
    }

    /// Returns the authored `refines` value.
    pub fn refines(&self) -> Option<&str> {
        self.refines.as_deref()
    }

    /// Returns every declaration in this analysis matching the refinement ID.
    pub fn targets(&self) -> &[ManifestOrdinal] {
        &self.targets
    }
}

/// A reading-order entry with its content resource, overlay relationship, and SMIL facts.
///
/// The manifest relationship retains missing and ambiguous targets. Overlay declarations,
/// resources, and SMIL facts are present only when the relationship resolves far enough to
/// identify them. All references belong to one [`crate::PublicationAnalysis`] snapshot.
#[derive(Debug, Clone, Copy)]
pub struct MediaOverlayAssociationRef<'a> {
    reading_order: ReadingOrderOccurrenceRef<'a>,
    content_declaration: ManifestDeclarationRef<'a>,
    content_resource: Option<ResourceRef<'a>>,
    reference: &'a ManifestReference,
    overlay_declaration: Option<ManifestDeclarationRef<'a>>,
    overlay_resource: Option<ResourceRef<'a>>,
    overlay_resource_facts: Option<&'a ResourceFacts>,
    references: &'a [AuthoredReference],
}

impl<'a> MediaOverlayAssociationRef<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        reading_order: ReadingOrderOccurrenceRef<'a>,
        content_declaration: ManifestDeclarationRef<'a>,
        content_resource: Option<ResourceRef<'a>>,
        reference: &'a ManifestReference,
        overlay_declaration: Option<ManifestDeclarationRef<'a>>,
        overlay_resource: Option<ResourceRef<'a>>,
        overlay_resource_facts: Option<&'a ResourceFacts>,
        references: &'a [AuthoredReference],
    ) -> Self {
        Self {
            reading_order,
            content_declaration,
            content_resource,
            reference,
            overlay_declaration,
            overlay_resource,
            overlay_resource_facts,
            references,
        }
    }

    /// Returns the exact reading-order occurrence carrying this association.
    pub fn reading_order(self) -> ReadingOrderOccurrenceRef<'a> {
        self.reading_order
    }

    /// Returns the content declaration that authored `media-overlay`.
    pub fn content_declaration(self) -> ManifestDeclarationRef<'a> {
        self.content_declaration
    }

    /// Returns the content resource when the reading-order declaration resolves to one.
    pub fn content_resource(self) -> Option<ResourceRef<'a>> {
        self.content_resource
    }

    /// Returns the authored manifest relationship, including its target state.
    pub fn reference(self) -> &'a ManifestReference {
        self.reference
    }

    /// Returns the uniquely resolved overlay declaration.
    pub fn overlay_declaration(self) -> Option<ManifestDeclarationRef<'a>> {
        self.overlay_declaration
    }

    /// Returns the overlay resource when the target declaration resolves to one.
    pub fn overlay_resource(self) -> Option<ResourceRef<'a>> {
        self.overlay_resource
    }

    /// Returns all analysis outcomes for the resolved overlay resource.
    pub fn overlay_resource_facts(self) -> Option<&'a ResourceFacts> {
        self.overlay_resource_facts
    }

    /// Returns complete or partial SMIL facts for the resolved overlay resource.
    pub fn smil_facts(self) -> Option<&'a SmilFacts> {
        self.overlay_resource_facts?.content().value()?.as_smil()
    }

    /// Iterates root playback nodes with their text and audio references.
    pub fn roots(self) -> impl Iterator<Item = SmilNodeRef<'a>> {
        let resource = self.overlay_resource.map(ResourceRef::ordinal);
        self.smil_facts().into_iter().flat_map(move |facts| {
            facts
                .roots()
                .iter()
                .filter_map(move |node| SmilNodeRef::new(resource?, *node, facts, self.references))
        })
    }
}

/// A SMIL playback node with access to its children and authored text or audio link.
#[derive(Clone, Copy)]
pub struct SmilNodeRef<'a> {
    resource: ResourceOrdinal,
    node: SmilNodeId,
    facts: &'a SmilFacts,
    references: &'a [AuthoredReference],
}

impl std::fmt::Debug for SmilNodeRef<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmilNodeRef")
            .field("resource", &self.resource)
            .field("node", &self.node)
            .field("fact", &self.fact())
            .finish()
    }
}

impl<'a> SmilNodeRef<'a> {
    pub(crate) fn new(
        resource: ResourceOrdinal,
        node: SmilNodeId,
        facts: &'a SmilFacts,
        references: &'a [AuthoredReference],
    ) -> Option<Self> {
        facts.node(node)?;
        Some(Self {
            resource,
            node,
            facts,
            references,
        })
    }

    /// Returns the extracted playback-node fact.
    pub fn fact(self) -> &'a SmilNodeFact {
        self.facts
            .node(self.node)
            .expect("SMIL node view always contains a valid node")
    }

    /// Iterates the node's children in authored order.
    pub fn children(self) -> impl Iterator<Item = Self> + 'a {
        self.fact()
            .children()
            .iter()
            .filter_map(move |node| Self::new(self.resource, *node, self.facts, self.references))
    }

    /// Returns the authored text link when this node has one.
    pub fn text_reference(self) -> Option<&'a HrefReference> {
        self.reference(
            self.facts.text_reference_slot(self.node),
            HrefRole::SmilText,
        )
    }

    /// Returns the authored audio link when this node has one.
    pub fn audio_reference(self) -> Option<&'a HrefReference> {
        self.reference(
            self.facts.audio_reference_slot(self.node),
            HrefRole::SmilAudio,
        )
    }

    fn reference(self, slot: Option<ReferenceSlot>, role: HrefRole) -> Option<&'a HrefReference> {
        match slot.and_then(|slot| self.references.get(slot.index())) {
            Some(AuthoredReference::Href(reference))
                if reference.source() == self.resource && reference.role() == role =>
            {
                Some(reference)
            }
            _ => None,
        }
    }
}

fn duration_meta(meta: &Meta, resources: &ResourceIndex) -> MediaOverlayDurationMeta {
    let authored = meta.content().map(ToString::to_string).unwrap_or_default();
    let refines = meta.refines().map(ToString::to_string);
    let targets = refines
        .as_deref()
        .and_then(refinement_id)
        .into_iter()
        .flat_map(|id| resources.declarations_with_id(id).ok())
        .flatten()
        .map(crate::resource::ManifestDeclarationRef::ordinal)
        .collect();
    MediaOverlayDurationMeta {
        parsed: parse_media_time(&authored),
        authored,
        refines,
        targets,
    }
}

fn collect_meta_values(package: &Package, property: &str) -> Vec<String> {
    package
        .metadata()
        .meta()
        .iter()
        .filter(|meta| is_property(meta, property))
        .map(|meta| meta.content().map(ToString::to_string).unwrap_or_default())
        .collect()
}

fn refinement_id(value: &str) -> Option<&str> {
    value
        .strip_prefix('#')
        .filter(|id| !id.is_empty() && !id.starts_with('#'))
}

fn is_property(meta: &Meta, property: &str) -> bool {
    meta.property().is_some_and(|value| {
        let value = value.as_str().to_ascii_lowercase();
        value == property || value.strip_prefix("media:") == Some(property)
    })
}
