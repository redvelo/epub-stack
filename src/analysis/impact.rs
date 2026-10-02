//! Advisory consequences of resource edits.
//!
//! Impact queries identify incoming links, relative links affected by a move, and package or
//! navigation structures an application may need to update. They do not perform the edit.
//! Incomplete relationship coverage means additional consequences may exist.

use super::ResourceAnalysisRef;
use super::reference::{
    AuthoredReference, HrefReference, HrefRole, ManifestReference, ManifestRole, ManifestTarget,
};
use crate::resource::{
    AuthoredHref, EpubPath, IdrefTarget, ManifestOrdinal, ParsedHref, ReadingOrderOccurrenceRef,
    ReadingOrderOrdinal, ResourceOrdinal, ResourceRef, parse_href,
};
use std::collections::HashSet;

/// Links and publication structures potentially affected by removing or moving a resource.
///
/// Any document whose relationships were not fully extracted can hide an incoming reference, so
/// impact is complete only when relationship coverage is. Unlike
/// [`Closure::incomplete_sources`](super::dependency::Closure::incomplete_sources), which reports
/// the producers actually reached from a root, that set is publication-wide: read it from
/// [`Coverage::incomplete_relationships`](super::coverage::Coverage::incomplete_relationships).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Impact {
    pub(crate) resource: ResourceOrdinal,
    pub(crate) incoming: Vec<AuthoredReference>,
    pub(crate) outgoing_rebased: Vec<AuthoredReference>,
    pub(crate) structural_changes: Vec<StructuralChange>,
}

/// Package-document changes require package-aware edit planning rather than resource impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("package document impact is not represented by resource impact")]
pub struct ImpactError;

impl Impact {
    /// Which resource this is about.
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

impl<'a> ResourceAnalysisRef<'a> {
    /// Derives advisory consequences of removing this resource.
    ///
    /// # Errors
    ///
    /// Returns [`ImpactError`] for the package document.
    pub fn impact_of_removal(self) -> Result<Impact, ImpactError> {
        let record = self.resource();
        let analysis = self.analysis;
        if record == analysis.resources().package() {
            return Err(ImpactError);
        }
        let key = record.ordinal();
        let declarations = record
            .declarations()
            .map(|declaration| declaration.ordinal())
            .collect::<HashSet<_>>();
        let mut slots = self.reference_index().to.clone();
        slots.extend(
            analysis
                .references
                .iter()
                .enumerate()
                .filter_map(|(slot, reference)| {
                    matches!(reference, AuthoredReference::Manifest(idref)
                if matches!(idref.target(), ManifestTarget::Ambiguous { candidates }
                    if candidates.iter().any(|key| declarations.contains(key))))
                    .then_some(slot)
                }),
        );
        slots.sort_unstable();
        slots.dedup();
        let incoming = analysis.references_at(&slots).cloned().collect::<Vec<_>>();
        let mut structural_changes = record
            .declarations()
            .map(|declaration| StructuralChange::ManifestDeclaration(declaration.ordinal()))
            .collect::<Vec<_>>();
        structural_changes.extend(analysis.resources().reading_order().filter_map(|entry| {
            reading_order_targets(entry, key, &declarations)
                .then_some(StructuralChange::ReadingOrderOccurrence(entry.ordinal()))
        }));
        structural_changes.extend(navigation_changes(&incoming));
        structural_changes.extend(incoming.iter().filter_map(|reference| {
            let AuthoredReference::Manifest(idref) = reference else {
                return None;
            };
            manifest_id_targets(idref, key, &declarations).then(|| match idref.role() {
                ManifestRole::Fallback => StructuralChange::FallbackReference(reference.clone()),
                ManifestRole::MediaOverlay => {
                    StructuralChange::MediaOverlayReference(reference.clone())
                }
            })
        }));

        Ok(Impact {
            resource: key,
            incoming,
            outgoing_rebased: Vec::new(),
            structural_changes,
        })
    }

    /// Derives advisory consequences of moving this resource to `destination`.
    ///
    /// # Errors
    ///
    /// Returns [`ImpactError`] for the package document.
    pub fn impact_of_move(self, destination: &EpubPath) -> Result<Impact, ImpactError> {
        let record = self.resource();
        let analysis = self.analysis;
        if record == analysis.resources().package() {
            return Err(ImpactError);
        }
        let key = record.ordinal();
        if record.local_path() == Some(destination) {
            return Ok(Impact {
                resource: key,
                incoming: Vec::new(),
                outgoing_rebased: Vec::new(),
                structural_changes: Vec::new(),
            });
        }

        let incoming = self
            .incoming_references()
            .filter(|reference| {
                matches!(reference, AuthoredReference::Href(href) if href.source() != key)
            })
            .cloned()
            .collect::<Vec<_>>();
        // Authored base elements are not retained by every extractor, so every relative
        // reference is a candidate, including fragment-only and query-only values.
        let outgoing_rebased = self
            .references()
            .filter(|href| is_relative_reference(href.declared()))
            .map(|href| AuthoredReference::Href(href.clone()))
            .collect::<Vec<_>>();
        let mut structural_changes = record
            .declarations()
            .map(|declaration| StructuralChange::ManifestDeclaration(declaration.ordinal()))
            .collect::<Vec<_>>();
        structural_changes.extend(navigation_changes(&incoming));

        Ok(Impact {
            resource: key,
            incoming,
            outgoing_rebased,
            structural_changes,
        })
    }
}

fn navigation_changes(
    incoming: &[AuthoredReference],
) -> impl Iterator<Item = StructuralChange> + '_ {
    incoming.iter().filter_map(|reference| match reference {
        AuthoredReference::Href(href) if is_navigation_reference(href) => {
            Some(StructuralChange::NavigationReference(reference.clone()))
        }
        _ => None,
    })
}

fn is_navigation_reference(href: &HrefReference) -> bool {
    matches!(
        href.role(),
        HrefRole::Toc | HrefRole::PageList | HrefRole::Landmark | HrefRole::Ncx
    )
}

fn reading_order_targets(
    entry: ReadingOrderOccurrenceRef<'_>,
    resource: ResourceOrdinal,
    declarations: &HashSet<ManifestOrdinal>,
) -> bool {
    match entry.target() {
        Some(IdrefTarget::Declaration(declaration)) => {
            entry.resource().map(ResourceRef::ordinal) == Some(resource)
                || declarations.contains(declaration)
        }
        Some(IdrefTarget::Ambiguous(candidates)) => {
            candidates.iter().any(|key| declarations.contains(key))
        }
        Some(IdrefTarget::Invalid | IdrefTarget::Missing) | None => false,
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
        ManifestTarget::InvalidManifestIdref | ManifestTarget::Missing => false,
    }
}

fn is_relative_reference(href: &AuthoredHref) -> bool {
    matches!(
        parse_href(href.clone()),
        ParsedHref::Local { .. } | ParsedHref::SameDocument { .. } | ParsedHref::Empty { .. }
    )
}
