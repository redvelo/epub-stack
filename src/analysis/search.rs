//! Extracted XHTML text for search indexes and text-to-speech preparation.
//!
//! Entries connect each searchable or speakable text chunk to its resource, manifest
//! declarations, and reading-order positions. Text is normalized source content, not rendered
//! browser text; tokenization, ranking, and durable index storage remain application concerns.

use super::{PublicationAnalysis, ResourceFacts};
use crate::content::ContentFacts;
use crate::content::text::{TextChunk, TextRangeError};
use crate::resource::{
    ManifestDeclarationRef, ReadingOrderOccurrenceRef, ReadingOrderTargetRow, ResourceIndex,
    ResourceRef,
};

/// One extracted text chunk with its resource and publication context.
///
/// All returned references belong to the same [`PublicationAnalysis`] and cannot outlive it.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    resources: &'a ResourceIndex,
    resource: ResourceRef<'a>,
    facts: &'a ResourceFacts,
    chunk: &'a TextChunk,
}

impl<'a> Entry<'a> {
    pub(crate) fn new(
        resources: &'a ResourceIndex,
        resource: ResourceRef<'a>,
        facts: &'a ResourceFacts,
        chunk: &'a TextChunk,
    ) -> Self {
        Self {
            resources,
            resource,
            facts,
            chunk,
        }
    }

    /// Returns the chunk's snapshot resource record.
    pub fn resource(&self) -> ResourceRef<'a> {
        self.resource
    }

    /// Returns the analysis results for that resource.
    pub fn facts(&self) -> &ResourceFacts {
        self.facts
    }

    /// Returns the extracted text chunk.
    pub fn chunk(&self) -> &TextChunk {
        self.chunk
    }

    /// Borrows the chunk text from its containing stream or owned chunk value.
    pub fn text(&self) -> Result<&str, TextRangeError> {
        let stream = self
            .facts
            .content()
            .value()
            .and_then(ContentFacts::as_xhtml)
            .expect("search entries are constructed from XHTML facts")
            .text_stream();
        self.chunk.text(stream)
    }

    /// Iterates manifest declarations associated with the resource.
    pub fn declarations(&self) -> impl Iterator<Item = ManifestDeclarationRef<'a>> {
        self.resource.declarations()
    }

    /// Iterates reading-order occurrences that resolve to the resource.
    pub fn reading_order_entries(&self) -> impl Iterator<Item = ReadingOrderOccurrenceRef<'a>> {
        let resource = self.resource.key();
        self.resources.reading_order().filter(move |entry| {
            matches!(
                entry.target_row(),
                ReadingOrderTargetRow::Declaration {
                    resource: Some(key),
                    ..
                } if *key == resource
            )
        })
    }
}

impl PublicationAnalysis {
    /// Iterates XHTML text chunks with the context needed for application-owned indexing.
    ///
    /// Chunks use extracted source text; this does not tokenize, rank, or reproduce rendered text.
    pub fn search_entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.resource_facts().flat_map(move |facts| {
            let resource = self
                .resources()
                .resource(facts.resource())
                .expect("analysis facts use resource-index ordinals");
            facts
                .content()
                .value()
                .and_then(ContentFacts::as_xhtml)
                .into_iter()
                .flat_map(move |content| {
                    content
                        .text()
                        .iter()
                        .map(move |chunk| Entry::new(self.resources(), resource, facts, chunk))
                })
        })
    }
}
