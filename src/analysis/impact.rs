//! Advisory consequences of resource edits.
//!
//! Impact queries identify incoming links, relative links affected by a move, and package or
//! navigation structures an application may need to update. They do not perform the edit.
//! Incomplete relationship coverage means additional consequences may exist.

use super::PublicationAnalysis;
use super::coverage::{CoverageState, RelationshipSource};
use super::reference::{
    AuthoredReference, HrefTarget, ManifestReference, ManifestRole, ManifestTarget,
    ReferenceContext, ReferenceSource,
};
use crate::resource::{
    AuthoredHref, EpubPath, ManifestOrdinal, ParsedHref, ProviderPresence,
    ReadingOrderOccurrenceRef, ReadingOrderOrdinal, ReadingOrderTargetRow, ResourceOrdinal,
    ResourceRef, parse_href,
};
use std::collections::HashSet;

/// Links and publication structures potentially affected by removing or moving a resource.
///
/// This value does not execute an edit and can become stale after any committed change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Impact {
    pub(crate) resource: ResourceOrdinal,
    pub(crate) incoming: Vec<AuthoredReference>,
    pub(crate) outgoing_rebased: Vec<AuthoredReference>,
    pub(crate) structural_changes: Vec<StructuralChange>,
    pub(crate) incomplete_sources: Vec<ReferenceSource>,
}

/// Failure to derive resource impact from a requested snapshot ordinal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImpactError {
    /// The ordinal is outside this analysis snapshot.
    #[error("resource ordinal is outside this analysis snapshot")]
    UnknownOrdinal,
    /// Package-document changes require package-aware edit planning.
    #[error("package document impact is not represented by resource impact")]
    PackageDocument(ResourceOrdinal),
}

impl Impact {
    /// Returns the affected snapshot-local resource ordinal.
    pub fn resource(&self) -> ResourceOrdinal {
        self.resource
    }

    /// Returns authored references that target the affected resource.
    pub fn incoming(&self) -> &[AuthoredReference] {
        &self.incoming
    }

    /// Returns relative outgoing hrefs whose meaning may change after a move.
    pub fn outgoing_rebased(&self) -> &[AuthoredReference] {
        &self.outgoing_rebased
    }

    /// Returns package or navigation structures that an edit may need to update.
    pub fn structural_changes(&self) -> &[StructuralChange] {
        &self.structural_changes
    }

    /// Returns relationship producers that could hide additional impact.
    pub fn incomplete_sources(&self) -> &[ReferenceSource] {
        &self.incomplete_sources
    }

    /// Returns whether every relevant relationship producer completed.
    pub fn is_complete(&self) -> bool {
        self.incomplete_sources.is_empty()
    }
}

/// A package or navigation structure potentially affected by a resource edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralChange {
    /// A manifest declaration for the resource.
    ManifestDeclaration(ManifestOrdinal),
    /// A reading-order occurrence resolving to the resource.
    ReadingOrderOccurrence(ReadingOrderOrdinal),
    /// An authored navigation href targeting the resource.
    NavigationReference(AuthoredReference),
    /// A manifest fallback relationship targeting the resource.
    FallbackReference(AuthoredReference),
    /// A manifest media-overlay relationship targeting the resource.
    MediaOverlayReference(AuthoredReference),
}

impl PublicationAnalysis {
    /// Derives advisory consequences of removing a snapshot resource.
    pub fn impact_of_removal(&self, key: ResourceOrdinal) -> Result<Impact, ImpactError> {
        let record = self
            .resources()
            .resource(key)
            .map_err(|_| ImpactError::UnknownOrdinal)?;
        if key == self.resources().package().ordinal() {
            return Err(ImpactError::PackageDocument(key));
        }
        let declarations = record
            .declarations()
            .map(|declaration| declaration.ordinal())
            .collect::<HashSet<_>>();
        let incoming = self
            .references()
            .filter(|reference| {
                reference_targets_resource(reference, key)
                    || matches!(reference, AuthoredReference::Manifest(idref) if manifest_id_targets(idref, key, &declarations))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut structural_changes = record
            .declarations()
            .map(|declaration| StructuralChange::ManifestDeclaration(declaration.ordinal()))
            .collect::<Vec<_>>();

        structural_changes.extend(self.resources().reading_order().filter_map(|entry| {
            reading_order_targets(entry, key, &declarations)
                .then_some(StructuralChange::ReadingOrderOccurrence(entry.ordinal()))
        }));
        structural_changes.extend(incoming.iter().filter_map(|reference| match reference {
            AuthoredReference::Href(reference)
                if matches!(reference.context(), ReferenceContext::Navigation(_)) =>
            {
                Some(StructuralChange::NavigationReference(
                    AuthoredReference::Href(reference.clone()),
                ))
            }
            _ => None,
        }));
        structural_changes.extend(self.references().filter_map(|reference| {
            let AuthoredReference::Manifest(idref) = reference else {
                return None;
            };
            if !manifest_id_targets(idref, key, &declarations) {
                return None;
            }
            match idref.role() {
                ManifestRole::Fallback => {
                    Some(StructuralChange::FallbackReference(reference.clone()))
                }
                ManifestRole::MediaOverlay => {
                    Some(StructuralChange::MediaOverlayReference(reference.clone()))
                }
            }
        }));

        Ok(Impact {
            resource: key,
            incoming,
            outgoing_rebased: Vec::new(),
            structural_changes,
            incomplete_sources: self.incomplete_relationship_sources(),
        })
    }

    /// Derives advisory consequences of moving a snapshot resource to `destination`.
    pub fn impact_of_move(
        &self,
        key: ResourceOrdinal,
        destination: &EpubPath,
    ) -> Result<Impact, ImpactError> {
        let record = self
            .resources()
            .resource(key)
            .map_err(|_| ImpactError::UnknownOrdinal)?;
        if key == self.resources().package().ordinal() {
            return Err(ImpactError::PackageDocument(key));
        }
        if record.local_path() == Some(destination) {
            return Ok(Impact {
                resource: key,
                incoming: Vec::new(),
                outgoing_rebased: Vec::new(),
                structural_changes: Vec::new(),
                incomplete_sources: self.incomplete_relationship_sources(),
            });
        }

        let incoming = self
            .references()
            .filter(|reference| match reference {
                AuthoredReference::Href(href) => {
                    reference_targets_resource(reference, key) && href.source() != key
                }
                AuthoredReference::Manifest(_) => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        // Authored base elements are not retained by every extractor, so every relative
        // reference is a candidate, including fragment-only and query-only values.
        let outgoing_rebased = self
            .references()
            .filter(|reference| match reference {
                AuthoredReference::Href(href) => {
                    href.source() == key && is_relative_reference(href.declared())
                }
                AuthoredReference::Manifest(_) => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut structural_changes = record
            .declarations()
            .map(|declaration| StructuralChange::ManifestDeclaration(declaration.ordinal()))
            .collect::<Vec<_>>();
        structural_changes.extend(incoming.iter().filter_map(|reference| match reference {
            AuthoredReference::Href(reference)
                if matches!(reference.context(), ReferenceContext::Navigation(_)) =>
            {
                Some(StructuralChange::NavigationReference(
                    AuthoredReference::Href(reference.clone()),
                ))
            }
            _ => None,
        }));

        Ok(Impact {
            resource: key,
            incoming,
            outgoing_rebased,
            structural_changes,
            incomplete_sources: self.incomplete_relationship_sources(),
        })
    }

    fn incomplete_relationship_sources(&self) -> Vec<ReferenceSource> {
        let mut sources = Vec::new();
        let mut seen = HashSet::new();
        for coverage in self
            .coverage()
            .relationships()
            .iter()
            .filter(|coverage| !matches!(coverage.state(), CoverageState::Complete))
        {
            let source = match coverage.source() {
                RelationshipSource::Package => {
                    ReferenceSource::Resource(self.resources().package().ordinal())
                }
                RelationshipSource::Navigation(resource)
                | RelationshipSource::Ncx(resource)
                | RelationshipSource::Smil(resource)
                | RelationshipSource::Xhtml(resource)
                | RelationshipSource::Css(resource)
                | RelationshipSource::Svg(resource) => ReferenceSource::Resource(*resource),
            };
            push_unique(&mut sources, &mut seen, source);
        }
        for record in self
            .resources()
            .resources()
            .filter(|record| self.remote_relationships_unknown(*record))
        {
            push_unique(
                &mut sources,
                &mut seen,
                ReferenceSource::Resource(record.ordinal()),
            );
        }
        sources
    }

    fn remote_relationships_unknown(&self, record: ResourceRef<'_>) -> bool {
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
}

fn reference_targets_resource(reference: &AuthoredReference, target: ResourceOrdinal) -> bool {
    match reference {
        AuthoredReference::Href(reference) => match reference.target() {
            HrefTarget::Resource { resource, .. } | HrefTarget::Fragment { resource, .. } => {
                *resource == target
            }
            HrefTarget::Remote {
                declared_resource, ..
            } => *declared_resource == Some(target),
            HrefTarget::Data(_)
            | HrefTarget::External(_)
            | HrefTarget::MissingLocal(_)
            | HrefTarget::Invalid(_) => false,
        },
        AuthoredReference::Manifest(reference) => matches!(
            reference.target(),
            ManifestTarget::Declaration { resource: Some(resource), .. } if *resource == target
        ),
    }
}

fn reading_order_targets(
    entry: ReadingOrderOccurrenceRef<'_>,
    resource: ResourceOrdinal,
    declarations: &HashSet<ManifestOrdinal>,
) -> bool {
    match entry.target_row() {
        ReadingOrderTargetRow::Declaration {
            declaration,
            resource: target,
        } => {
            target.map(Into::into) == Some(resource)
                || declarations.contains(&(*declaration).into())
        }
        ReadingOrderTargetRow::AmbiguousManifestId { candidates } => candidates
            .iter()
            .any(|key| declarations.contains(&(*key).into())),
        ReadingOrderTargetRow::MissingIdref | ReadingOrderTargetRow::MissingManifestId => false,
    }
}

fn manifest_id_targets(
    reference: &ManifestReference,
    resource: ResourceOrdinal,
    declarations: &HashSet<ManifestOrdinal>,
) -> bool {
    match reference.target() {
        ManifestTarget::Declaration {
            declaration,
            resource: target,
        } => *target == Some(resource) || declarations.contains(declaration),
        ManifestTarget::Ambiguous { candidates } => {
            candidates.iter().any(|key| declarations.contains(key))
        }
        ManifestTarget::Missing => false,
    }
}

fn is_relative_reference(href: &AuthoredHref) -> bool {
    matches!(
        parse_href(href.clone()),
        ParsedHref::Local { .. } | ParsedHref::SameDocument { .. } | ParsedHref::Empty { .. }
    )
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
