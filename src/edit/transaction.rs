use crate::publication::{
    annotation::{
        is_protected_embedded_annotation_resource_path, provider_bytes_with_changes_bounded,
    },
    committed_bytes, invalid_navigation_href_error, structural_manifest_href_path,
};
use crate::{
    Epub,
    annotation::{
        AnnotationBundle, AnnotationBundleError, AnnotationSet, EmbeddedAnnotationsError,
        MAX_ANNOTATIONS_JSON_BYTES,
    },
    edit::{
        EditChange, EditError, EmbeddedAnnotationResourceRemoval, GuideHrefFailure,
        SelectionFailure, StructuralResourceKind,
        select::{
            InsertionTarget, ListSelector, ManifestItemSelector, MetaSelector,
            MetadataElementSelector, MetadataLinkSelector, MetadataNodeSelector, PointMatch,
            PointSelector, SelectionTarget, SpineItemRefSelector,
        },
    },
    navigation::{NavigationDocument, NavigationList, NavigationPoint, parse},
    package::{
        DC_NS, EpubVersion, LINK, META, OPF_NS, Package, PackageError, PackageField,
        legacy::ReferenceType,
        manifest::{KnownManifestProperty, ManifestItem},
        manifest_ids_equal,
        metadata::{DcElement, Element, Meta, MetaPropertyToken, MetadataLink},
        spine::{ItemRef, Linear},
    },
    publication::persistence::{ResourceChange, ResourceChanges},
    resource::provider::{ProviderIndex, ResourceProvider},
    resource::{
        AuthoredHref, EpubHref, EpubPath, MediaType, ResourceIndex, ResourceReadError,
        resolve_local_href_from_source,
    },
    semantics::{EpubStructuralSemantic, SemanticToken},
    string::EpubString,
    xml::decode_xml,
};
use std::collections::{BTreeSet, HashSet};

use xot::{Node, Xot};

type Result<T> = std::result::Result<T, EditError>;

const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const OPS_NS: &str = "http://www.idpf.org/2007/ops";
const ANNOTATIONS_JSON: &str = "META-INF/annotations.json";

/// Changes waiting to be applied, staged one call at a time.
///
/// Nothing touches the publication until you [`preview`](Self::preview) and commit; drop this
/// and the book is as it was.
#[derive(Debug)]
pub struct EpubEdit<'a, R: ResourceProvider> {
    pub(super) epub: &'a mut Epub<R>,
    pub(super) changes: ResourceChanges,
    pub(super) edit_changes: Vec<EditChange>,
    pub(super) package_override: Option<Package>,
    pub(super) navigation_override: Option<Option<NavigationDocument>>,
    semantic_structural_paths: BTreeSet<EpubPath>,
}

/// The book as it would be, checked and ready to apply.
///
/// Inspect it, then commit — or drop it and nothing happens.
#[derive(Debug)]
pub struct EpubEditPreview<'a, R: ResourceProvider> {
    pub(super) epub: &'a mut Epub<R>,
    resource_changes: ResourceChanges,
    provider_index: ProviderIndex,
    changes: Vec<EditChange>,
    pub(super) package: Package,
    pub(super) navigation: Option<NavigationDocument>,
    pub(super) resources: ResourceIndex,
}

fn guide_semantics(reference_type: &ReferenceType) -> Option<EpubStructuralSemantic> {
    match reference_type {
        ReferenceType::Cover => Some(EpubStructuralSemantic::Cover),
        ReferenceType::TitlePage => Some(EpubStructuralSemantic::TitlePage),
        ReferenceType::Toc => Some(EpubStructuralSemantic::Toc),
        ReferenceType::Index => Some(EpubStructuralSemantic::Index),
        ReferenceType::Glossary => Some(EpubStructuralSemantic::Glossary),
        ReferenceType::Acknowledgements => Some(EpubStructuralSemantic::Acknowledgments),
        ReferenceType::Bibliography => Some(EpubStructuralSemantic::Bibliography),
        ReferenceType::Colophon => Some(EpubStructuralSemantic::Colophon),
        ReferenceType::CopyrightPage => Some(EpubStructuralSemantic::CopyrightPage),
        ReferenceType::Epigraph => Some(EpubStructuralSemantic::Epigraph),
        ReferenceType::Foreword => Some(EpubStructuralSemantic::Foreword),
        ReferenceType::Preface => Some(EpubStructuralSemantic::Preface),
        ReferenceType::Dedication => Some(EpubStructuralSemantic::Dedication),
        ReferenceType::Loi => Some(EpubStructuralSemantic::Loi),
        ReferenceType::Lot => Some(EpubStructuralSemantic::Lot),
        ReferenceType::Text => Some(EpubStructuralSemantic::Bodymatter),
        ReferenceType::Notes | ReferenceType::Other(_) => None,
    }
}

fn upsert_modified_meta_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    modified: &EpubString,
    package_path: &EpubPath,
) -> Result<()> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let property_name = xot.add_name("property");
    let matches = xot
        .children(metadata)
        .filter(|child| {
            is_element_ns(xot, *child, OPF_NS, META)
                && xot
                    .get_attribute(*child, property_name)
                    .is_some_and(|property| property.eq_ignore_ascii_case("dcterms:modified"))
        })
        .collect::<Vec<_>>();
    if let Some(first) = matches.first().copied() {
        replace_text_content(xot, first, Some(modified), package_path)?;
        return Ok(());
    }

    let meta = Meta::new(
        MetaPropertyToken::try_new("dcterms:modified").expect("static string is non-empty"),
        modified.clone(),
    );
    append_meta_to_package_xml(xot, doc, &meta, package_path)
}

fn add_manifest_item_property_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    id: &str,
    property: KnownManifestProperty,
    package_path: &EpubPath,
) -> Result<()> {
    let item = find_manifest_item_node(xot, doc, id, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    let properties_name = xot.add_name("properties");
    let property = property.to_string();
    let known_property = property.parse::<KnownManifestProperty>().ok();
    let mut properties = xot
        .get_attribute(item, properties_name)
        .map(|value| {
            value
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let already_present = properties.iter().any(|value| {
        value == &property || value.parse::<KnownManifestProperty>().ok() == known_property
    });
    if !already_present {
        properties.push(property);
    }
    xot.set_attribute(item, properties_name, properties.join(" "));
    Ok(())
}

fn reject_mimetype_edit(path: &EpubPath) -> Result<()> {
    if path.as_str() == "mimetype" {
        return Err(EditError::MimetypeResourceEdit);
    }
    Ok(())
}

mod xml;
use xml::*;

fn unique_position(
    mut matches: impl Iterator<Item = usize>,
    target: impl Into<SelectionTarget>,
) -> Result<usize> {
    let Some(first) = matches.next() else {
        return Err(EditError::selection(target, SelectionFailure::NotFound));
    };
    if matches.next().is_some() {
        return Err(EditError::selection(target, SelectionFailure::Ambiguous));
    }
    Ok(first)
}

fn unique_manifest_item<'a>(
    package: &'a Package,
    selector: &ManifestItemSelector,
) -> Result<(usize, &'a ManifestItem)> {
    let items = package.manifest().items();
    if let ManifestItemSelector::Id(id) = selector {
        crate::package::normalize_manifest_id(id).map_err(PackageError::from)?;
    }
    let index = unique_position(
        items
            .iter()
            .enumerate()
            .filter(|(index, item)| match selector {
                ManifestItemSelector::Ordinal(ordinal) => ordinal.index() == *index,
                ManifestItemSelector::Id(id) => item
                    .id()
                    .is_some_and(|item_id| manifest_ids_equal(item_id, id)),
                ManifestItemSelector::Href(href) => item.href().as_ref() == Some(href),
                ManifestItemSelector::AuthoredHref(href) => item.authored_href() == Some(href),
            })
            .map(|(index, _)| index),
        selector.clone(),
    )?;
    Ok((index, &items[index]))
}

fn required_manifest_item_id(item: &ManifestItem) -> Result<&str> {
    item.id().ok_or_else(|| {
        PackageError::EmptyField {
            field: PackageField::ManifestItemId,
        }
        .into()
    })
}

fn required_spine_itemref_idref(itemref: &ItemRef) -> Result<&str> {
    itemref.idref().ok_or_else(|| {
        PackageError::EmptyField {
            field: PackageField::ItemRefIdref,
        }
        .into()
    })
}

/// Returns the selected itemref, which is guaranteed to carry an `idref`.
fn unique_spine_itemref<'a>(
    package: &'a Package,
    selector: &SpineItemRefSelector,
) -> Result<(usize, &'a ItemRef, &'a str)> {
    if let SpineItemRefSelector::Idref(idref) = selector {
        crate::package::normalize_manifest_id(idref).map_err(PackageError::from)?;
    }
    let itemrefs = package.spine().itemrefs();
    let index = unique_position(
        itemrefs
            .iter()
            .enumerate()
            .filter(|(index, itemref)| match selector {
                SpineItemRefSelector::Idref(idref) => itemref
                    .idref()
                    .is_some_and(|itemref_id| manifest_ids_equal(itemref_id, idref)),
                SpineItemRefSelector::Ordinal(ordinal) => ordinal.index() == *index,
            })
            .map(|(index, _)| index),
        selector.clone(),
    )?;
    let itemref = &itemrefs[index];
    let idref = itemref
        .idref()
        .ok_or_else(|| EditError::selection(selector.clone(), SelectionFailure::MissingIdentity))?;
    Ok((index, itemref, idref))
}

fn unique_metadata_element(package: &Package, selector: &MetadataElementSelector) -> Result<usize> {
    let elements = package.metadata().elements(selector.element);
    unique_position(
        elements
            .iter()
            .enumerate()
            .filter(|(index, element)| match &selector.node {
                MetadataNodeSelector::Index(expected) => index == expected,
                MetadataNodeSelector::Id(id) => element.id() == Some(id),
            })
            .map(|(index, _)| index),
        selector.clone(),
    )
}

fn unique_meta(package: &Package, selector: &MetaSelector) -> Result<usize> {
    unique_position(
        package
            .metadata()
            .meta()
            .iter()
            .enumerate()
            .filter(|(index, meta)| {
                let property = meta.property().map(|value| value.as_str());
                match selector {
                    MetaSelector::Index(expected) => index == expected,
                    MetaSelector::Id(id) => meta.id() == Some(id),
                    MetaSelector::Property(expected) => property == Some(expected.as_str()),
                    MetaSelector::PropertyRefines {
                        property: expected,
                        refines,
                    } => property == Some(expected.as_str()) && meta.refines() == Some(refines),
                }
            })
            .map(|(index, _)| index),
        selector.clone(),
    )
}

fn unique_metadata_link(package: &Package, selector: &MetadataLinkSelector) -> Result<usize> {
    unique_position(
        package
            .metadata()
            .link()
            .iter()
            .enumerate()
            .filter(|(index, link)| match selector {
                MetadataLinkSelector::Index(expected) => index == expected,
                MetadataLinkSelector::Id(id) => link.id() == Some(id),
                MetadataLinkSelector::Href(href) => link.href().as_ref() == Some(href),
                MetadataLinkSelector::AuthoredHref(href) => link.authored_href() == Some(href),
            })
            .map(|(index, _)| index),
        selector.clone(),
    )
}

fn annotation_epub_path(path: impl AsRef<str>) -> Result<EpubPath> {
    let path = path.as_ref();
    EpubPath::new(path).map_err(|_| {
        AnnotationBundleError::InvalidPath {
            path: path.to_string(),
        }
        .into()
    })
}

fn navigation_generate_error(
    nav_path: &EpubPath,
    error: crate::navigation::generate::NavigationGenerateError,
) -> EditError {
    use crate::navigation::generate::NavigationGenerateError;
    match error {
        NavigationGenerateError::CannotRebaseSameDocumentHref { href, .. }
        | NavigationGenerateError::InvalidRebasedHref { href, .. } => {
            EditError::NavigationHrefRebase { href }
        }
        other => EditError::structural_xml(nav_path, other),
    }
}

fn annotations_json_path() -> EpubPath {
    EpubPath::new(ANNOTATIONS_JSON).expect("static annotation path is valid")
}

#[derive(Debug, Clone)]
struct NavPointTarget<'a> {
    nav_path: EpubPath,
    list_index: usize,
    point_path: Vec<usize>,
    point: &'a NavigationPoint,
}

impl From<&NavPointTarget<'_>> for NavPointEditTarget {
    fn from(value: &NavPointTarget<'_>) -> Self {
        Self {
            nav_path: value.nav_path.clone(),
            list_index: value.list_index,
            point_path: value.point_path.clone(),
        }
    }
}

impl From<&NavInsertionResolved> for NavInsertionEditTarget {
    fn from(value: &NavInsertionResolved) -> Self {
        Self {
            nav_path: value.nav_path.clone(),
            list_index: value.list_index,
            parent_path: value.parent_path.clone(),
        }
    }
}

mod migration;
mod navigation;
mod opf;
mod preview;
mod resources;

impl<R: ResourceProvider> Epub<R> {
    /// Starts staging edits from the current committed in-memory state.
    pub fn edit(&mut self) -> EpubEdit<'_, R> {
        EpubEdit {
            epub: self,
            changes: ResourceChanges::new(),
            edit_changes: Vec::new(),
            package_override: None,
            navigation_override: None,
            semantic_structural_paths: BTreeSet::new(),
        }
    }
}

impl<R: ResourceProvider> EpubEditPreview<'_, R> {
    /// Returns navigation acquisition observations for the staged navigation document.
    pub fn navigation_loading(&self) -> crate::navigation::facts::NavigationLoadingFacts {
        self.epub.navigation_loading_after_edit(&self.navigation)
    }

    /// Resolves staged navigation hrefs against the staged resource inventory.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationPositionOverflow`](crate::navigation::facts::NavigationPositionOverflow)
    /// if a list or point position exceeds `u32`.
    pub fn navigation_targets(
        &self,
    ) -> std::result::Result<
        Vec<crate::navigation::facts::NavigationTargetFacts>,
        crate::navigation::facts::NavigationPositionOverflow,
    > {
        self.navigation.as_ref().map_or_else(
            || Ok(Vec::new()),
            |document| crate::navigation::facts::navigation_targets(document, &self.resources),
        )
    }

    /// Returns deterministic final resource changes, coalesced by canonical path.
    pub fn changes(&self) -> &[EditChange] {
        &self.changes
    }

    /// Returns the package model that would be installed by [`Self::commit`].
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// Returns the navigation model that [`Self::commit`] would install.
    pub fn navigation(&self) -> Option<&NavigationDocument> {
        self.navigation.as_ref()
    }

    /// Returns the resource inventory that would be installed by [`Self::commit`].
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    /// Loads the embedded annotation set from the previewed state.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddedAnnotationsError`] under the same conditions as
    /// [`Epub::embedded_annotations`].
    pub fn embedded_annotations(
        &self,
    ) -> std::result::Result<Option<AnnotationBundle>, EmbeddedAnnotationsError> {
        self.epub
            .embedded_annotations_with_changes(&self.resource_changes, &self.provider_index)
    }

    /// Applies the previewed state to the open publication, and tells you what changed.
    ///
    /// This updates the book in memory. [`Epub::export`](crate::Epub::export) writes it to a
    /// file.
    pub fn commit(self) -> Vec<EditChange> {
        self.epub.install_edit_preview(
            self.package,
            self.navigation,
            self.resource_changes,
            self.resources,
        );
        self.changes
    }
}
