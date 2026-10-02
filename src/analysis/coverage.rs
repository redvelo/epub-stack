//! Completeness of extracted links, content, inspection metadata, and fingerprints.
//!
//! [`Coverage`] records which results are complete, partial, or unavailable, and why.

use super::{AnalysisIssue, AnalysisOutcome, PublicationAnalysis, ResourceAnalysis};
use crate::resource::ResourceOrdinal;

/// Whether a piece of analysis finished, stopped early, or produced nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Completeness {
    /// It finished.
    Complete,
    /// It produced something usable, then stopped.
    Partial(AnalysisIssue),
    /// It produced nothing usable.
    Unavailable(AnalysisIssue),
}

impl Completeness {
    /// Why it stopped, if it did.
    pub fn issue(self) -> Option<AnalysisIssue> {
        match self {
            Self::Complete => None,
            Self::Partial(issue) | Self::Unavailable(issue) => Some(issue),
        }
    }

    /// Whether it finished.
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }
}

/// How one resource fared for one kind of analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCompleteness {
    /// Which resource this is about.
    pub resource: ResourceOrdinal,
    /// Whether the resource's result is complete, and why not.
    pub completeness: Completeness,
}

#[derive(Debug, Clone, Copy)]
enum Entries<'a> {
    Derived {
        facts: &'a [ResourceAnalysis],
        outcome: fn(&ResourceAnalysis) -> Option<Completeness>,
    },
    Stored(&'a [ResourceCompleteness]),
}

/// Completeness of one analysis result family across the resources it applies to.
#[derive(Debug, Clone, Copy)]
pub struct ResourceCoverage<'a> {
    entries: Entries<'a>,
}

impl<'a> ResourceCoverage<'a> {
    /// Iterates every resource for which this result family applies, in resource-index order.
    pub fn iter(self) -> impl Iterator<Item = ResourceCompleteness> + 'a {
        let (facts, outcome, stored) = match self.entries {
            Entries::Derived { facts, outcome } => (facts, Some(outcome), &[][..]),
            Entries::Stored(stored) => (&[][..], None, stored),
        };
        let derived = facts.iter().filter_map(move |facts| {
            outcome?(facts).map(|completeness| ResourceCompleteness {
                resource: facts.resource(),
                completeness,
            })
        });
        derived.chain(stored.iter().copied())
    }

    /// Iterates resources whose result is partial or unavailable.
    pub fn incomplete(self) -> impl Iterator<Item = ResourceCompleteness> + 'a {
        self.iter()
            .filter(|entry| !entry.completeness.is_complete())
    }

    /// Returns whether every applicable resource completed.
    pub fn is_complete(self) -> bool {
        self.incomplete().next().is_none()
    }
}

/// A document from which authored links can be extracted.
///
/// OPF package relationships come from the parsed package and are always complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipSource {
    /// The book's navigation document.
    Navigation(ResourceOrdinal),
    /// A legacy NCX document.
    Ncx(ResourceOrdinal),
    /// A media overlay.
    Smil(ResourceOrdinal),
    /// An XHTML document.
    Xhtml(ResourceOrdinal),
    /// A stylesheet.
    Css(ResourceOrdinal),
    /// An SVG document.
    Svg(ResourceOrdinal),
}

impl RelationshipSource {
    /// Returns the source document's resource ordinal.
    pub fn resource(self) -> ResourceOrdinal {
        match self {
            Self::Navigation(resource)
            | Self::Ncx(resource)
            | Self::Smil(resource)
            | Self::Xhtml(resource)
            | Self::Css(resource)
            | Self::Svg(resource) => resource,
        }
    }
}

/// Coverage of authored relationships from one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationshipCoverage {
    /// The relationship producer.
    pub source: RelationshipSource,
    /// The producer's execution completeness.
    pub completeness: Completeness,
}

impl RelationshipCoverage {
    pub(crate) fn new(source: RelationshipSource, completeness: Completeness) -> Self {
        Self {
            source,
            completeness,
        }
    }
}

/// Completeness summaries for links, content, inspection, and fingerprints.
///
/// Remote resources declared with a supported document type have unavailable relationship
/// coverage because their links cannot be extracted.
#[derive(Debug, Clone, Copy)]
pub struct Coverage<'a> {
    analysis: &'a PublicationAnalysis,
}

impl<'a> Coverage<'a> {
    pub(crate) fn new(analysis: &'a PublicationAnalysis) -> Self {
        Self { analysis }
    }

    /// Whether every part of the analysis finished: relationships, fragments, content,
    /// inspection, and fingerprints.
    ///
    /// A part with nothing to cover counts as complete, so this reports that nothing stopped
    /// early rather than that there was anything to find.
    pub fn is_complete(self) -> bool {
        self.incomplete_relationships().next().is_none()
            && self.fragments().is_complete()
            && self.content().is_complete()
            && self.inspection().is_complete()
            && self.fingerprints().is_complete()
    }

    /// Returns coverage for each document relationship producer.
    pub fn relationships(self) -> &'a [RelationshipCoverage] {
        &self.analysis.relationship_coverage
    }

    /// Iterates relationship producers that did not complete.
    pub fn incomplete_relationships(self) -> impl Iterator<Item = RelationshipSource> + 'a {
        self.relationships()
            .iter()
            .filter(|coverage| !coverage.completeness.is_complete())
            .map(|coverage| coverage.source)
    }

    /// Returns fragment-discovery coverage for resources targeted by fragment references.
    pub fn fragments(self) -> ResourceCoverage<'a> {
        ResourceCoverage {
            entries: Entries::Stored(&self.analysis.fragment_coverage),
        }
    }

    /// Returns semantic content-extraction coverage.
    pub fn content(self) -> ResourceCoverage<'a> {
        self.derived(|facts| facts.content().completeness())
    }

    /// Returns byte-level resource-inspection coverage.
    pub fn inspection(self) -> ResourceCoverage<'a> {
        self.derived(|facts| facts.inspection().completeness())
    }

    /// Returns whole-resource fingerprint coverage.
    pub fn fingerprints(self) -> ResourceCoverage<'a> {
        self.derived(|facts| facts.fingerprint().completeness())
    }

    fn derived(
        self,
        outcome: fn(&ResourceAnalysis) -> Option<Completeness>,
    ) -> ResourceCoverage<'a> {
        ResourceCoverage {
            entries: Entries::Derived {
                facts: &self.analysis.resource_facts,
                outcome,
            },
        }
    }
}

impl<T> AnalysisOutcome<T> {
    /// Returns the operation's completeness, or `None` when it was not applicable.
    pub fn completeness(&self) -> Option<Completeness> {
        match self {
            Self::NotApplicable => None,
            Self::Complete(_) => Some(Completeness::Complete),
            Self::Partial { issue, .. } => Some(Completeness::Partial(*issue)),
            Self::Unavailable(issue) => Some(Completeness::Unavailable(*issue)),
        }
    }
}
