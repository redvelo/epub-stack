//! Accessibility metadata, conformance claims, and source observations for application policy.
//!
//! Applications can inspect declared access modes, features, hazards, summaries, certification,
//! and conformance claims through [`AccessibilityFacts`].
//! [`crate::PublicationAnalysis::accessibility_observations`] also brings together navigation,
//! alternatives and ARIA attributes, document structure, media overlays, media tracks, and
//! WebVTT details with their source resources.
//!
//! These models report authored declarations and observed source facts; they do not validate the
//! EPUB or decide accessibility conformance. Consult [`crate::analysis::coverage::Coverage`] to
//! determine whether source extraction and media inspection were complete. Values belong to one
//! analysis snapshot and are not a lossless XML representation.

mod facts;
mod metadata;

use crate::analysis::ResourceFacts;
use crate::analysis::inspection::{MediaTrack, WebVtt};
use crate::analysis::reference::ReferenceSlot;
use crate::content::StructureFact;
use crate::media_overlay::{MediaOverlayAssociationRef, SmilFacts};
use crate::navigation::{Navigation, NavigationDocument, NavigationPoint, NavigationSource};
use crate::package::{
    Package,
    metadata::{Meta, MetadataLink},
};
use crate::resource::{ResourceAddress, ResourceIndex, ResourceRef, ResourceRow};
use std::collections::HashMap;

use metadata::collect_metadata;

pub use facts::{
    AccessibilityElementFact, AccessibilityFact, AccessibilityHeadingLevelFact,
    AccessibilityValueFact, SvgAccessibilityTextFact,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Declared accessibility metadata, conformance claims, and extracted source observations.
pub struct AccessibilityFacts {
    metadata: AccessibilityMetadata,
    claims: Vec<AccessibilityClaim>,
    navigation: Vec<AccessibilityNavigationObservation>,
    content: Vec<AccessibilityContentOccurrence>,
}

impl AccessibilityFacts {
    /// Returns authored accessibility metadata projected from the package document.
    pub fn metadata(&self) -> &AccessibilityMetadata {
        &self.metadata
    }

    /// Iterates authored conformance claims, including claims that could not be interpreted.
    pub fn claims(&self) -> impl Iterator<Item = &AccessibilityClaim> {
        self.claims.iter()
    }

    pub(crate) fn navigation_observations(
        &self,
    ) -> impl Iterator<Item = &AccessibilityNavigationObservation> {
        self.navigation.iter()
    }

    pub(crate) fn content_occurrences(
        &self,
    ) -> impl Iterator<Item = &AccessibilityContentOccurrence> {
        self.content.iter()
    }

    pub(crate) fn build(
        package: &Package,
        navigation: &Navigation,
        secondary_navigation: &[NavigationDocument],
        resources: &ResourceIndex,
        resource_facts: &[ResourceFacts],
        content_occurrences: Vec<(ResourceRow, AccessibilityFact)>,
        package_link_references: &[Option<ReferenceSlot>],
    ) -> Self {
        let (metadata, claims) = collect_metadata(package, package_link_references);
        let navigation = collect_navigation(navigation, secondary_navigation, resources);
        let content = collect_content(resource_facts, content_occurrences);
        Self {
            metadata,
            claims,
            navigation,
            content,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessibilityContentOccurrence {
    resource: ResourceRow,
    fact: AccessibilityFact,
}

impl AccessibilityContentOccurrence {
    pub(crate) fn resource(&self) -> ResourceRow {
        self.resource
    }

    pub(crate) fn fact(&self) -> &AccessibilityFact {
        &self.fact
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Accessibility metadata values and certifier-report links retained from the package.
pub struct AccessibilityMetadata {
    values: Vec<AccessibilityMetadataValue>,
    certifier_reports: Vec<AccessibilityCertifierReport>,
}

impl AccessibilityMetadata {
    /// Iterates retained metadata values in authored order.
    pub fn values(&self) -> impl Iterator<Item = &AccessibilityMetadataValue> {
        self.values.iter()
    }

    /// Iterates retained metadata values with the requested interpreted property.
    pub fn values_of(
        &self,
        kind: AccessibilityMetadataKind,
    ) -> impl Iterator<Item = &AccessibilityMetadataValue> {
        self.values.iter().filter(move |value| value.kind == kind)
    }

    /// Iterates authored certifier-report links.
    pub fn certifier_reports(&self) -> impl Iterator<Item = &AccessibilityCertifierReport> {
        self.certifier_reports.iter()
    }

    pub(crate) fn contains_certifier_report(&self, report: &AccessibilityCertifierReport) -> bool {
        self.certifier_reports.contains(report)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// The recognized property represented by an accessibility metadata value.
pub enum AccessibilityMetadataKind {
    /// The `schema:accessMode` property.
    AccessMode,
    /// The `schema:accessModeSufficient` property.
    AccessModeSufficient,
    /// The `schema:accessibilityFeature` property.
    Feature,
    /// The `schema:accessibilityHazard` property.
    Hazard,
    /// The `schema:accessibilitySummary` property.
    Summary,
    /// The `a11y:certifierCredential` contact-email refinement.
    ContactEmail,
    /// The `a11y:certifiedBy` property.
    CertifiedBy,
    /// The `a11y:certifierCredential` property.
    CertifierCredential,
    /// A current or legacy page-break source property.
    PageBreakSource(PageBreakSourceTerm),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// An authored certifier-report link that can be resolved through publication analysis.
pub struct AccessibilityCertifierReport {
    authored: MetadataLink,
    reference: Option<ReferenceSlot>,
}

impl AccessibilityCertifierReport {
    /// Returns the retained package metadata link.
    pub fn authored(&self) -> &MetadataLink {
        &self.authored
    }

    pub(crate) fn reference_slot(&self) -> Option<ReferenceSlot> {
        self.reference
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// The current or legacy package term used for page-break source metadata.
pub enum PageBreakSourceTerm {
    /// The current `pageBreakSource` term.
    Current,
    /// The legacy `page-source` term.
    LegacyPageSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One retained package metadata value with its interpreted accessibility property.
pub struct AccessibilityMetadataValue {
    kind: AccessibilityMetadataKind,
    authored: Meta,
}

impl AccessibilityMetadataValue {
    /// Returns the interpreted property.
    pub fn kind(&self) -> AccessibilityMetadataKind {
        self.kind
    }

    /// Returns the retained package metadata element.
    pub fn authored(&self) -> &Meta {
        &self.authored
    }

    /// Returns its authored content when present.
    pub fn value(&self) -> Option<&str> {
        self.authored.content().map(|value| value.as_str())
    }

    /// Interprets recognized access-mode tokens for applicable properties.
    pub fn access_modes(&self) -> Option<Vec<AccessMode>> {
        matches!(
            self.kind,
            AccessibilityMetadataKind::AccessMode | AccessibilityMetadataKind::AccessModeSufficient
        )
        .then(|| {
            self.value()?
                .split(',')
                .flat_map(str::split_whitespace)
                .map(AccessMode::parse)
                .collect()
        })?
    }

    /// Interprets the value as a recognized accessibility feature when applicable.
    pub fn feature(&self) -> Option<AccessibilityFeature> {
        (self.kind == AccessibilityMetadataKind::Feature)
            .then(|| self.value().and_then(AccessibilityFeature::parse))?
    }

    /// Interprets the value as a recognized accessibility hazard when applicable.
    pub fn hazard(&self) -> Option<AccessibilityHazard> {
        (self.kind == AccessibilityMetadataKind::Hazard)
            .then(|| self.value().and_then(AccessibilityHazard::parse))?
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized Schema.org access-mode token.
pub enum AccessMode {
    /// Information perceived through hearing.
    Auditory,
    /// Charts conveyed visually.
    ChartOnVisual,
    /// Chemical information conveyed visually.
    ChemOnVisual,
    /// Understanding depends on color perception.
    ColorDependent,
    /// Diagrams conveyed visually.
    DiagramOnVisual,
    /// Mathematical notation conveyed visually.
    MathOnVisual,
    /// Musical notation conveyed visually.
    MusicOnVisual,
    /// Information perceived through touch.
    Tactile,
    /// Text conveyed visually.
    TextOnVisual,
    /// Information available as text.
    Textual,
    /// Information perceived through sight.
    Visual,
}

impl AccessMode {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auditory" => Self::Auditory,
            "chartOnVisual" => Self::ChartOnVisual,
            "chemOnVisual" => Self::ChemOnVisual,
            "colorDependent" => Self::ColorDependent,
            "diagramOnVisual" => Self::DiagramOnVisual,
            "mathOnVisual" => Self::MathOnVisual,
            "musicOnVisual" => Self::MusicOnVisual,
            "tactile" => Self::Tactile,
            "textOnVisual" => Self::TextOnVisual,
            "textual" => Self::Textual,
            "visual" => Self::Visual,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized Schema.org accessibility-feature token.
pub enum AccessibilityFeature {
    /// Alternatives are supplied for non-text content.
    AlternativeText,
    /// Annotation support is present.
    Annotations,
    /// ARIA semantics are used.
    Aria,
    /// Audio description is present.
    AudioDescription,
    /// Bookmark navigation is supported.
    Bookmarks,
    /// Braille content is present.
    Braille,
    /// Captions are present.
    Captions,
    /// Chemical notation is encoded with ChemML.
    ChemMl,
    /// Closed captions are present.
    ClosedCaptions,
    /// Mathematical content has descriptions.
    DescribedMath,
    /// Display properties can be transformed by the reader.
    DisplayTransformability,
    /// High-contrast audio is available.
    HighContrastAudio,
    /// High-contrast display content is available.
    HighContrastDisplay,
    /// An index is present.
    Index,
    /// Full ruby annotations are present.
    FullRubyAnnotations,
    /// Horizontal writing is supported.
    HorizontalWriting,
    /// A large-print presentation is available.
    LargePrint,
    /// Mathematical notation is available as LaTeX.
    Latex,
    /// Chemical notation is available as LaTeX.
    LatexChemistry,
    /// Long descriptions are present.
    LongDescription,
    /// Mathematical notation is encoded with MathML.
    MathMl,
    /// Chemical notation is encoded with MathML.
    MathMlChemistry,
    /// The publication claims no accessibility features.
    None,
    /// Open captions are present.
    OpenCaptions,
    /// Page-break markers are present.
    PageBreakMarkers,
    /// Navigation by page is present.
    PageNavigation,
    /// Print page numbers are present.
    PrintPageNumbers,
    /// A logical reading order is provided.
    ReadingOrder,
    /// Ruby annotations are present.
    RubyAnnotations,
    /// Sign-language content is present.
    SignLanguage,
    /// Structural navigation is present.
    StructuralNavigation,
    /// Synchronized text and audio are present.
    SynchronizedAudioText,
    /// A table of contents is present.
    TableOfContents,
    /// A tagged PDF rendition is present.
    TaggedPdf,
    /// Tactile graphics are present.
    TactileGraphic,
    /// Tactile objects are present.
    TactileObject,
    /// Playback timing can be controlled.
    TimingControl,
    /// A transcript is present.
    Transcript,
    /// Text-to-speech markup is present.
    TtsMarkup,
    /// Content is not restricted by access controls.
    Unlocked,
    /// The accessibility features are unknown.
    Unknown,
    /// Vertical writing is supported.
    VerticalWriting,
    /// Additional word segmentation is provided.
    WithAdditionalWordSegmentation,
    /// No additional word segmentation is required.
    WithoutAdditionalWordSegmentation,
}

impl AccessibilityFeature {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "alternativeText" => Self::AlternativeText,
            "annotations" => Self::Annotations,
            "ARIA" => Self::Aria,
            "audioDescription" => Self::AudioDescription,
            "bookmarks" => Self::Bookmarks,
            "braille" => Self::Braille,
            "captions" => Self::Captions,
            "ChemML" => Self::ChemMl,
            "closedCaptions" => Self::ClosedCaptions,
            "describedMath" => Self::DescribedMath,
            "displayTransformability" => Self::DisplayTransformability,
            "highContrastAudio" => Self::HighContrastAudio,
            "highContrastDisplay" => Self::HighContrastDisplay,
            "index" => Self::Index,
            "fullRubyAnnotations" => Self::FullRubyAnnotations,
            "horizontalWriting" => Self::HorizontalWriting,
            "largePrint" => Self::LargePrint,
            "latex" => Self::Latex,
            "latex-chemistry" => Self::LatexChemistry,
            "longDescription" => Self::LongDescription,
            "MathML" => Self::MathMl,
            "MathML-chemistry" => Self::MathMlChemistry,
            "none" => Self::None,
            "openCaptions" => Self::OpenCaptions,
            "pageBreakMarkers" => Self::PageBreakMarkers,
            "pageNavigation" => Self::PageNavigation,
            "printPageNumbers" => Self::PrintPageNumbers,
            "readingOrder" => Self::ReadingOrder,
            "rubyAnnotations" => Self::RubyAnnotations,
            "signLanguage" => Self::SignLanguage,
            "structuralNavigation" => Self::StructuralNavigation,
            "synchronizedAudioText" => Self::SynchronizedAudioText,
            "tableOfContents" => Self::TableOfContents,
            "taggedPDF" => Self::TaggedPdf,
            "tactileGraphic" => Self::TactileGraphic,
            "tactileObject" => Self::TactileObject,
            "timingControl" => Self::TimingControl,
            "transcript" => Self::Transcript,
            "ttsMarkup" => Self::TtsMarkup,
            "unlocked" => Self::Unlocked,
            "unknown" => Self::Unknown,
            "verticalWriting" => Self::VerticalWriting,
            "withAdditionalWordSegmentation" => Self::WithAdditionalWordSegmentation,
            "withoutAdditionalWordSegmentation" => Self::WithoutAdditionalWordSegmentation,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized Schema.org accessibility-hazard token.
pub enum AccessibilityHazard {
    /// The content contains a flashing hazard.
    Flashing,
    /// The content contains a motion-simulation hazard.
    MotionSimulation,
    /// The content claims no hazards.
    None,
    /// The content claims no flashing hazard.
    NoFlashingHazard,
    /// The content claims no motion-simulation hazard.
    NoMotionSimulationHazard,
    /// The content claims no sound hazard.
    NoSoundHazard,
    /// The content contains a sound hazard.
    Sound,
    /// Overall hazard information is unknown.
    Unknown,
    /// The presence of flashing hazards is unknown.
    UnknownFlashingHazard,
    /// The presence of motion-simulation hazards is unknown.
    UnknownMotionSimulationHazard,
    /// The presence of sound hazards is unknown.
    UnknownSoundHazard,
}

impl AccessibilityHazard {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "flashing" => Self::Flashing,
            "motionSimulation" => Self::MotionSimulation,
            "none" => Self::None,
            "noFlashingHazard" => Self::NoFlashingHazard,
            "noMotionSimulationHazard" => Self::NoMotionSimulationHazard,
            "noSoundHazard" => Self::NoSoundHazard,
            "sound" => Self::Sound,
            "unknown" => Self::Unknown,
            "unknownFlashingHazard" => Self::UnknownFlashingHazard,
            "unknownMotionSimulationHazard" => Self::UnknownMotionSimulationHazard,
            "unknownSoundHazard" => Self::UnknownSoundHazard,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// An authored `dcterms:conformsTo` claim and its optional exact interpretation.
pub struct AccessibilityClaim {
    authored: Meta,
    conformance: Option<AccessibilityConformance>,
}

impl AccessibilityClaim {
    /// Returns the retained package metadata element.
    pub fn authored(&self) -> &Meta {
        &self.authored
    }

    /// Returns the authored claim text when present.
    pub fn value(&self) -> Option<&str> {
        self.authored.content().map(|value| value.as_str())
    }

    /// Returns the exact recognized conformance tuple.
    pub fn conformance(&self) -> Option<AccessibilityConformance> {
        self.conformance
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// An exactly interpreted EPUB Accessibility and WCAG conformance claim.
pub struct AccessibilityConformance {
    epub_accessibility: EpubAccessibilityVersion,
    wcag: WcagVersion,
    level: WcagLevel,
}

impl AccessibilityConformance {
    /// Returns the recognized EPUB Accessibility version.
    pub fn epub_accessibility(self) -> EpubAccessibilityVersion {
        self.epub_accessibility
    }

    /// Returns the recognized WCAG version.
    pub fn wcag(self) -> WcagVersion {
        self.wcag
    }

    /// Returns the recognized WCAG conformance level.
    pub fn level(self) -> WcagLevel {
        self.level
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized EPUB Accessibility specification version.
pub enum EpubAccessibilityVersion {
    /// EPUB Accessibility 1.0.
    V1_0,
    /// EPUB Accessibility 1.1.
    V1_1,
    /// EPUB Accessibility 1.2.
    V1_2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized WCAG version.
pub enum WcagVersion {
    /// Web Content Accessibility Guidelines 2.0.
    V2_0,
    /// Web Content Accessibility Guidelines 2.1.
    V2_1,
    /// Web Content Accessibility Guidelines 2.2.
    V2_2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A recognized WCAG conformance level.
pub enum WcagLevel {
    /// WCAG level A.
    A,
    /// WCAG level AA.
    Aa,
    /// WCAG level AAA.
    Aaa,
}

/// One accessibility-relevant navigation, content, structure, overlay, or media observation.
///
/// Every observation belongs to one analysis snapshot. These are factual inputs for application
/// policy, not validation results or conformance verdicts. Coverage determines whether all
/// expected sources were available.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum AccessibilityObservationRef<'a> {
    /// A selected navigation list and its source resource when available.
    Navigation {
        /// The analyzed navigation-list properties.
        observation: &'a AccessibilityNavigationObservation,
        /// The navigation document resource, when present in the resource index.
        resource: Option<ResourceRef<'a>>,
    },
    /// One accessibility fact extracted from XHTML or SVG.
    Content {
        /// The source resource.
        resource: ResourceRef<'a>,
        /// The extracted accessibility fact.
        fact: &'a AccessibilityFact,
    },
    /// One structural fact relevant to accessibility consumers.
    Structure {
        /// The source resource.
        resource: ResourceRef<'a>,
        /// The extracted structural fact.
        fact: &'a StructureFact,
    },
    /// An analyzed standalone SMIL document.
    Smil {
        /// The SMIL resource.
        resource: ResourceRef<'a>,
        /// The analyzed SMIL facts.
        facts: &'a SmilFacts,
    },
    /// An authored reading-order media-overlay association.
    MediaOverlay(MediaOverlayAssociationRef<'a>),
    /// One inspected audio or video track.
    MediaTrack {
        /// The media resource.
        resource: ResourceRef<'a>,
        /// The inspected track.
        track: &'a MediaTrack,
    },
    /// An inspected WebVTT resource.
    WebVtt {
        /// The WebVTT resource.
        resource: ResourceRef<'a>,
        /// The inspected WebVTT facts.
        webvtt: &'a WebVtt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// The recognized role of a navigation-list observation.
pub enum AccessibilityNavigationKind {
    /// A table-of-contents list.
    Toc,
    /// A page-navigation list.
    PageList,
    /// A landmarks list.
    Landmarks,
    /// A list without one of the principal navigation roles.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Factual properties of one selected EPUB NAV or NCX list.
pub struct AccessibilityNavigationObservation {
    resource: Option<ResourceRow>,
    source: NavigationSource,
    kind: AccessibilityNavigationKind,
    hidden: bool,
    point_count: usize,
}

impl AccessibilityNavigationObservation {
    pub(crate) fn resource_key(&self) -> Option<ResourceRow> {
        self.resource
    }

    /// Returns whether the list came from EPUB NAV or NCX.
    pub fn source(&self) -> NavigationSource {
        self.source
    }

    /// Returns the interpreted navigation-list role.
    pub fn kind(&self) -> AccessibilityNavigationKind {
        self.kind
    }

    /// Returns whether the list was authored as hidden.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Returns the recursive number of navigation points.
    pub fn point_count(&self) -> usize {
        self.point_count
    }
}

fn collect_navigation(
    navigation: &Navigation,
    secondary_navigation: &[NavigationDocument],
    resources: &ResourceIndex,
) -> Vec<AccessibilityNavigationObservation> {
    navigation
        .document()
        .into_iter()
        .chain(secondary_navigation)
        .flat_map(|document| collect_navigation_document(document, resources))
        .collect()
}

fn collect_navigation_document(
    document: &NavigationDocument,
    resources: &ResourceIndex,
) -> Vec<AccessibilityNavigationObservation> {
    let address = ResourceAddress::Local(document.path().clone());
    let resource = resources
        .resources_at(&address)
        .next()
        .map(ResourceRef::row);
    document
        .lists()
        .iter()
        .map(|list| {
            let kind = match list.semantic() {
                Some(crate::semantics::EpubStructuralSemantic::Toc) => {
                    AccessibilityNavigationKind::Toc
                }
                Some(crate::semantics::EpubStructuralSemantic::PageList) => {
                    AccessibilityNavigationKind::PageList
                }
                Some(crate::semantics::EpubStructuralSemantic::Landmarks) => {
                    AccessibilityNavigationKind::Landmarks
                }
                _ => AccessibilityNavigationKind::Other,
            };
            AccessibilityNavigationObservation {
                resource,
                source: document.source(),
                kind,
                hidden: list.hidden(),
                point_count: count_navigation_points(list.points()),
            }
        })
        .collect()
}

fn count_navigation_points(points: &[NavigationPoint]) -> usize {
    points
        .iter()
        .map(|point| 1 + count_navigation_points(point.children()))
        .sum()
}

fn collect_content(
    facts: &[ResourceFacts],
    occurrences: Vec<(ResourceRow, AccessibilityFact)>,
) -> Vec<AccessibilityContentOccurrence> {
    let mut by_resource = HashMap::<ResourceRow, Vec<AccessibilityFact>>::new();
    let mut content_occurrences = Vec::with_capacity(occurrences.len());
    for (resource, fact) in occurrences {
        by_resource.entry(resource).or_default().push(fact);
    }
    for resource_facts in facts {
        let resource = resource_facts.resource_row();
        let Some(content) = resource_facts.content().value() else {
            continue;
        };
        if content.as_xhtml().is_none() && content.as_svg().is_none() {
            continue;
        }
        for fact in by_resource.remove(&resource).unwrap_or_default() {
            content_occurrences.push(AccessibilityContentOccurrence { resource, fact });
        }
    }
    assert!(
        by_resource.is_empty(),
        "every extracted accessibility occurrence must belong to retained content facts"
    );
    content_occurrences
}
