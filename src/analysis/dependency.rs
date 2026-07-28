//! Local resources needed to serve a resource, declaration, or reading-order entry.
//!
//! A closure includes resources reached through supported links, manifest fallbacks, and media
//! overlays. Unresolved links and incomplete relationship coverage are returned separately so an
//! application can decide whether the resource list is safe to package, cache, or remove.

use super::PublicationAnalysis;
use super::coverage::{CoverageState, RelationshipSource};
use super::reference::{
    AuthoredReference, HrefRole, HrefTarget, ManifestRole, ManifestTarget, ReferenceSource,
};
use crate::resource::{
    AuthoredIdRef, DeclarationTargetRow, ManifestOrdinal, ProviderPresence, ReadingOrderOrdinal,
    ReadingOrderTargetRow, ResourceOrdinal, ResourceRef,
};
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Resource(ResourceOrdinal),
    Declaration(ManifestOrdinal),
}

/// The resource, declaration, or reading-order entry whose local dependencies are requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Root {
    /// Start from a resolved resource.
    Resource(ResourceOrdinal),
    /// Start from a manifest declaration, preserving fallback and overlay edges.
    Declaration(ManifestOrdinal),
    /// Start from one reading-order occurrence and resolve its declaration.
    ReadingOrderOccurrence(ReadingOrderOrdinal),
}

/// Failure to resolve a dependency-closure root.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RootError {
    /// An ordinal is outside this analysis snapshot.
    #[error("ordinal is outside this analysis snapshot")]
    UnknownOrdinal,
    /// The occurrence has no authored manifest ID reference.
    #[error("reading-order occurrence has no manifest id reference")]
    MissingReadingOrderIdref(ReadingOrderOrdinal),
    /// The occurrence's authored ID has no matching manifest declaration.
    #[error("reading-order occurrence references a missing manifest id")]
    MissingManifestId {
        /// The snapshot-local reading-order occurrence.
        root: ReadingOrderOrdinal,
        /// The unresolved authored ID reference.
        idref: AuthoredIdRef,
    },
    /// The occurrence's authored ID matches more than one declaration.
    #[error("reading-order occurrence references an ambiguous manifest id")]
    AmbiguousManifestId {
        /// The snapshot-local reading-order occurrence.
        root: ReadingOrderOrdinal,
        /// All matching snapshot-local declarations.
        candidates: Vec<ManifestOrdinal>,
    },
}

/// Local resources and unresolved links reached from one dependency root.
///
/// Resource ordinals and references belong to the analysis snapshot that produced this value.
/// [`Self::unresolved`] reports authored links with no unique target, while
/// [`Self::incomplete_sources`] reports documents whose links were not fully extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closure {
    pub(crate) resources: Vec<ResourceOrdinal>,
    pub(crate) unresolved: Vec<AuthoredReference>,
    pub(crate) incomplete_sources: Vec<ReferenceSource>,
}

impl Closure {
    /// Returns reached resource ordinals in deterministic traversal order.
    pub fn resources(&self) -> &[ResourceOrdinal] {
        &self.resources
    }

    /// Returns dependency references whose authored targets could not be resolved.
    pub fn unresolved(&self) -> &[AuthoredReference] {
        &self.unresolved
    }

    /// Returns sources whose relationships were not fully available to traversal.
    pub fn incomplete_sources(&self) -> &[ReferenceSource] {
        &self.incomplete_sources
    }

    /// Returns whether traversal had neither unresolved targets nor incomplete producers.
    pub fn is_complete(&self) -> bool {
        self.unresolved.is_empty() && self.incomplete_sources.is_empty()
    }
}

impl PublicationAnalysis {
    /// Returns the local resources needed to serve `root`.
    ///
    /// Remote, data, and other external targets are not returned as local resources.
    pub fn dependency_closure(&self, root: Root) -> Result<Closure, RootError> {
        let root = match root {
            Root::Resource(key) => {
                self.resources()
                    .resource(key)
                    .map_err(|_| RootError::UnknownOrdinal)?;
                Node::Resource(key)
            }
            Root::Declaration(key) => {
                self.resources()
                    .declaration(key)
                    .map_err(|_| RootError::UnknownOrdinal)?;
                Node::Declaration(key)
            }
            Root::ReadingOrderOccurrence(key) => {
                let entry = self
                    .resources()
                    .occurrence(key)
                    .map_err(|_| RootError::UnknownOrdinal)?;
                match entry.target_row() {
                    ReadingOrderTargetRow::Declaration { declaration, .. } => {
                        Node::Declaration((*declaration).into())
                    }
                    ReadingOrderTargetRow::MissingIdref => {
                        return Err(RootError::MissingReadingOrderIdref(key));
                    }
                    ReadingOrderTargetRow::MissingManifestId => {
                        let Some(idref) = entry.idref().cloned() else {
                            return Err(RootError::MissingReadingOrderIdref(key));
                        };
                        return Err(RootError::MissingManifestId { root: key, idref });
                    }
                    ReadingOrderTargetRow::AmbiguousManifestId { candidates } => {
                        return Err(RootError::AmbiguousManifestId {
                            root: key,
                            candidates: candidates.iter().copied().map(Into::into).collect(),
                        });
                    }
                }
            }
        };

        let mut queue = VecDeque::from([root]);
        let mut visited_resources = HashSet::new();
        let mut visited_declarations = HashSet::new();
        let mut resources = Vec::new();
        let mut unresolved = Vec::new();
        let mut incomplete_sources = Vec::new();
        let mut incomplete_seen = HashSet::new();

        while let Some(node) = queue.pop_front() {
            match node {
                Node::Resource(key) => {
                    if !visited_resources.insert(key) {
                        continue;
                    }
                    resources.push(key);
                    let record = self
                        .resources()
                        .resource(key)
                        .expect("dependency nodes use resource-index keys");
                    if record.presence() == ProviderPresence::Missing
                        || self.dependency_remote_relationships_unknown(record)
                        || self.resource_relationships_incomplete(key)
                    {
                        push_unique(
                            &mut incomplete_sources,
                            &mut incomplete_seen,
                            ReferenceSource::Resource(key),
                        );
                    }
                    for reference in self.references().filter(|reference| {
                        matches!(reference.source(), ReferenceSource::Resource(source) if source == key)
                            && is_dependency_reference(reference)
                    }) {
                        self.follow_dependency(reference, &mut queue, &mut unresolved);
                    }
                }
                Node::Declaration(key) => {
                    if !visited_declarations.insert(key) {
                        continue;
                    }
                    if self.package_relationships_incomplete() {
                        push_unique(
                            &mut incomplete_sources,
                            &mut incomplete_seen,
                            ReferenceSource::Declaration(key),
                        );
                    }
                    let declaration = self
                        .resources()
                        .declaration(key)
                        .expect("dependency nodes use resource-index keys");
                    match declaration.target_row() {
                        DeclarationTargetRow::Resource(resource) => {
                            queue.push_back(Node::Resource((*resource).into()));
                        }
                        DeclarationTargetRow::MissingHref
                        | DeclarationTargetRow::InvalidHref(_) => {
                            push_unique(
                                &mut incomplete_sources,
                                &mut incomplete_seen,
                                ReferenceSource::Declaration(key),
                            );
                        }
                    }
                    for reference in self.references().filter(|reference| {
                        matches!(reference.source(), ReferenceSource::Declaration(source) if source == key)
                            && is_dependency_reference(reference)
                    }) {
                        self.follow_dependency(reference, &mut queue, &mut unresolved);
                    }
                }
            }
        }

        Ok(Closure {
            resources,
            unresolved,
            incomplete_sources,
        })
    }

    fn follow_dependency(
        &self,
        reference: &AuthoredReference,
        queue: &mut VecDeque<Node>,
        unresolved: &mut Vec<AuthoredReference>,
    ) {
        match reference {
            AuthoredReference::Href(href) => match href.target() {
                HrefTarget::Resource { resource, .. } => {
                    queue.push_back(Node::Resource(*resource));
                    if self
                        .resources()
                        .resource(*resource)
                        .is_ok_and(|record| record.presence() == ProviderPresence::Missing)
                    {
                        unresolved.push(reference.clone());
                    }
                }
                HrefTarget::Fragment {
                    resource, exists, ..
                } => {
                    queue.push_back(Node::Resource(*resource));
                    if *exists == Some(false)
                        || self
                            .resources()
                            .resource(*resource)
                            .is_ok_and(|record| record.presence() == ProviderPresence::Missing)
                    {
                        unresolved.push(reference.clone());
                    }
                }
                HrefTarget::Remote {
                    declared_resource: Some(resource),
                    ..
                } => queue.push_back(Node::Resource(*resource)),
                HrefTarget::MissingLocal(_) | HrefTarget::Invalid(_) => {
                    unresolved.push(reference.clone());
                }
                HrefTarget::Remote {
                    declared_resource: None,
                    ..
                }
                | HrefTarget::Data(_)
                | HrefTarget::External(_) => {}
            },
            AuthoredReference::Manifest(idref) => match idref.target() {
                ManifestTarget::Declaration {
                    declaration,
                    resource,
                } => {
                    queue.push_back(Node::Declaration(*declaration));
                    if resource.is_none() {
                        unresolved.push(reference.clone());
                    }
                }
                ManifestTarget::Missing | ManifestTarget::Ambiguous { .. } => {
                    unresolved.push(reference.clone());
                }
            },
        }
    }

    fn resource_relationships_incomplete(&self, key: ResourceOrdinal) -> bool {
        self.coverage().relationships().iter().any(|coverage| {
            matches!(
                coverage.source(),
                RelationshipSource::Xhtml(resource)
                    | RelationshipSource::Css(resource)
                    | RelationshipSource::Svg(resource)
                    | RelationshipSource::Smil(resource)
                    if *resource == key
            ) && !matches!(coverage.state(), CoverageState::Complete)
        })
    }

    fn dependency_remote_relationships_unknown(&self, record: ResourceRef<'_>) -> bool {
        record.presence() == ProviderPresence::NotApplicable
            && record.declarations().any(|declaration| {
                declaration.media_type().is_some_and(|media_type| {
                    media_type.is_xhtml()
                        || media_type.is_css()
                        || media_type.is_svg()
                        || media_type.is_smil()
                        || media_type.is_ncx()
                })
            })
    }

    fn package_relationships_incomplete(&self) -> bool {
        self.coverage().relationships().iter().any(|coverage| {
            matches!(coverage.source(), RelationshipSource::Package)
                && !matches!(coverage.state(), CoverageState::Complete)
        })
    }
}

fn is_dependency_reference(reference: &AuthoredReference) -> bool {
    match reference {
        AuthoredReference::Href(reference) => matches!(
            reference.role(),
            HrefRole::Stylesheet
                | HrefRole::Image
                | HrefRole::Script
                | HrefRole::Audio
                | HrefRole::Video
                | HrefRole::Source
                | HrefRole::Track
                | HrefRole::Poster
                | HrefRole::Object
                | HrefRole::Embed
                | HrefRole::Iframe
                | HrefRole::Svg
                | HrefRole::SmilText
                | HrefRole::SmilAudio
                | HrefRole::CssImport
                | HrefRole::CssUrl
                | HrefRole::Font
        ),
        AuthoredReference::Manifest(reference) => matches!(
            reference.role(),
            ManifestRole::Fallback | ManifestRole::MediaOverlay
        ),
    }
}

fn push_unique<T: Copy + Eq + std::hash::Hash>(
    values: &mut Vec<T>,
    seen: &mut HashSet<T>,
    value: T,
) {
    if seen.insert(value) {
        values.push(value);
    }
}
