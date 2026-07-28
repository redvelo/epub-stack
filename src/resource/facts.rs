//! Serializable resource topology detached from borrowed graph views.

use super::{
    DeclarationTargetRow, ManifestOrdinal, ProviderPresence, ReadingOrderOrdinal,
    ReadingOrderPresentation, ReadingOrderTargetRow, ResourceAddress, ResourceIndex,
    ResourceOrdinal, SelectionRows,
};

/// Path metadata supplied by the provider without reading resource bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProviderMetadataFacts {
    /// Provider-reported byte length encoded as a decimal string for JSON safety.
    pub size_bytes: Option<String>,
    /// Lowercase local path extension, when available.
    pub file_extension: Option<String>,
}

/// One distinct resolved resource in portable owned form.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ResourceFacts {
    /// Canonical local path or retained non-local address.
    pub address: ResourceAddress,
    /// Whether provider bytes apply and are currently present.
    pub presence: ProviderPresence,
    /// Provider metadata available without reading content.
    pub provider_metadata: ProviderMetadataFacts,
    /// Manifest declarations resolving to this resource, in manifest order.
    pub declarations: Vec<ManifestOrdinal>,
    /// Whether any declaration identifies XHTML.
    pub has_xhtml_declaration: bool,
    /// Whether any declaration identifies CSS.
    pub has_stylesheet_declaration: bool,
    /// Whether any declaration identifies SVG.
    pub has_svg_declaration: bool,
    /// Whether any declaration carries the `scripted` property.
    pub scripted: bool,
}

/// Resolution of one manifest item, aligned by manifest ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ManifestTargetFacts {
    /// The declaration resolves to a distinct resource.
    Resource {
        /// Resolved resource ordinal.
        resource: ResourceOrdinal,
    },
    /// The declaration omitted its href.
    MissingHref,
    /// The declaration's authored href cannot resolve to an accepted address.
    InvalidHref,
}

/// Resolution of one reading-order occurrence, aligned by occurrence ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ReadingOrderTargetFacts {
    /// A unique declaration was selected, with an optional usable resource.
    Declaration {
        /// Selected declaration ordinal.
        declaration: ManifestOrdinal,
        /// Resolved resource, absent for a missing or invalid declaration href.
        resource: Option<ResourceOrdinal>,
    },
    /// The itemref omitted its `idref`.
    MissingIdref,
    /// The authored `idref` matched no valid manifest ID.
    MissingManifestId,
    /// The authored `idref` matched multiple declarations.
    AmbiguousManifestId {
        /// Matching declaration ordinals in manifest order.
        candidates: Vec<ManifestOrdinal>,
    },
}

/// Resource resolution and presentation for one authored spine occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ReadingOrderFacts {
    /// Position in authored reading order.
    pub ordinal: ReadingOrderOrdinal,
    /// Declaration and resource resolution outcome.
    pub target: ReadingOrderTargetFacts,
    /// Occurrence-owned authored rendition presentation evidence.
    pub presentation: ReadingOrderPresentation,
}

/// Authored relationship used to select a structural resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum SelectionSource {
    /// The package path supplied when opening the rendition.
    PackagePath,
    /// The manifest `nav` property.
    EpubNavProperty,
    /// The manifest `cover-image` property.
    CoverImageProperty,
    /// EPUB 2 `meta name="cover"` metadata.
    Opf2CoverMetadata,
    /// The spine `toc` attribute.
    SpineToc,
}

/// Explicit structural-resource selection outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum SelectionFacts {
    /// No applicable authored selection evidence exists.
    Absent,
    /// One declaration or direct resource was selected.
    Selected {
        /// Relationship responsible for the selection.
        source: SelectionSource,
        /// Selected declaration, absent for the directly selected package resource.
        declaration: Option<ManifestOrdinal>,
        /// Resolved resource, absent when a selected declaration has no usable href.
        resource: Option<ResourceOrdinal>,
    },
    /// An authored ID relationship matched no declaration.
    UnresolvedAuthoredId {
        /// Relationship containing the unresolved ID.
        source: SelectionSource,
        /// Exact modeled authored ID text.
        authored_id: String,
    },
    /// More than one declaration satisfies the authored selection relationship.
    AmbiguousCandidateDeclarations {
        /// Relationship responsible for the ambiguity.
        source: SelectionSource,
        /// Candidate declaration ordinals in manifest order.
        candidates: Vec<ManifestOrdinal>,
    },
}

/// Explicit outcomes for publication structural-resource selections.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ResourceSelectionsFacts {
    /// Selected package document.
    pub package: SelectionFacts,
    /// Selected cover image.
    pub cover: SelectionFacts,
    /// Selected EPUB navigation document declaration.
    pub epub_nav: SelectionFacts,
    /// Selected EPUB 2 NCX declaration.
    pub ncx: SelectionFacts,
}

/// Portable resource topology for one committed publication state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ResourceIndexFacts {
    /// Distinct resources in deterministic index order.
    pub resources: Vec<ResourceFacts>,
    /// Manifest target outcomes aligned exactly with package manifest items.
    pub manifest_targets: Vec<ManifestTargetFacts>,
    /// Reading-order outcomes aligned exactly with package spine itemrefs.
    pub reading_order: Vec<ReadingOrderFacts>,
    /// Explicit package, cover, EPUB NAV, and NCX selections.
    pub selections: ResourceSelectionsFacts,
}

impl ResourceIndex {
    pub(crate) fn facts(&self) -> ResourceIndexFacts {
        let resource_ordinal = ResourceOrdinal::from_index;
        let manifest_ordinal = ManifestOrdinal::from_index;
        let resources = self
            .resources
            .iter()
            .map(|record| ResourceFacts {
                address: record.address.clone(),
                presence: record.presence,
                provider_metadata: ProviderMetadataFacts {
                    size_bytes: record.metadata.size_bytes.map(|size| size.to_string()),
                    file_extension: record.metadata.file_extension.clone(),
                },
                declarations: record
                    .declarations
                    .iter()
                    .map(|key| manifest_ordinal(key.0))
                    .collect(),
                has_xhtml_declaration: record.has_xhtml,
                has_stylesheet_declaration: record.has_stylesheet,
                has_svg_declaration: record.has_svg,
                scripted: record.scripted,
            })
            .collect();
        let manifest_targets = self
            .declarations
            .iter()
            .map(|declaration| match declaration.target {
                DeclarationTargetRow::Resource(key) => ManifestTargetFacts::Resource {
                    resource: resource_ordinal(key.0),
                },
                DeclarationTargetRow::MissingHref => ManifestTargetFacts::MissingHref,
                DeclarationTargetRow::InvalidHref(_) => ManifestTargetFacts::InvalidHref,
            })
            .collect();
        let reading_order = self
            .reading_order
            .iter()
            .enumerate()
            .map(|(index, entry)| ReadingOrderFacts {
                ordinal: ReadingOrderOrdinal::from_index(index),
                target: match &entry.target {
                    ReadingOrderTargetRow::Declaration {
                        declaration,
                        resource,
                    } => ReadingOrderTargetFacts::Declaration {
                        declaration: manifest_ordinal(declaration.0),
                        resource: resource.map(|key| resource_ordinal(key.0)),
                    },
                    ReadingOrderTargetRow::MissingIdref => ReadingOrderTargetFacts::MissingIdref,
                    ReadingOrderTargetRow::MissingManifestId => {
                        ReadingOrderTargetFacts::MissingManifestId
                    }
                    ReadingOrderTargetRow::AmbiguousManifestId { candidates } => {
                        ReadingOrderTargetFacts::AmbiguousManifestId {
                            candidates: candidates
                                .iter()
                                .map(|key| manifest_ordinal(key.0))
                                .collect(),
                        }
                    }
                },
                presentation: entry.presentation.clone(),
            })
            .collect();

        ResourceIndexFacts {
            resources,
            manifest_targets,
            reading_order,
            selections: self.selection_facts(),
        }
    }

    fn selection_facts(&self) -> ResourceSelectionsFacts {
        let convert = |selection: &SelectionRows| match selection {
            SelectionRows::Absent => SelectionFacts::Absent,
            SelectionRows::Selected {
                source,
                declaration,
                resource,
            } => SelectionFacts::Selected {
                source: *source,
                declaration: declaration.map(Into::into),
                resource: resource.map(Into::into),
            },
            SelectionRows::UnresolvedAuthoredId {
                source,
                authored_id,
            } => SelectionFacts::UnresolvedAuthoredId {
                source: *source,
                authored_id: authored_id.clone(),
            },
            SelectionRows::Ambiguous { source, candidates } => {
                SelectionFacts::AmbiguousCandidateDeclarations {
                    source: *source,
                    candidates: candidates.iter().copied().map(Into::into).collect(),
                }
            }
        };

        ResourceSelectionsFacts {
            package: convert(&self.selections.package),
            cover: convert(&self.selections.cover),
            epub_nav: convert(&self.selections.epub_nav),
            ncx: convert(&self.selections.ncx),
        }
    }
}
