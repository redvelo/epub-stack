//! Queries for media-overlay associations and broken authored links.

use super::PublicationAnalysis;
use super::reference::{AuthoredReference, HrefTarget, ManifestRole, ManifestTarget};
use crate::media_overlay::MediaOverlayAssociationRef;
use crate::resource::{ProviderPresence, ReadingOrderOccurrenceRef};

impl PublicationAnalysis {
    /// Iterates reading-order occurrences with an authored media-overlay relationship.
    ///
    /// Includes missing and ambiguous overlay targets.
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
        let content_declaration = entry.declaration()?;
        content_declaration.media_overlay()?;
        let reference = self
            .declaration(content_declaration.ordinal())?
            .references()
            .find(|reference| reference.role() == ManifestRole::MediaOverlay)?;
        let (overlay_declaration, overlay_resource) = match reference.target() {
            ManifestTarget::Declaration {
                declaration,
                resource,
            } => (
                self.resources().declaration(*declaration),
                resource.map(|key| self.resource_analysis(key)),
            ),
            ManifestTarget::InvalidManifestIdref
            | ManifestTarget::Missing
            | ManifestTarget::Ambiguous { .. } => (None, None),
        };
        Some(MediaOverlayAssociationRef::new(
            entry,
            content_declaration,
            reference,
            overlay_declaration,
            overlay_resource,
        ))
    }

    /// Iterates authored references known to be broken in this analysis.
    ///
    /// A link counts as broken when it points at a file the book does not have, at a fragment
    /// known to be absent, or at a manifest ID that matches nothing or matches twice.
    ///
    /// Remote and `data:` links are never checked, and a fragment nobody looked inside is not
    /// assumed broken. An empty result means nothing was found, which is only as strong as
    /// [`Self::coverage`] says it is.
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
                        .is_some_and(|record| record.presence() == ProviderPresence::Missing)
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
