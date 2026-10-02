//! Local resources needed to serve a resource or declaration.
//!
//! Dependency traversal follows supported links, manifest fallbacks, and media overlays.
//! Unresolved links and incomplete sources are reported separately. For a reading-order
//! occurrence, start from its resolved declaration; the occurrence target records why an
//! occurrence has none.

use super::coverage::RelationshipSource;
use super::reference::{AuthoredReference, HrefRole, HrefTarget, ManifestRole, ManifestTarget};
use super::{DeclarationAnalysisRef, PublicationAnalysis, ResourceAnalysisRef};
use crate::resource::{ManifestOrdinal, ProviderPresence, ResourceOrdinal};
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Resource(ResourceOrdinal),
    Declaration(ManifestOrdinal),
}

/// Local resources and unresolved links reached from one dependency root.
///
/// [`Self::unresolved`] reports authored links with no usable target, while
/// [`Self::incomplete_sources`] reports documents whose links were not fully extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closure {
    pub(crate) resources: Vec<ResourceOrdinal>,
    pub(crate) unresolved: Vec<AuthoredReference>,
    pub(crate) incomplete_sources: Vec<RelationshipSource>,
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

    /// Returns reached sources whose relationships were not fully available to traversal.
    ///
    /// This is closure-local: it names only producers among the resources this traversal reached,
    /// unlike publication-wide relationship coverage.
    pub fn incomplete_sources(&self) -> &[RelationshipSource] {
        &self.incomplete_sources
    }

    /// Returns whether every reached relationship producer completed.
    ///
    /// Unresolved authored links are facts about the publication and do not make traversal
    /// incomplete.
    pub fn is_complete(&self) -> bool {
        self.incomplete_sources.is_empty()
    }
}

impl<'a> ResourceAnalysisRef<'a> {
    /// Returns the local resources needed to serve this resource.
    ///
    /// Remote, data, and other external targets are not returned as local resources.
    pub fn dependency_closure(self) -> Closure {
        self.analysis
            .dependency_closure(Node::Resource(self.resource().ordinal()))
    }
}

impl<'a> DeclarationAnalysisRef<'a> {
    /// Returns the local resources needed to serve this declaration, including fallback and
    /// media-overlay edges.
    pub fn dependency_closure(self) -> Closure {
        self.analysis
            .dependency_closure(Node::Declaration(self.declaration().ordinal()))
    }
}

impl PublicationAnalysis {
    fn dependency_closure(&self, root: Node) -> Closure {
        let mut queue = VecDeque::from([root]);
        let mut visited_resources = HashSet::new();
        let mut visited_declarations = HashSet::new();
        let mut resources = Vec::new();
        let mut unresolved = Vec::new();
        let mut incomplete_sources = Vec::new();

        while let Some(node) = queue.pop_front() {
            match node {
                Node::Resource(key) => {
                    if !visited_resources.insert(key) {
                        continue;
                    }
                    resources.push(key);
                    incomplete_sources.extend(
                        self.coverage()
                            .incomplete_relationships()
                            .filter(|source| source.resource() == key),
                    );
                    for reference in self
                        .references_at(&self.resource_references[key.index()].from)
                        .filter(|reference| is_dependency_reference(reference))
                    {
                        self.follow_dependency(reference, &mut queue, &mut unresolved);
                    }
                }
                Node::Declaration(key) => {
                    if !visited_declarations.insert(key) {
                        continue;
                    }
                    let declaration = self
                        .declaration(key)
                        .expect("dependency nodes use resource-index keys");
                    if let Some(resource) = declaration.declaration().resource() {
                        queue.push_back(Node::Resource(resource.ordinal()));
                    }
                    for reference in self
                        .references_at(&declaration.reference_index().from)
                        .filter(|reference| is_dependency_reference(reference))
                    {
                        self.follow_dependency(reference, &mut queue, &mut unresolved);
                    }
                }
            }
        }

        Closure {
            resources,
            unresolved,
            incomplete_sources,
        }
    }

    fn follow_dependency(
        &self,
        reference: &AuthoredReference,
        queue: &mut VecDeque<Node>,
        unresolved: &mut Vec<AuthoredReference>,
    ) {
        let missing = |resource: ResourceOrdinal| {
            self.resources()
                .resource(resource)
                .is_some_and(|record| record.presence() == ProviderPresence::Missing)
        };
        match reference {
            AuthoredReference::Href(href) => match href.target() {
                HrefTarget::Resource { resource, .. } => {
                    queue.push_back(Node::Resource(*resource));
                    if missing(*resource) {
                        unresolved.push(reference.clone());
                    }
                }
                HrefTarget::Fragment {
                    resource, exists, ..
                } => {
                    queue.push_back(Node::Resource(*resource));
                    if *exists == Some(false) || missing(*resource) {
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
                ManifestTarget::InvalidManifestIdref
                | ManifestTarget::Missing
                | ManifestTarget::Ambiguous { .. } => {
                    unresolved.push(reference.clone());
                }
            },
        }
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
