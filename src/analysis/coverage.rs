//! Completeness of extracted links, content, inspection metadata, and fingerprints.
//!
//! Use [`Coverage`] to distinguish an empty result from analysis that could not inspect all
//! expected resources. Coverage belongs to one analysis snapshot, and its resource ordinals must be
//! used only with that snapshot.

use super::AnalysisIssue;
use crate::resource::ResourceOrdinal;
use std::collections::HashSet;

/// One expected resource whose result is partial or unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteResource {
    resource: ResourceOrdinal,
    issue: AnalysisIssue,
}

impl IncompleteResource {
    /// Returns the resource ordinal in the containing analysis snapshot.
    pub fn resource(&self) -> ResourceOrdinal {
        self.resource
    }

    /// Returns why the resource's result is incomplete.
    pub fn issue(&self) -> AnalysisIssue {
        self.issue
    }

    pub(crate) fn new(resource: impl Into<ResourceOrdinal>, issue: AnalysisIssue) -> Self {
        Self {
            resource: resource.into(),
            issue,
        }
    }
}

/// Completeness of one analysis result family across its expected resources.
///
/// Each ordinal in [`Self::expected`] occurs in exactly one of [`Self::completed`],
/// [`Self::partial`], or [`Self::unavailable`]. Ordinals apply only to the analysis snapshot
/// that owns this value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceCoverage {
    expected: Vec<ResourceOrdinal>,
    completed: Vec<ResourceOrdinal>,
    partial: Vec<IncompleteResource>,
    unavailable: Vec<IncompleteResource>,
}

impl ResourceCoverage {
    /// Returns every resource for which this result family applies.
    pub fn expected(&self) -> &[ResourceOrdinal] {
        &self.expected
    }

    /// Returns resources with complete results.
    pub fn completed(&self) -> &[ResourceOrdinal] {
        &self.completed
    }

    /// Returns resources with usable but incomplete results.
    pub fn partial(&self) -> &[IncompleteResource] {
        &self.partial
    }

    /// Returns resources for which no result was produced.
    pub fn unavailable(&self) -> &[IncompleteResource] {
        &self.unavailable
    }

    /// Returns whether every expected resource completed.
    pub fn is_complete(&self) -> bool {
        self.partial.is_empty()
            && self.unavailable.is_empty()
            && self.completed.len() == self.expected.len()
    }

    pub(crate) fn new<T: Into<ResourceOrdinal>>(
        expected: Vec<T>,
        completed: Vec<T>,
        partial: Vec<IncompleteResource>,
        unavailable: Vec<IncompleteResource>,
    ) -> Self {
        let expected = expected.into_iter().map(Into::into).collect::<Vec<_>>();
        let completed = completed.into_iter().map(Into::into).collect::<Vec<_>>();
        assert_eq!(
            expected.len(),
            completed.len() + partial.len() + unavailable.len()
        );
        let expected_set = expected.iter().copied().collect::<HashSet<_>>();
        assert_eq!(expected_set.len(), expected.len());
        let states = completed
            .iter()
            .copied()
            .chain(partial.iter().map(IncompleteResource::resource))
            .chain(unavailable.iter().map(IncompleteResource::resource))
            .collect::<Vec<_>>();
        assert_eq!(states.iter().copied().collect::<HashSet<_>>(), expected_set);
        assert_eq!(
            states.iter().copied().collect::<HashSet<_>>().len(),
            states.len()
        );
        Self {
            expected,
            completed,
            partial,
            unavailable,
        }
    }
}

/// A package or document from which authored links can be extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipSource {
    /// OPF package relationships.
    Package,
    /// The selected EPUB NAV document at this snapshot resource ordinal.
    Navigation(ResourceOrdinal),
    /// An NCX document at this snapshot resource ordinal.
    Ncx(ResourceOrdinal),
    /// A SMIL document at this snapshot resource ordinal.
    Smil(ResourceOrdinal),
    /// An XHTML document at this snapshot resource ordinal.
    Xhtml(ResourceOrdinal),
    /// A stylesheet at this snapshot resource ordinal.
    Css(ResourceOrdinal),
    /// An SVG document at this snapshot resource ordinal.
    Svg(ResourceOrdinal),
}

/// Completeness of links extracted from one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageState {
    /// All supported relationships were produced.
    Complete,
    /// Some relationships were produced before the operational issue.
    Partial(AnalysisIssue),
    /// No relationships could be produced because of the operational issue.
    Unavailable(AnalysisIssue),
}

/// Coverage of authored relationships from one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipCoverage {
    source: RelationshipSource,
    state: CoverageState,
}

impl RelationshipCoverage {
    /// Returns the relationship producer.
    pub fn source(&self) -> &RelationshipSource {
        &self.source
    }

    /// Returns the producer's execution completeness.
    pub fn state(&self) -> &CoverageState {
        &self.state
    }

    pub(crate) fn new(source: RelationshipSource, state: CoverageState) -> Self {
        Self { source, state }
    }
}

/// Completeness summaries for links, content, inspection, classification, and fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    relationships: Vec<RelationshipCoverage>,
    classification: ResourceCoverage,
    fragments: ResourceCoverage,
    content: ResourceCoverage,
    inspection: ResourceCoverage,
    fingerprints: ResourceCoverage,
}

impl Coverage {
    /// Returns coverage for each package or document relationship producer.
    pub fn relationships(&self) -> &[RelationshipCoverage] {
        &self.relationships
    }

    /// Returns semantic-format classification coverage.
    pub fn classification(&self) -> &ResourceCoverage {
        &self.classification
    }

    /// Returns fragment-discovery coverage used by reference resolution.
    pub fn fragments(&self) -> &ResourceCoverage {
        &self.fragments
    }

    /// Returns semantic content-extraction coverage.
    pub fn content(&self) -> &ResourceCoverage {
        &self.content
    }

    /// Returns byte-level resource-inspection coverage.
    pub fn inspection(&self) -> &ResourceCoverage {
        &self.inspection
    }

    /// Returns whole-resource fingerprint coverage.
    pub fn fingerprints(&self) -> &ResourceCoverage {
        &self.fingerprints
    }

    pub(crate) fn new(
        relationships: Vec<RelationshipCoverage>,
        classification: ResourceCoverage,
        fragments: ResourceCoverage,
        content: ResourceCoverage,
        inspection: ResourceCoverage,
        fingerprints: ResourceCoverage,
    ) -> Self {
        assert_eq!(
            relationships
                .iter()
                .map(RelationshipCoverage::source)
                .collect::<HashSet<_>>()
                .len(),
            relationships.len()
        );
        Self {
            relationships,
            classification,
            fragments,
            content,
            inspection,
            fingerprints,
        }
    }
}
