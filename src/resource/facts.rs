//! Serializable resource topology detached from borrowed graph views.

use super::{
    DeclarationTargetRow, ManifestIdrefResolution, ManifestOrdinal, ProviderPresence,
    ReadingOrderOrdinal, ReadingOrderPresentation, ReadingOrderTargetRow, ResourceAddress,
    ResourceIndex, ResourceOrdinal, SelectionRows,
};
use crate::package::normalize_manifest_id;
use std::collections::HashMap;

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

/// Resolution of one manifest declaration's authored `fallback` IDREF.
///
/// Entries are declaration targets rather than physical resources. This preserves duplicate
/// declarations even when they share one resolved resource.
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
pub enum ManifestFallbackTargetFacts {
    /// The declaration has no authored fallback.
    Absent,
    /// The authored IDREF uniquely selected a declaration.
    Declaration {
        /// Selected declaration ordinal.
        declaration: ManifestOrdinal,
    },
    /// The authored IDREF is not a valid normalized manifest ID.
    InvalidManifestIdref,
    /// The valid normalized IDREF matched no declaration.
    MissingManifestId,
    /// The valid normalized IDREF matched multiple declarations.
    AmbiguousManifestId,
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ManifestFallbackTargetFacts {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(
            tag = "state",
            rename_all = "kebab-case",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum Repr {
            Absent {},
            Declaration { declaration: ManifestOrdinal },
            InvalidManifestIdref {},
            MissingManifestId {},
            AmbiguousManifestId {},
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Absent {} => Self::Absent,
            Repr::Declaration { declaration } => Self::Declaration { declaration },
            Repr::InvalidManifestIdref {} => Self::InvalidManifestIdref,
            Repr::MissingManifestId {} => Self::MissingManifestId,
            Repr::AmbiguousManifestId {} => Self::AmbiguousManifestId,
        })
    }
}

/// Manifest fallback targets aligned exactly with package manifest declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
pub struct ManifestFallbackFacts(Vec<ManifestFallbackTargetFacts>);

/// Authored manifest declaration fields needed to validate fallback facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestFallbackDeclaration<'a> {
    id: Option<&'a str>,
    fallback: Option<&'a str>,
}

impl<'a> ManifestFallbackDeclaration<'a> {
    /// Borrows one declaration's exact authored `id` and `fallback` values.
    pub fn new(id: Option<&'a str>, fallback: Option<&'a str>) -> Self {
        Self { id, fallback }
    }
}

/// A serialized fallback snapshot does not match its authored declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ManifestFallbackValidationError {
    /// The facts and authored manifest have different declaration counts.
    #[error(
        "manifest fallback declaration count differs: facts={facts}, declarations={declarations}"
    )]
    DeclarationCountMismatch {
        /// Number of serialized fallback entries.
        facts: usize,
        /// Number of authored declarations.
        declarations: usize,
    },
    /// One serialized target differs from resolution of its authored fallback.
    #[error("manifest fallback facts differ at {declaration:?}")]
    TargetMismatch {
        /// Declaration whose fallback facts do not match.
        declaration: ManifestOrdinal,
    },
}

impl ManifestFallbackFacts {
    /// Returns the number of manifest-aligned fallback entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Reports whether there are no fallback entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the fallback target for one declaration ordinal.
    pub fn get(&self, declaration: ManifestOrdinal) -> Option<&ManifestFallbackTargetFacts> {
        self.0.get(declaration.index())
    }

    /// Validates every serialized fallback target against authored declaration IDs and IDREFs.
    ///
    /// ID normalization and resolution use the same rules as publication resource indexing.
    pub fn validate_against(
        &self,
        declarations: &[ManifestFallbackDeclaration<'_>],
    ) -> Result<(), ManifestFallbackValidationError> {
        if self.len() != declarations.len() {
            return Err(ManifestFallbackValidationError::DeclarationCountMismatch {
                facts: self.len(),
                declarations: declarations.len(),
            });
        }

        let mut by_id = HashMap::<&str, Vec<ManifestOrdinal>>::new();
        for (index, declaration) in declarations.iter().enumerate() {
            if let Some(id) = declaration.id.and_then(|id| normalize_manifest_id(id).ok()) {
                by_id
                    .entry(id)
                    .or_default()
                    .push(ManifestOrdinal(index as u32));
            }
        }

        for (index, declaration) in declarations.iter().enumerate() {
            let expected = match declaration.fallback {
                None => ManifestFallbackTargetFacts::Absent,
                Some(fallback) => match normalize_manifest_id(fallback) {
                    Err(_) => ManifestFallbackTargetFacts::InvalidManifestIdref,
                    Ok(fallback) => {
                        match by_id.get(fallback).map(Vec::as_slice).unwrap_or_default() {
                            [] => ManifestFallbackTargetFacts::MissingManifestId,
                            [target] => ManifestFallbackTargetFacts::Declaration {
                                declaration: *target,
                            },
                            _ => ManifestFallbackTargetFacts::AmbiguousManifestId,
                        }
                    }
                },
            };
            let ordinal = ManifestOrdinal(index as u32);
            if self.get(ordinal) != Some(&expected) {
                return Err(ManifestFallbackValidationError::TargetMismatch {
                    declaration: ordinal,
                });
            }
        }
        Ok(())
    }

    /// Traverses declaration-level fallbacks without applying media support policy.
    ///
    /// Visited declarations preserve traversal order and never repeat. A malformed start or edge
    /// is returned as a topology error rather than an authored unresolved outcome.
    pub fn traverse(
        &self,
        start: ManifestOrdinal,
    ) -> Result<ManifestFallbackTraversal, ManifestFallbackTopologyError> {
        if self.get(start).is_none() {
            return Err(ManifestFallbackTopologyError::StartOutOfRange { start });
        }

        let mut declarations = Vec::new();
        let mut visited = vec![false; self.len()];
        let mut current = start;

        loop {
            if visited[current.index()] {
                return Ok(ManifestFallbackTraversal {
                    declarations,
                    outcome: ManifestFallbackTraversalOutcome::Cycle {
                        repeated_declaration: current,
                    },
                });
            }
            visited[current.index()] = true;
            declarations.push(current);

            match self
                .get(current)
                .expect("the start and each followed edge were bounds checked")
            {
                ManifestFallbackTargetFacts::Absent => {
                    return Ok(ManifestFallbackTraversal {
                        declarations,
                        outcome: ManifestFallbackTraversalOutcome::End,
                    });
                }
                ManifestFallbackTargetFacts::Declaration { declaration } => {
                    if self.get(*declaration).is_none() {
                        return Err(ManifestFallbackTopologyError::EdgeOutOfRange {
                            declaration: current,
                            target: *declaration,
                        });
                    }
                    current = *declaration;
                }
                ManifestFallbackTargetFacts::InvalidManifestIdref => {
                    return Ok(ManifestFallbackTraversal {
                        declarations,
                        outcome: ManifestFallbackTraversalOutcome::UnresolvedReference {
                            declaration: current,
                            reason: ManifestFallbackUnresolvedReason::InvalidManifestIdref,
                        },
                    });
                }
                ManifestFallbackTargetFacts::MissingManifestId => {
                    return Ok(ManifestFallbackTraversal {
                        declarations,
                        outcome: ManifestFallbackTraversalOutcome::UnresolvedReference {
                            declaration: current,
                            reason: ManifestFallbackUnresolvedReason::MissingManifestId,
                        },
                    });
                }
                ManifestFallbackTargetFacts::AmbiguousManifestId => {
                    return Ok(ManifestFallbackTraversal {
                        declarations,
                        outcome: ManifestFallbackTraversalOutcome::UnresolvedReference {
                            declaration: current,
                            reason: ManifestFallbackUnresolvedReason::AmbiguousManifestId,
                        },
                    });
                }
            }
        }
    }
}

/// A malformed ordinal in manifest fallback topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ManifestFallbackTopologyError {
    /// The requested starting declaration is outside the aligned fallback facts.
    #[error("manifest fallback start is out of range: {start:?}")]
    StartOutOfRange {
        /// Out-of-range starting declaration.
        start: ManifestOrdinal,
    },
    /// A declaration fallback points outside the aligned fallback facts.
    #[error("manifest fallback edge from {declaration:?} is out of range: {target:?}")]
    EdgeOutOfRange {
        /// Declaration containing the malformed edge.
        declaration: ManifestOrdinal,
        /// Out-of-range target declaration.
        target: ManifestOrdinal,
    },
}

/// Reason manifest fallback traversal could not follow an authored reference.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(
        tag = "reason",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ManifestFallbackUnresolvedReason {
    /// The authored IDREF is not a valid normalized manifest ID.
    InvalidManifestIdref,
    /// The valid normalized IDREF matched no declaration.
    MissingManifestId,
    /// The valid normalized IDREF matched multiple declarations.
    AmbiguousManifestId,
}

/// Terminal outcome of a cycle-safe manifest fallback traversal.
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
pub enum ManifestFallbackTraversalOutcome {
    /// Traversal reached a declaration with no authored fallback.
    End,
    /// Traversal stopped at an authored unresolved edge.
    UnresolvedReference {
        /// Declaration whose fallback could not be followed.
        declaration: ManifestOrdinal,
        /// Why the edge could not be followed.
        reason: ManifestFallbackUnresolvedReason,
    },
    /// Traversal reached a declaration already present in the path.
    Cycle {
        /// Declaration that would have been visited again.
        repeated_declaration: ManifestOrdinal,
    },
}

/// Ordered declarations and terminal outcome from manifest fallback traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ManifestFallbackTraversal {
    /// Visited declarations in traversal order, including the starting declaration when valid.
    pub declarations: Vec<ManifestOrdinal>,
    /// Why traversal stopped.
    pub outcome: ManifestFallbackTraversalOutcome,
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
    /// Fallback target outcomes aligned exactly with package manifest items.
    pub manifest_fallbacks: ManifestFallbackFacts,
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
        let manifest_fallbacks = self
            .declarations
            .iter()
            .map(|declaration| {
                let Some(fallback) = declaration.fallback() else {
                    return ManifestFallbackTargetFacts::Absent;
                };
                match self.resolve_manifest_idref(fallback.as_str()) {
                    ManifestIdrefResolution::Invalid => {
                        ManifestFallbackTargetFacts::InvalidManifestIdref
                    }
                    ManifestIdrefResolution::Missing => {
                        ManifestFallbackTargetFacts::MissingManifestId
                    }
                    ManifestIdrefResolution::Unique(declaration) => {
                        ManifestFallbackTargetFacts::Declaration {
                            declaration: declaration.into(),
                        }
                    }
                    ManifestIdrefResolution::Ambiguous(_) => {
                        ManifestFallbackTargetFacts::AmbiguousManifestId
                    }
                }
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
            manifest_fallbacks: ManifestFallbackFacts(manifest_fallbacks),
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
