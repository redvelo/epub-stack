//! Queries for media-overlay, incoming, outgoing, and broken authored links.

use super::PublicationAnalysis;
use super::reference::{
    AuthoredReference, HrefReference, HrefTarget, ManifestReference, ManifestRole, ManifestTarget,
};
use crate::media_overlay::MediaOverlayAssociationRef;
use crate::resource::{
    ManifestOrdinal, OrdinalOutOfBounds, ProviderPresence, ReadingOrderOccurrenceRef,
    ReadingOrderTargetRow, ResourceOrdinal,
};

impl PublicationAnalysis {
    /// Iterates reading-order occurrences with an authored media-overlay relationship.
    ///
    /// Each item provides the reading-order entry, content resource, authored manifest
    /// relationship, resolved overlay resource, and available SMIL facts. Missing and ambiguous
    /// overlay targets remain visible through the returned relationship.
    pub fn media_overlay_associations(
        &self,
    ) -> impl Iterator<Item = MediaOverlayAssociationRef<'_>> {
        self.resources()
            .reading_order()
            .filter_map(move |entry| self.media_overlay_association(entry))
    }

    pub(super) fn media_overlay_association<'a>(
        &'a self,
        entry: ReadingOrderOccurrenceRef<'a>,
    ) -> Option<MediaOverlayAssociationRef<'a>> {
        let ReadingOrderTargetRow::Declaration {
            declaration,
            resource,
        } = entry.target_row()
        else {
            return None;
        };
        let content_declaration = self.resources().declaration((*declaration).into()).ok()?;
        content_declaration.media_overlay()?;
        let reference = match self
            .media_overlay_references
            .get(&(*declaration).into())
            .and_then(|index| self.references.get(*index))?
        {
            AuthoredReference::Manifest(reference)
                if reference.source() == (*declaration).into()
                    && reference.role() == ManifestRole::MediaOverlay =>
            {
                reference
            }
            AuthoredReference::Href(_) | AuthoredReference::Manifest(_) => return None,
        };
        let content_resource = resource.and_then(|key| self.resources().resource(key.into()).ok());
        let (overlay_declaration, overlay_resource) = match reference.target() {
            ManifestTarget::Declaration {
                declaration,
                resource,
            } => (
                self.resources().declaration(*declaration).ok(),
                resource.and_then(|key| self.resources().resource(key).ok()),
            ),
            ManifestTarget::InvalidManifestIdref
            | ManifestTarget::Missing
            | ManifestTarget::Ambiguous { .. } => (None, None),
        };
        let overlay_resource_facts =
            overlay_resource.and_then(|resource| self.facts_for_row(resource.row()).ok());
        Some(MediaOverlayAssociationRef::new(
            entry,
            content_declaration,
            content_resource,
            reference,
            overlay_declaration,
            overlay_resource,
            overlay_resource_facts,
            &self.references,
        ))
    }

    /// Iterates authored links and relationships originating in `source`.
    ///
    /// `source` must be a resource ordinal from this analysis snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`OrdinalOutOfBounds`] when `source` does not identify a resource in this snapshot.
    pub fn references_from_resource(
        &self,
        source: ResourceOrdinal,
    ) -> Result<impl Iterator<Item = &HrefReference>, OrdinalOutOfBounds> {
        self.resources()
            .resource(source)
            .map_err(|_| OrdinalOutOfBounds)?;
        Ok(self.references().filter_map(move |reference| {
            let AuthoredReference::Href(reference) = reference else {
                return None;
            };
            (reference.source() == source).then_some(reference)
        }))
    }

    /// Iterates manifest relationships authored by `source`.
    ///
    /// `source` must be a manifest-declaration ordinal from this analysis snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`OrdinalOutOfBounds`] when `source` does not identify a declaration in this snapshot.
    pub fn references_from_declaration(
        &self,
        source: ManifestOrdinal,
    ) -> Result<impl Iterator<Item = &ManifestReference>, OrdinalOutOfBounds> {
        self.resources()
            .declaration(source)
            .map_err(|_| OrdinalOutOfBounds)?;
        Ok(self.references().filter_map(move |reference| {
            let AuthoredReference::Manifest(reference) = reference else {
                return None;
            };
            (reference.source() == source).then_some(reference)
        }))
    }

    /// Iterates authored links and relationships that resolve to `target`.
    ///
    /// This includes local resource and fragment hrefs, remote hrefs matched to a manifest
    /// declaration, and manifest ID references whose declaration resolves to the resource.
    /// `target` must be a resource ordinal from this analysis snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`OrdinalOutOfBounds`] when `target` does not identify a resource in this snapshot.
    pub fn references_to(
        &self,
        target: ResourceOrdinal,
    ) -> Result<impl Iterator<Item = &AuthoredReference>, OrdinalOutOfBounds> {
        self.resources()
            .resource(target)
            .map_err(|_| OrdinalOutOfBounds)?;
        Ok(self.references().filter(move |reference| match reference {
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
        }))
    }

    /// Iterates manifest relationships that resolve to declaration `target`.
    ///
    /// `target` must be a manifest-declaration ordinal from this analysis snapshot. Missing and
    /// ambiguous ID references do not match a declaration.
    ///
    /// # Errors
    ///
    /// Returns [`OrdinalOutOfBounds`] when `target` does not identify a declaration in this snapshot.
    pub fn references_to_declaration(
        &self,
        target: ManifestOrdinal,
    ) -> Result<impl Iterator<Item = &ManifestReference>, OrdinalOutOfBounds> {
        self.resources()
            .declaration(target)
            .map_err(|_| OrdinalOutOfBounds)?;
        Ok(self.references().filter_map(move |reference| {
            let AuthoredReference::Manifest(reference) = reference else {
                return None;
            };
            matches!(
                reference.target(),
                ManifestTarget::Declaration { declaration, .. } if *declaration == target
            )
            .then_some(reference)
        }))
    }

    /// Iterates authored references known to be broken in this analysis.
    ///
    /// A reference is broken when its local target is missing or invalid, its inspected
    /// fragment is known not to exist, its resolved provider resource is missing, or its
    /// manifest ID target is missing or ambiguous. Unknown fragment existence is not considered
    /// broken, and remote, data, and other external hrefs are not checked. Consult relationship
    /// and fragment coverage before treating this as a complete list.
    pub fn broken_references(&self) -> impl Iterator<Item = &AuthoredReference> {
        self.references().filter(|reference| match reference {
            AuthoredReference::Href(reference) => match reference.target() {
                HrefTarget::MissingLocal(_) | HrefTarget::Invalid(_) => true,
                HrefTarget::Fragment {
                    exists: Some(false),
                    ..
                } => true,
                HrefTarget::Resource { resource, .. } | HrefTarget::Fragment { resource, .. } => {
                    self.resources()
                        .resource(*resource)
                        .is_ok_and(|record| record.presence() == ProviderPresence::Missing)
                }
                HrefTarget::Remote { .. } | HrefTarget::Data(_) | HrefTarget::External(_) => false,
            },
            AuthoredReference::Manifest(reference) => matches!(
                reference.target(),
                ManifestTarget::InvalidManifestIdref
                    | ManifestTarget::Missing
                    | ManifestTarget::Ambiguous { .. }
            ),
        })
    }
}
