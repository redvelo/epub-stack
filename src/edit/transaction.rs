use crate::publication::{
    annotation::{
        is_protected_embedded_annotation_resource_path, provider_bytes_with_changes_bounded,
    },
    invalid_navigation_href_error, resource_bytes_from_parts, structural_manifest_href_path,
};
use crate::{
    Epub,
    annotation::{
        AnnotationBundle, AnnotationBundleError, AnnotationError, AnnotationSet,
        MAX_ANNOTATIONS_JSON_BYTES, normalize_annotation_resource_path,
    },
    edit::{
        EditChange, StructuralEditKind,
        annotation::EmbeddedAnnotationResourceRemoval,
        navigation::{InsertionTarget, ListSelector, PointMatch, PointSelector},
        package::{
            ManifestItemSelector, MetaSelector, MetadataElementSelector, MetadataLinkSelector,
            MetadataNodeSelector, SpineItemRefSelector,
        },
        report::EditReport,
    },
    edit::{EditError, SelectionFailure, StructuralResourceKind},
    navigation::{
        Navigation, NavigationDocument, NavigationList, NavigationPoint, NavigationSemanticSource,
        NavigationSource, parse,
    },
    package::{
        DC_NS, EpubVersion, LINK, META, OPF_NS, Package, PackageError,
        legacy::ReferenceType,
        manifest::{KnownManifestProperty, ManifestItem},
        manifest_ids_equal,
        metadata::{Element, Meta, MetaPropertyToken, MetadataElement, MetadataLink},
        spine::{ItemRef, Linear},
    },
    publication::persistence::{ResourceChange, ResourceChanges},
    resource::provider::ResourceProvider,
    resource::{
        AuthoredHref, EpubHref, EpubPath, MediaType, ResourceAddress, ResourceIndex,
        ResourceSelector, resolve_local_href_from_source,
    },
    semantics::EpubStructuralSemantic,
    string::EpubString,
    xml::decode_xml,
};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use xot::{Node, Xot};

type Result<T> = std::result::Result<T, EditError>;

const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const OPS_NS: &str = "http://www.idpf.org/2007/ops";

/// Stages a chain of publication changes for preview.
///
/// Staging does not mutate the live [`Epub`]. Call [`Self::preview`] to validate and inspect the
/// proposed state, then commit the returned preview to memory.
#[derive(Debug)]
pub struct EpubEdit<'a, R: ResourceProvider> {
    pub(super) epub: &'a mut Epub<R>,
    pub(super) changes: ResourceChanges,
    pub(super) edit_changes: Vec<EditChange>,
    embedded_annotation_orphan_paths: HashSet<String>,
    annotations_touched: bool,
    pub(super) package_override: Option<Package>,
    pub(super) navigation_override: Option<Navigation>,
    structural_edits: BTreeMap<EpubPath, StructuralEdit>,
}

/// Exposes a validated publication snapshot that can be committed in memory.
///
/// Dropping a preview leaves the live publication unchanged. Snapshot-local resource
/// identities must not be retained after the preview is consumed.
#[derive(Debug)]
pub struct EpubEditPreview<'a, R: ResourceProvider> {
    pub(super) epub: &'a mut Epub<R>,
    resource_changes: ResourceChanges,
    changes: Vec<EditChange>,
    pub(super) package: Package,
    pub(super) navigation: Navigation,
    pub(super) annotations: Option<AnnotationBundle>,
    pub(super) resources: ResourceIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuralEdit {
    Upsert,
    Remove,
    Overwritten,
}

#[derive(Debug, thiserror::Error)]
enum ManifestItemLookupError {
    #[error("Manifest item not found for id: {0}")]
    IdNotFound(String),
    #[error("Manifest item lookup for id: {0} is ambiguous")]
    IdAmbiguous(String),
    #[error("Manifest item not found for href: {0}")]
    HrefNotFound(String),
    #[error("Manifest item lookup for href: {0} is ambiguous")]
    HrefAmbiguous(String),
    #[error("Manifest item not found for authored href: {0}")]
    AuthoredHrefNotFound(String),
    #[error("Manifest item lookup for authored href: {0} is ambiguous")]
    AuthoredHrefAmbiguous(String),
    #[error("Selected manifest item has no id")]
    MissingId,
}

impl From<ManifestItemLookupError> for EditError {
    fn from(error: ManifestItemLookupError) -> Self {
        let (selector, failure) = match error {
            ManifestItemLookupError::IdNotFound(value) => {
                (format!("id {value}"), SelectionFailure::NotFound)
            }
            ManifestItemLookupError::IdAmbiguous(value) => {
                (format!("id {value}"), SelectionFailure::Ambiguous)
            }
            ManifestItemLookupError::HrefNotFound(value) => {
                (format!("href {value}"), SelectionFailure::NotFound)
            }
            ManifestItemLookupError::HrefAmbiguous(value) => {
                (format!("href {value}"), SelectionFailure::Ambiguous)
            }
            ManifestItemLookupError::AuthoredHrefNotFound(value) => {
                (format!("authored href {value}"), SelectionFailure::NotFound)
            }
            ManifestItemLookupError::AuthoredHrefAmbiguous(value) => (
                format!("authored href {value}"),
                SelectionFailure::Ambiguous,
            ),
            ManifestItemLookupError::MissingId => (
                "selected item".to_string(),
                SelectionFailure::MissingIdentity,
            ),
        };
        Self::Selection {
            target: "manifest item",
            selector,
            failure,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum SpineItemRefLookupError {
    #[error("Spine itemref not found for idref: {0}")]
    IdrefNotFound(String),
    #[error("Spine itemref lookup for idref: {0} is ambiguous")]
    IdrefAmbiguous(String),
    #[error("Spine itemref not found for index: {0}")]
    IndexNotFound(String),
    #[error("Spine itemref lookup for index: {0} is ambiguous")]
    IndexAmbiguous(String),
    #[error("Selected spine itemref at index {0} has no idref")]
    MissingIdref(usize),
}

impl From<SpineItemRefLookupError> for EditError {
    fn from(error: SpineItemRefLookupError) -> Self {
        let (selector, failure) = match error {
            SpineItemRefLookupError::IdrefNotFound(value) => {
                (format!("idref {value}"), SelectionFailure::NotFound)
            }
            SpineItemRefLookupError::IdrefAmbiguous(value) => {
                (format!("idref {value}"), SelectionFailure::Ambiguous)
            }
            SpineItemRefLookupError::IndexNotFound(value) => {
                (format!("index {value}"), SelectionFailure::NotFound)
            }
            SpineItemRefLookupError::IndexAmbiguous(value) => {
                (format!("index {value}"), SelectionFailure::Ambiguous)
            }
            SpineItemRefLookupError::MissingIdref(index) => {
                (format!("index {index}"), SelectionFailure::MissingIdentity)
            }
        };
        Self::Selection {
            target: "spine itemref",
            selector,
            failure,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum MetadataLookupError {
    #[error("Metadata {kind} not found for id: {value}")]
    IdNotFound { kind: String, value: String },
    #[error("Metadata {kind} lookup for id is ambiguous: {value}")]
    IdAmbiguous { kind: String, value: String },
    #[error("Metadata {kind} not found for index: {index}")]
    IndexNotFound { kind: String, index: usize },
    #[error("Metadata {kind} not found for property: {value}")]
    PropertyNotFound { kind: String, value: String },
    #[error("Metadata {kind} lookup for property is ambiguous: {value}")]
    PropertyAmbiguous { kind: String, value: String },
    #[error("Metadata {kind} not found for href: {value}")]
    HrefNotFound { kind: String, value: String },
    #[error("Metadata {kind} lookup for href is ambiguous: {value}")]
    HrefAmbiguous { kind: String, value: String },
    #[error("Metadata {kind} not found for authored href: {value}")]
    AuthoredHrefNotFound { kind: String, value: String },
    #[error("Metadata {kind} lookup for authored href is ambiguous: {value}")]
    AuthoredHrefAmbiguous { kind: String, value: String },
}

impl From<MetadataLookupError> for EditError {
    fn from(error: MetadataLookupError) -> Self {
        let (kind, selector, failure) = match error {
            MetadataLookupError::IdNotFound { kind, value } => {
                (kind, format!("id {value}"), SelectionFailure::NotFound)
            }
            MetadataLookupError::IdAmbiguous { kind, value } => {
                (kind, format!("id {value}"), SelectionFailure::Ambiguous)
            }
            MetadataLookupError::IndexNotFound { kind, index } => {
                (kind, format!("index {index}"), SelectionFailure::NotFound)
            }
            MetadataLookupError::PropertyNotFound { kind, value } => (
                kind,
                format!("property {value}"),
                SelectionFailure::NotFound,
            ),
            MetadataLookupError::PropertyAmbiguous { kind, value } => (
                kind,
                format!("property {value}"),
                SelectionFailure::Ambiguous,
            ),
            MetadataLookupError::HrefNotFound { kind, value } => {
                (kind, format!("href {value}"), SelectionFailure::NotFound)
            }
            MetadataLookupError::HrefAmbiguous { kind, value } => {
                (kind, format!("href {value}"), SelectionFailure::Ambiguous)
            }
            MetadataLookupError::AuthoredHrefNotFound { kind, value } => (
                kind,
                format!("authored href {value}"),
                SelectionFailure::NotFound,
            ),
            MetadataLookupError::AuthoredHrefAmbiguous { kind, value } => (
                kind,
                format!("authored href {value}"),
                SelectionFailure::Ambiguous,
            ),
        };
        Self::Selection {
            target: "metadata",
            selector: format!("{kind} {selector}"),
            failure,
        }
    }
}

fn guide_semantics(reference_type: ReferenceType) -> Option<EpubStructuralSemantic> {
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
        MetaPropertyToken::raw("dcterms:modified").expect("static string is non-empty"),
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
    let item = find_manifest_item_node(xot, doc, id, package_path)?.ok_or_else(|| {
        EditError::StructuralXml {
            path: package_path.clone(),
            message: format!("manifest item {id} not found in package XML"),
        }
    })?;
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

fn merge_resource_changes_into(
    target: &mut ResourceChanges,
    source: &ResourceChanges,
) -> Result<()> {
    for (path, change) in source.entries() {
        match change {
            ResourceChange::Upsert(bytes) => target.upsert(path.clone(), bytes.clone()),
            ResourceChange::Remove => target.remove(path.clone()),
        }
    }
    Ok(())
}

fn merged_resource_changes(
    base: &ResourceChanges,
    overlay: &ResourceChanges,
) -> Result<ResourceChanges> {
    let mut merged = base.clone();
    merge_resource_changes_into(&mut merged, overlay)?;
    Ok(merged)
}

fn coalesced_edit_changes(changes: Vec<EditChange>) -> Vec<EditChange> {
    let mut by_path = BTreeMap::new();
    for change in changes {
        by_path.insert(change.path().clone(), change);
    }
    by_path.into_values().collect()
}

fn reject_mimetype_edit(path: &EpubPath) -> Result<()> {
    if path.as_str() == "mimetype" {
        return Err(EditError::MimetypeResourceEdit);
    }
    Ok(())
}

fn structural_xml_operation(
    path: EpubPath,
    source: impl std::error::Error + Send + Sync + 'static,
) -> EditError {
    EditError::StructuralXmlOperation {
        path,
        source: crate::edit::StructuralXmlOperationError::new(source),
    }
}

mod xml;
use xml::*;

fn unique_manifest_item<'a>(
    package: &'a Package,
    selector: &ManifestItemSelector,
) -> Result<(usize, &'a ManifestItem)> {
    let errors = manifest_selector_errors(selector);
    let matches = package
        .manifest()
        .items()
        .iter()
        .enumerate()
        .filter(|(_, item)| manifest_item_matches_selector(item, selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(*item),
        [] => Err((errors.not_found)(errors.value).into()),
        _ => Err((errors.ambiguous)(errors.value).into()),
    }
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

fn manifest_item_matches_selector(item: &ManifestItem, selector: &ManifestItemSelector) -> bool {
    match selector {
        ManifestItemSelector::Id(id) => item
            .id()
            .is_some_and(|item_id| manifest_ids_equal(item_id, id)),
        ManifestItemSelector::Href(href) => item.href().as_ref() == Some(href),
        ManifestItemSelector::AuthoredHref(href) => item.authored_href() == Some(href),
    }
}

fn selected_manifest_item_id(item: &ManifestItem) -> Result<&str> {
    item.id()
        .ok_or_else(|| ManifestItemLookupError::MissingId.into())
}

fn required_manifest_item_id(item: &ManifestItem) -> Result<&str> {
    item.id().ok_or_else(|| {
        PackageError::EmptyField {
            field: "manifest item id",
        }
        .into()
    })
}

fn required_spine_itemref_idref(itemref: &ItemRef) -> Result<&str> {
    itemref.idref().ok_or_else(|| {
        PackageError::EmptyField {
            field: "itemref idref",
        }
        .into()
    })
}

struct ManifestSelectorErrors {
    not_found: fn(String) -> ManifestItemLookupError,
    ambiguous: fn(String) -> ManifestItemLookupError,
    value: String,
}

fn manifest_selector_errors(selector: &ManifestItemSelector) -> ManifestSelectorErrors {
    match selector {
        ManifestItemSelector::Id(id) => ManifestSelectorErrors {
            not_found: ManifestItemLookupError::IdNotFound,
            ambiguous: ManifestItemLookupError::IdAmbiguous,
            value: id.to_string(),
        },
        ManifestItemSelector::Href(href) => ManifestSelectorErrors {
            not_found: ManifestItemLookupError::HrefNotFound,
            ambiguous: ManifestItemLookupError::HrefAmbiguous,
            value: href.to_string(),
        },
        ManifestItemSelector::AuthoredHref(href) => ManifestSelectorErrors {
            not_found: ManifestItemLookupError::AuthoredHrefNotFound,
            ambiguous: ManifestItemLookupError::AuthoredHrefAmbiguous,
            value: href.to_string(),
        },
    }
}

fn unique_spine_itemref<'a>(
    package: &'a Package,
    selector: &SpineItemRefSelector,
) -> Result<(usize, &'a ItemRef)> {
    let errors = spine_selector_errors(selector);
    let matches = package
        .spine()
        .itemrefs()
        .iter()
        .enumerate()
        .filter(|(index, itemref)| spine_itemref_matches_selector(*index, itemref, selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [(index, itemref)] => {
            if itemref.idref().is_none() {
                return Err(SpineItemRefLookupError::MissingIdref(*index).into());
            }
            Ok((*index, *itemref))
        }
        [] => Err((errors.not_found)(errors.value).into()),
        _ => Err((errors.ambiguous)(errors.value).into()),
    }
}

fn unique_metadata_element(
    package: &Package,
    selector: &MetadataElementSelector,
) -> Result<(&'static str, usize)> {
    let local_name = metadata_element_selector_local_name(selector);
    let elements = metadata_elements(package, local_name);
    match metadata_element_node_selector(selector) {
        MetadataNodeSelector::Index(index) => {
            if *index < elements.len() {
                Ok((local_name, *index))
            } else {
                Err(MetadataLookupError::IndexNotFound {
                    kind: local_name.to_string(),
                    index: *index,
                }
                .into())
            }
        }
        MetadataNodeSelector::Id(id) => {
            let matches = elements
                .iter()
                .enumerate()
                .filter(|(_, element)| element.id() == Some(id))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(
                matches,
                local_name,
                MetadataSelectorDisplay::Id(id.to_string()),
            )
            .map(|index| (local_name, index))
        }
    }
}

fn unique_meta(package: &Package, selector: &MetaSelector) -> Result<usize> {
    let metas = package.metadata().meta();
    match selector {
        MetaSelector::Index(index) => {
            if *index < metas.len() {
                Ok(*index)
            } else {
                Err(MetadataLookupError::IndexNotFound {
                    kind: META.to_string(),
                    index: *index,
                }
                .into())
            }
        }
        MetaSelector::Id(id) => {
            let matches = metas
                .iter()
                .enumerate()
                .filter(|(_, meta)| meta.id() == Some(id))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(matches, META, MetadataSelectorDisplay::Id(id.to_string()))
        }
        MetaSelector::Property(property) => {
            let matches = metas
                .iter()
                .enumerate()
                .filter(|(_, meta)| {
                    meta.property().map(|value| value.as_str()) == Some(property.as_str())
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(
                matches,
                META,
                MetadataSelectorDisplay::Property(property.to_string()),
            )
        }
        MetaSelector::PropertyRefines { property, refines } => {
            let matches = metas
                .iter()
                .enumerate()
                .filter(|(_, meta)| {
                    meta.property().map(|value| value.as_str()) == Some(property.as_str())
                        && meta.refines() == Some(refines)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(
                matches,
                META,
                MetadataSelectorDisplay::Property(format!("{} refines {}", property, refines)),
            )
        }
    }
}

fn unique_metadata_link(package: &Package, selector: &MetadataLinkSelector) -> Result<usize> {
    let links = package.metadata().link();
    match selector {
        MetadataLinkSelector::Index(index) => {
            if *index < links.len() {
                Ok(*index)
            } else {
                Err(MetadataLookupError::IndexNotFound {
                    kind: LINK.to_string(),
                    index: *index,
                }
                .into())
            }
        }
        MetadataLinkSelector::Id(id) => {
            let matches = links
                .iter()
                .enumerate()
                .filter(|(_, link)| link.id() == Some(id))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(matches, LINK, MetadataSelectorDisplay::Id(id.to_string()))
        }
        MetadataLinkSelector::Href(href) => {
            let matches = links
                .iter()
                .enumerate()
                .filter(|(_, link)| link.href().as_ref() == Some(href))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(
                matches,
                LINK,
                MetadataSelectorDisplay::Href(href.to_string()),
            )
        }
        MetadataLinkSelector::AuthoredHref(href) => {
            let matches = links
                .iter()
                .enumerate()
                .filter(|(_, link)| link.authored_href() == Some(href))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            unique_metadata_index(
                matches,
                LINK,
                MetadataSelectorDisplay::AuthoredHref(href.to_string()),
            )
        }
    }
}

enum MetadataSelectorDisplay {
    Id(String),
    Property(String),
    Href(String),
    AuthoredHref(String),
}

fn unique_metadata_index(
    matches: Vec<usize>,
    kind: &str,
    selector: MetadataSelectorDisplay,
) -> Result<usize> {
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(metadata_not_found_error(kind, selector).into()),
        _ => Err(metadata_ambiguous_error(kind, selector).into()),
    }
}

fn metadata_not_found_error(kind: &str, selector: MetadataSelectorDisplay) -> MetadataLookupError {
    match selector {
        MetadataSelectorDisplay::Id(value) => MetadataLookupError::IdNotFound {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::Property(value) => MetadataLookupError::PropertyNotFound {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::Href(value) => MetadataLookupError::HrefNotFound {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::AuthoredHref(value) => MetadataLookupError::AuthoredHrefNotFound {
            kind: kind.to_string(),
            value,
        },
    }
}

fn metadata_ambiguous_error(kind: &str, selector: MetadataSelectorDisplay) -> MetadataLookupError {
    match selector {
        MetadataSelectorDisplay::Id(value) => MetadataLookupError::IdAmbiguous {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::Property(value) => MetadataLookupError::PropertyAmbiguous {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::Href(value) => MetadataLookupError::HrefAmbiguous {
            kind: kind.to_string(),
            value,
        },
        MetadataSelectorDisplay::AuthoredHref(value) => {
            MetadataLookupError::AuthoredHrefAmbiguous {
                kind: kind.to_string(),
                value,
            }
        }
    }
}

fn metadata_element_selector_local_name(selector: &MetadataElementSelector) -> &'static str {
    match selector {
        MetadataElementSelector::Identifier(_) => "identifier",
        MetadataElementSelector::Title(_) => "title",
        MetadataElementSelector::Language(_) => "language",
        MetadataElementSelector::Contributor(_) => "contributor",
        MetadataElementSelector::Coverage(_) => "coverage",
        MetadataElementSelector::Creator(_) => "creator",
        MetadataElementSelector::Date(_) => "date",
        MetadataElementSelector::Description(_) => "description",
        MetadataElementSelector::Format(_) => "format",
        MetadataElementSelector::Publisher(_) => "publisher",
        MetadataElementSelector::Relation(_) => "relation",
        MetadataElementSelector::Rights(_) => "rights",
        MetadataElementSelector::Source(_) => "source",
        MetadataElementSelector::Subject(_) => "subject",
        MetadataElementSelector::Type(_) => "type",
    }
}

fn metadata_element_node_selector(selector: &MetadataElementSelector) -> &MetadataNodeSelector {
    match selector {
        MetadataElementSelector::Identifier(selector)
        | MetadataElementSelector::Title(selector)
        | MetadataElementSelector::Language(selector)
        | MetadataElementSelector::Contributor(selector)
        | MetadataElementSelector::Coverage(selector)
        | MetadataElementSelector::Creator(selector)
        | MetadataElementSelector::Date(selector)
        | MetadataElementSelector::Description(selector)
        | MetadataElementSelector::Format(selector)
        | MetadataElementSelector::Publisher(selector)
        | MetadataElementSelector::Relation(selector)
        | MetadataElementSelector::Rights(selector)
        | MetadataElementSelector::Source(selector)
        | MetadataElementSelector::Subject(selector)
        | MetadataElementSelector::Type(selector) => selector,
    }
}

fn metadata_elements<'a>(package: &'a Package, local_name: &str) -> &'a [Element] {
    match local_name {
        "identifier" => package.metadata().identifier(),
        "title" => package.metadata().title(),
        "language" => package.metadata().language(),
        "contributor" => package.metadata().contributor(),
        "coverage" => package.metadata().coverage(),
        "creator" => package.metadata().creator(),
        "date" => package.metadata().date(),
        "description" => package.metadata().description(),
        "format" => package.metadata().format(),
        "publisher" => package.metadata().publisher(),
        "relation" => package.metadata().relation(),
        "rights" => package.metadata().rights(),
        "source" => package.metadata().source(),
        "subject" => package.metadata().subject(),
        "type" => package.metadata().dc_type(),
        _ => &[],
    }
}

fn spine_itemref_matches_selector(
    index: usize,
    itemref: &ItemRef,
    selector: &SpineItemRefSelector,
) -> bool {
    match selector {
        SpineItemRefSelector::Idref(idref) => itemref
            .idref()
            .is_some_and(|itemref_id| manifest_ids_equal(itemref_id, idref)),
        SpineItemRefSelector::Index(expected) => index == *expected,
    }
}

struct SpineSelectorErrors {
    not_found: fn(String) -> SpineItemRefLookupError,
    ambiguous: fn(String) -> SpineItemRefLookupError,
    value: String,
}

fn spine_selector_errors(selector: &SpineItemRefSelector) -> SpineSelectorErrors {
    match selector {
        SpineItemRefSelector::Idref(idref) => SpineSelectorErrors {
            not_found: SpineItemRefLookupError::IdrefNotFound,
            ambiguous: SpineItemRefLookupError::IdrefAmbiguous,
            value: idref.to_string(),
        },
        SpineItemRefSelector::Index(index) => SpineSelectorErrors {
            not_found: SpineItemRefLookupError::IndexNotFound,
            ambiguous: SpineItemRefLookupError::IndexAmbiguous,
            value: index.to_string(),
        },
    }
}

fn annotation_epub_path(path: impl AsRef<Path>) -> Result<EpubPath> {
    EpubPath::new(path.as_ref()).map_err(|source| EditError::InvalidResourcePath {
        path: path.as_ref().to_path_buf(),
        source,
    })
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
            embedded_annotation_orphan_paths: HashSet::new(),
            annotations_touched: false,
            package_override: None,
            navigation_override: None,
            structural_edits: BTreeMap::new(),
        }
    }
}

impl<R: ResourceProvider> EpubEditPreview<'_, R> {
    /// Returns deterministic final resource changes, coalesced by canonical path.
    pub fn changes(&self) -> &[EditChange] {
        &self.changes
    }

    /// Returns the package model that would be installed by [`Self::commit`].
    pub fn package(&self) -> &Package {
        &self.package
    }

    /// Returns the navigation model that [`Self::commit`] would install.
    ///
    /// The borrow is tied to this preview; navigation and point identities do not persist after
    /// the preview is consumed.
    pub fn navigation(&self) -> &Navigation {
        &self.navigation
    }

    /// Returns the resource inventory that would be installed by [`Self::commit`].
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    /// Returns touched embedded annotations, if this transaction inspected their bytes.
    ///
    /// `None` means annotations were not touched; it is not an eager publication annotation
    /// view and does not imply that no embedded annotation set exists.
    pub fn annotations(&self) -> Option<&AnnotationBundle> {
        self.annotations.as_ref()
    }

    /// Atomically installs the previewed state in memory and returns its change report.
    ///
    /// This operation cannot fail and does not export an EPUB. Call
    /// [`Epub::export`](crate::Epub::export) separately after the commit to write an archive.
    pub fn commit(self) -> EditReport {
        self.epub.install_edit_preview(
            self.package,
            self.navigation,
            self.resource_changes,
            self.resources,
        );
        EditReport {
            changes: self.changes,
        }
    }
}
