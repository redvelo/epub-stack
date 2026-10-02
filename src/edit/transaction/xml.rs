use super::*;

pub(super) fn decode_structural_xml<'a>(
    bytes: &'a [u8],
    path: &EpubPath,
) -> Result<std::borrow::Cow<'a, str>> {
    decode_xml(bytes).map_err(|source| EditError::StructuralXmlDecode {
        path: path.clone(),
        source,
    })
}

pub(super) fn manifest_item_resource_path(
    item: &ManifestItem,
    package_path: &EpubPath,
) -> Result<EpubPath> {
    structural_manifest_href_path(item, package_path).ok_or_else(|| {
        EditError::NonLocalManifestItem {
            id: item.id().map(ToString::to_string),
        }
    })
}

pub(super) fn ensure_manifest_item_targets_path(
    item: &ManifestItem,
    path: &EpubPath,
    package_path: &EpubPath,
) -> Result<()> {
    let actual = structural_manifest_href_path(item, package_path);
    if actual.as_ref() != Some(path) {
        return Err(EditError::ManifestHrefMismatch {
            expected: path.clone(),
            actual,
        });
    }
    Ok(())
}

pub(super) fn reject_manifest_resource_structural_removal<R: ResourceProvider>(
    edit: &EpubEdit<'_, R>,
    item: &ManifestItem,
    path: &EpubPath,
) -> Result<()> {
    if item.has_property(KnownManifestProperty::Nav) {
        return Err(EditError::NavigationManifestItem {
            id: item.id().map(ToString::to_string),
        });
    }
    edit.reject_structural_path(path)
}

pub(super) fn reject_shared_manifest_resource_path(
    package: &Package,
    selected_index: usize,
    resource_path: &EpubPath,
    package_path: &EpubPath,
    allow_nav_alias: bool,
) -> Result<()> {
    for (index, item) in package.manifest().items().iter().enumerate() {
        if index == selected_index {
            continue;
        }
        if allow_nav_alias && item.has_property(KnownManifestProperty::Nav) {
            continue;
        }
        let Some(path) = manifest_item_local_resource_path(item, package_path) else {
            continue;
        };
        if &path == resource_path {
            return Err(EditError::SharedResourcePath {
                path: resource_path.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn manifest_item_local_resource_path(
    item: &ManifestItem,
    package_path: &EpubPath,
) -> Option<EpubPath> {
    let authored_href = item.authored_href()?;
    resolve_local_href_from_source(authored_href, package_path).map(|(path, _)| path)
}

pub(super) fn opf2_cover_meta_ids(package: &Package) -> Result<Vec<String>> {
    let mut cover_ids = Vec::new();
    for meta in package.metadata().opf2meta().iter().filter(|meta| {
        meta.name()
            .is_some_and(|name| name.eq_ignore_ascii_case("cover"))
    }) {
        let Some(content) = meta.content() else {
            return Err(EditError::InvalidOpf2Cover { id: None });
        };
        if content.trim().is_empty() {
            return Err(EditError::InvalidOpf2Cover { id: None });
        }
        cover_ids.push(content.to_string());
    }
    Ok(cover_ids)
}

pub(super) fn guide_landmark_points(package: &Package) -> Result<Vec<NavigationPoint>> {
    package
        .guide()
        .into_iter()
        .flat_map(|guide| guide.references())
        .map(|reference| {
            let authored_href =
                reference
                    .authored_href()
                    .ok_or_else(|| EditError::InvalidGuideHref {
                        failure: GuideHrefFailure::Missing,
                    })?;
            let href = if authored_href.as_str().trim().is_empty() {
                return Err(EditError::InvalidGuideHref {
                    failure: GuideHrefFailure::Blank {
                        href: authored_href.to_string(),
                    },
                });
            } else {
                EpubHref::try_new(authored_href.as_str()).map_err(|_| {
                    EditError::InvalidGuideHref {
                        failure: GuideHrefFailure::InvalidSyntax {
                            href: authored_href.to_string(),
                        },
                    }
                })?
            };
            let semantics = reference.reference_type().and_then(guide_semantics);
            let label = reference
                .title()
                .cloned()
                .or_else(|| EpubString::try_new(authored_href.to_string()).ok())
                .or_else(|| semantics.and_then(|value| EpubString::try_new(value.to_string()).ok()))
                .unwrap_or_else(|| {
                    EpubString::try_new("Landmark").expect("static string is non-empty")
                });
            Ok(NavigationPoint::builder()
                .label(label)
                .href(href)
                .maybe_semantic(semantics)
                .build()?)
        })
        .collect()
}

pub(super) fn rebase_guide_landmarks(
    points: Vec<NavigationPoint>,
    package_path: &EpubPath,
    navigation_path: &EpubPath,
) -> Result<Vec<NavigationPoint>> {
    if points.is_empty() {
        return Ok(points);
    }
    let guide = NavigationDocument::builder()
        .path(package_path.clone())
        .lists(vec![
            NavigationList::builder()
                .semantic(crate::semantics::EpubStructuralSemantic::Landmarks)
                .points(points)
                .build()?,
        ])
        .build()?;
    let title = EpubString::try_new("Navigation").expect("static string is non-empty");
    let xml = guide
        .generate_epub_nav_xhtml(navigation_path, &title)
        .map_err(|error| navigation_generate_error(navigation_path, error))?;
    let rebased = parse::epub_nav(navigation_path.clone(), &xml)?;
    let landmarks = rebased
        .landmarks()
        .ok_or_else(|| EditError::source_node_missing(navigation_path))?;
    Ok(landmarks.points().to_vec())
}

pub(super) fn migrated_navigation_lists(
    source: &NavigationDocument,
    guide_landmarks: Vec<NavigationPoint>,
) -> Result<Vec<NavigationList>> {
    let mut saw_landmarks = false;
    let mut lists = source
        .lists()
        .iter()
        .map(|list| {
            if list.semantic() == Some(crate::semantics::EpubStructuralSemantic::Landmarks) {
                saw_landmarks = true;
                let mut points = list.points().to_vec();
                points.extend(guide_landmarks.iter().cloned());
                Ok(NavigationList::builder()
                    .semantic(crate::semantics::EpubStructuralSemantic::Landmarks)
                    .maybe_heading(list.heading().cloned())
                    .hidden(list.hidden())
                    .points(points)
                    .build()?)
            } else {
                Ok(list.clone())
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if !saw_landmarks && !guide_landmarks.is_empty() {
        lists.push(
            NavigationList::builder()
                .semantic(crate::semantics::EpubStructuralSemantic::Landmarks)
                .points(guide_landmarks)
                .build()?,
        );
    }
    Ok(lists)
}

pub(super) fn append_manifest_item_to_package_xml(
    xot: &mut Xot,
    doc: Node,
    item: &ManifestItem,
    package_path: &EpubPath,
) -> Result<()> {
    let manifest = find_opf_package_child(xot, doc, "manifest", package_path)?;

    let opf_ns = xot.add_namespace(OPF_NS);
    let item_name = xot.add_name_ns("item", opf_ns);
    let id_name = xot.add_name("id");
    let href_name = xot.add_name("href");
    let media_type_name = xot.add_name("media-type");
    let fallback_name = xot.add_name("fallback");
    let media_overlay_name = xot.add_name("media-overlay");
    let properties_name = xot.add_name("properties");

    let item_node = xot.new_element(item_name);
    let id = required_manifest_item_id(item)?;
    let media_type = item.media_type().ok_or(PackageError::EmptyField {
        field: PackageField::ManifestItemMediaType,
    })?;
    xot.set_attribute(item_node, id_name, id.to_string());
    if let Some(href) = item.authored_href() {
        xot.set_attribute(item_node, href_name, href.to_string());
    }
    xot.set_attribute(item_node, media_type_name, media_type.to_string());
    if let Some(fallback) = item.fallback() {
        xot.set_attribute(item_node, fallback_name, fallback.to_string());
    }
    if let Some(media_overlay) = item.media_overlay() {
        xot.set_attribute(item_node, media_overlay_name, media_overlay.to_string());
    }
    if !item.properties().is_empty() {
        let properties = item
            .properties()
            .iter()
            .map(|property| property.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        xot.set_attribute(item_node, properties_name, properties);
    }

    xot.append(manifest, item_node)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn remove_manifest_item_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    id: &str,
    package_path: &EpubPath,
) -> Result<()> {
    let item = find_manifest_item_node(xot, doc, id, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    xot.remove(item)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn remove_manifest_item_at_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<()> {
    let item = find_manifest_item_node_at(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    xot.remove(item)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn replace_manifest_item_at_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    item: &ManifestItem,
    package_path: &EpubPath,
) -> Result<()> {
    let item_node = find_manifest_item_node_at(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    set_manifest_item_attributes(xot, item_node, item)
}

#[derive(Debug, Clone)]
pub(super) struct Opf2MigrationPackageEdit {
    pub(super) modified: EpubString,
    pub(super) nav_item: Option<ManifestItem>,
    pub(super) cover_ids: Vec<String>,
    pub(super) ncx_id: Option<String>,
}

pub(super) fn migrate_opf2_package_xml(
    xot: &mut Xot,
    doc: Node,
    edit: &Opf2MigrationPackageEdit,
    package_path: &EpubPath,
) -> Result<()> {
    let package = xot
        .document_element(doc)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    ensure_opf_element(xot, package, "package", package_path)?;
    let version_name = xot.add_name("version");
    xot.set_attribute(package, version_name, "3.0");

    upsert_modified_meta_in_package_xml(xot, doc, &edit.modified, package_path)?;
    for cover_id in &edit.cover_ids {
        add_manifest_item_property_in_package_xml(
            xot,
            doc,
            cover_id,
            KnownManifestProperty::CoverImage,
            package_path,
        )?;
    }
    remove_opf2_cover_meta_from_package_xml(xot, doc, package_path)?;
    if let Some(nav_item) = &edit.nav_item {
        append_manifest_item_to_package_xml(xot, doc, nav_item, package_path)?;
    }
    if let Some(ncx_id) = &edit.ncx_id {
        remove_manifest_item_from_package_xml(xot, doc, ncx_id, package_path)?;
    }
    remove_spine_toc_from_package_xml(xot, doc, package_path)?;
    remove_guide_from_package_xml(xot, doc, package_path)?;
    Ok(())
}

pub(super) fn remove_opf2_cover_meta_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    package_path: &EpubPath,
) -> Result<()> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let name_name = xot.add_name("name");
    let cover_meta = xot
        .children(metadata)
        .filter(|child| {
            is_element_ns(xot, *child, OPF_NS, META)
                && xot
                    .get_attribute(*child, name_name)
                    .is_some_and(|name| name.eq_ignore_ascii_case("cover"))
        })
        .collect::<Vec<_>>();
    for node in cover_meta {
        remove_package_xml_node(xot, node, package_path)?;
    }
    Ok(())
}

pub(super) fn remove_spine_toc_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    package_path: &EpubPath,
) -> Result<()> {
    let spine = find_spine_node(xot, doc, package_path)?;
    let toc_name = xot.add_name("toc");
    xot.remove_attribute(spine, toc_name);
    Ok(())
}

pub(super) fn remove_guide_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    package_path: &EpubPath,
) -> Result<()> {
    let package = xot
        .document_element(doc)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    if let Some(guide) = find_opf_child(xot, package, "guide") {
        remove_package_xml_node(xot, guide, package_path)?;
    }
    Ok(())
}

pub(super) fn verify_opf2_migration_package(
    package: &Package,
    modified: &EpubString,
    cover_ids: &[String],
    package_path: &EpubPath,
) -> Result<()> {
    if package.version() != Some(EpubVersion::Three) {
        return Err(EditError::model_mismatch(package_path));
    }
    if package.nav_item().is_none() {
        return Err(EditError::model_mismatch(package_path));
    }
    if package.ncx_item().is_some() || package.spine().toc().is_some() {
        return Err(EditError::model_mismatch(package_path));
    }
    if package.guide().is_some() {
        return Err(EditError::model_mismatch(package_path));
    }
    if !package.metadata().meta().iter().any(|meta| {
        meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
            && meta.content() == Some(modified)
    }) {
        return Err(EditError::model_mismatch(package_path));
    }
    if package.metadata().opf2meta().iter().any(|meta| {
        meta.name()
            .is_some_and(|name| name.eq_ignore_ascii_case("cover"))
    }) {
        return Err(EditError::model_mismatch(package_path));
    }
    for cover_id in cover_ids {
        let Some(item) = package.manifest_item_by_id(cover_id) else {
            return Err(EditError::model_mismatch(package_path));
        };
        if !item.has_property(KnownManifestProperty::CoverImage) {
            return Err(EditError::source_node_missing(package_path));
        }
    }
    Ok(())
}

pub(super) fn set_manifest_item_attributes(
    xot: &mut Xot,
    item_node: Node,
    item: &ManifestItem,
) -> Result<()> {
    let id_name = xot.add_name("id");
    let href_name = xot.add_name("href");
    let media_type_name = xot.add_name("media-type");
    let fallback_name = xot.add_name("fallback");
    let media_overlay_name = xot.add_name("media-overlay");
    let properties_name = xot.add_name("properties");

    for attr in [
        id_name,
        href_name,
        media_type_name,
        fallback_name,
        media_overlay_name,
        properties_name,
    ] {
        xot.remove_attribute(item_node, attr);
    }

    let id = required_manifest_item_id(item)?;
    let media_type = item.media_type().ok_or(PackageError::EmptyField {
        field: PackageField::ManifestItemMediaType,
    })?;
    xot.set_attribute(item_node, id_name, id.to_string());
    if let Some(href) = item.authored_href() {
        xot.set_attribute(item_node, href_name, href.to_string());
    }
    xot.set_attribute(item_node, media_type_name, media_type.to_string());
    if let Some(fallback) = item.fallback() {
        xot.set_attribute(item_node, fallback_name, fallback.to_string());
    }
    if let Some(media_overlay) = item.media_overlay() {
        xot.set_attribute(item_node, media_overlay_name, media_overlay.to_string());
    }
    if !item.properties().is_empty() {
        let properties = item
            .properties()
            .iter()
            .map(|property| property.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        xot.set_attribute(item_node, properties_name, properties);
    }
    Ok(())
}

pub(super) fn append_metadata_element_to_package_xml(
    xot: &mut Xot,
    doc: Node,
    kind: DcElement,
    element: &Element,
    package_path: &EpubPath,
) -> Result<()> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let dc_ns = xot.add_namespace(DC_NS);
    let dc_prefix = xot.add_prefix("dc");
    let element_name = xot.add_name_ns(kind.local_name(), dc_ns);
    let element_node = xot.new_element(element_name);
    xot.namespaces_mut(element_node).insert(dc_prefix, dc_ns);
    set_dc_element_attributes(xot, element_node, element);
    append_text_if_present(xot, element_node, element.content(), package_path)?;
    append_package_xml_child(xot, metadata, element_node, package_path)
}

pub(super) fn append_meta_to_package_xml(
    xot: &mut Xot,
    doc: Node,
    meta: &Meta,
    package_path: &EpubPath,
) -> Result<()> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let opf_ns = xot.add_namespace(OPF_NS);
    let meta_name = xot.add_name_ns(META, opf_ns);
    let meta_node = xot.new_element(meta_name);
    set_meta_attributes(xot, meta_node, meta);
    append_text_if_present(xot, meta_node, meta.content(), package_path)?;
    append_package_xml_child(xot, metadata, meta_node, package_path)
}

pub(super) fn append_metadata_link_to_package_xml(
    xot: &mut Xot,
    doc: Node,
    link: &MetadataLink,
    package_path: &EpubPath,
) -> Result<()> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let opf_ns = xot.add_namespace(OPF_NS);
    let link_name = xot.add_name_ns(LINK, opf_ns);
    let link_node = xot.new_element(link_name);
    set_metadata_link_attributes(xot, link_node, link);
    append_package_xml_child(xot, metadata, link_node, package_path)
}

pub(super) fn remove_metadata_element_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    local_name: &str,
    index: usize,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_metadata_element_node(xot, doc, DC_NS, local_name, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    remove_package_xml_node(xot, node, package_path)
}

pub(super) fn replace_metadata_element_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    local_name: &str,
    index: usize,
    element: &Element,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_metadata_element_node(xot, doc, DC_NS, local_name, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    set_dc_element_attributes(xot, node, element);
    replace_text_content(xot, node, element.content(), package_path)
}

pub(super) fn remove_meta_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_epub3_meta_node(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    remove_package_xml_node(xot, node, package_path)
}

pub(super) fn replace_meta_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    meta: &Meta,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_epub3_meta_node(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    set_meta_attributes(xot, node, meta);
    replace_text_content(xot, node, meta.content(), package_path)
}

pub(super) fn remove_metadata_link_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_metadata_element_node(xot, doc, OPF_NS, LINK, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    remove_package_xml_node(xot, node, package_path)
}

pub(super) fn replace_metadata_link_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    link: &MetadataLink,
    package_path: &EpubPath,
) -> Result<()> {
    let node = find_metadata_element_node(xot, doc, OPF_NS, LINK, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    set_metadata_link_attributes(xot, node, link);
    remove_all_children(xot, node, package_path)
}

pub(super) fn set_dc_element_attributes(xot: &mut Xot, element_node: Node, element: &Element) {
    let id_name = xot.add_name("id");
    let dir_name = xot.add_name("dir");
    let xml_ns = xot.add_namespace("http://www.w3.org/XML/1998/namespace");
    let lang_name = xot.add_name_ns("lang", xml_ns);

    for attr in [id_name, dir_name, lang_name] {
        xot.remove_attribute(element_node, attr);
    }

    if let Some(id) = element.id() {
        xot.set_attribute(element_node, id_name, id.to_string());
    }
    if let Some(lang) = element.xml_lang() {
        xot.set_attribute(element_node, lang_name, lang.to_string());
    }
    if let Some(dir) = element.dir() {
        xot.set_attribute(element_node, dir_name, dir.to_string());
    }
}

pub(super) fn set_meta_attributes(xot: &mut Xot, meta_node: Node, meta: &Meta) {
    let id_name = xot.add_name("id");
    let dir_name = xot.add_name("dir");
    let refines_name = xot.add_name("refines");
    let property_name = xot.add_name("property");
    let scheme_name = xot.add_name("scheme");
    let xml_ns = xot.add_namespace("http://www.w3.org/XML/1998/namespace");
    let lang_name = xot.add_name_ns("lang", xml_ns);

    for attr in [
        id_name,
        dir_name,
        refines_name,
        property_name,
        scheme_name,
        lang_name,
    ] {
        xot.remove_attribute(meta_node, attr);
    }

    if let Some(id) = meta.id() {
        xot.set_attribute(meta_node, id_name, id.to_string());
    }
    if let Some(lang) = meta.xml_lang() {
        xot.set_attribute(meta_node, lang_name, lang.to_string());
    }
    if let Some(dir) = meta.dir() {
        xot.set_attribute(meta_node, dir_name, dir.to_string());
    }
    if let Some(refines) = meta.refines() {
        xot.set_attribute(meta_node, refines_name, refines.to_string());
    }
    if let Some(property) = meta.property() {
        xot.set_attribute(meta_node, property_name, property.as_str().to_string());
    }
    if let Some(scheme) = meta.scheme() {
        xot.set_attribute(meta_node, scheme_name, scheme.to_string());
    }
}

pub(super) fn set_metadata_link_attributes(xot: &mut Xot, link_node: Node, link: &MetadataLink) {
    let id_name = xot.add_name("id");
    let href_name = xot.add_name("href");
    let rel_name = xot.add_name("rel");
    let hreflang_name = xot.add_name("hreflang");
    let refines_name = xot.add_name("refines");
    let media_type_name = xot.add_name("media-type");
    let properties_name = xot.add_name("properties");

    for attr in [
        id_name,
        href_name,
        rel_name,
        hreflang_name,
        refines_name,
        media_type_name,
        properties_name,
    ] {
        xot.remove_attribute(link_node, attr);
    }

    if let Some(id) = link.id() {
        xot.set_attribute(link_node, id_name, id.to_string());
    }
    if let Some(href) = link.authored_href() {
        xot.set_attribute(link_node, href_name, href.to_string());
    }
    if let Some(rel) = link.rel() {
        xot.set_attribute(link_node, rel_name, rel.as_str().to_string());
    }
    if let Some(hreflang) = link.hreflang() {
        xot.set_attribute(link_node, hreflang_name, hreflang.to_string());
    }
    if let Some(refines) = link.refines() {
        xot.set_attribute(link_node, refines_name, refines.to_string());
    }
    if let Some(media_type) = link.media_type() {
        xot.set_attribute(link_node, media_type_name, media_type.to_string());
    }
    if !link.properties().is_empty() {
        let properties = link
            .properties()
            .iter()
            .map(|property| property.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        xot.set_attribute(link_node, properties_name, properties);
    }
}

pub(super) fn append_text_if_present(
    xot: &mut Xot,
    node: Node,
    text: Option<&EpubString>,
    package_path: &EpubPath,
) -> Result<()> {
    if let Some(text) = text {
        let text_node = xot.new_text(text.as_str());
        append_package_xml_child(xot, node, text_node, package_path)?;
    }
    Ok(())
}

pub(super) fn append_package_xml_child(
    xot: &mut Xot,
    parent: Node,
    child: Node,
    package_path: &EpubPath,
) -> Result<()> {
    xot.append(parent, child)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn remove_package_xml_node(
    xot: &mut Xot,
    node: Node,
    package_path: &EpubPath,
) -> Result<()> {
    xot.remove(node)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn replace_text_content(
    xot: &mut Xot,
    node: Node,
    text: Option<&EpubString>,
    package_path: &EpubPath,
) -> Result<()> {
    remove_all_children(xot, node, package_path)?;
    append_text_if_present(xot, node, text, package_path)
}

pub(super) fn remove_all_children(
    xot: &mut Xot,
    node: Node,
    package_path: &EpubPath,
) -> Result<()> {
    let children = xot.children(node).collect::<Vec<_>>();
    for child in children {
        remove_package_xml_node(xot, child, package_path)?;
    }
    Ok(())
}

pub(super) fn append_spine_itemref_to_package_xml(
    xot: &mut Xot,
    doc: Node,
    itemref: &ItemRef,
    package_path: &EpubPath,
) -> Result<()> {
    let spine = find_spine_node(xot, doc, package_path)?;
    let opf_ns = xot.add_namespace(OPF_NS);
    let itemref_name = xot.add_name_ns("itemref", opf_ns);
    let itemref_node = xot.new_element(itemref_name);
    set_spine_itemref_attributes(xot, itemref_node, itemref)?;
    xot.append(spine, itemref_node)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn remove_spine_itemref_from_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<()> {
    let itemref = find_spine_itemref_node(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    xot.remove(itemref)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    Ok(())
}

pub(super) fn replace_spine_itemref_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    index: usize,
    itemref: &ItemRef,
    package_path: &EpubPath,
) -> Result<()> {
    let itemref_node = find_spine_itemref_node(xot, doc, index, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    set_spine_itemref_attributes(xot, itemref_node, itemref)
}

pub(super) fn move_spine_itemref_in_package_xml(
    xot: &mut Xot,
    doc: Node,
    from: usize,
    to: usize,
    package_path: &EpubPath,
) -> Result<()> {
    if from == to {
        return Ok(());
    }
    let spine = find_spine_node(xot, doc, package_path)?;
    let itemref = find_spine_itemref_node(xot, doc, from, package_path)?
        .ok_or_else(|| EditError::source_node_missing(package_path))?;
    let itemref_count = xot
        .children(spine)
        .filter(|child| is_opf_element(xot, *child, "itemref"))
        .count();
    if to >= itemref_count {
        return Err(EditError::source_node_missing(package_path));
    }

    xot.detach(itemref)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    let remaining = xot
        .children(spine)
        .filter(|child| is_opf_element(xot, *child, "itemref"))
        .collect::<Vec<_>>();
    if to >= remaining.len() {
        xot.append(spine, itemref)
            .map_err(|source| EditError::structural_xml(package_path, source))?;
    } else {
        xot.insert_before(remaining[to], itemref)
            .map_err(|source| EditError::structural_xml(package_path, source))?;
    }
    Ok(())
}

pub(super) fn set_spine_itemref_attributes(
    xot: &mut Xot,
    itemref_node: Node,
    itemref: &ItemRef,
) -> Result<()> {
    let id_name = xot.add_name("id");
    let idref_name = xot.add_name("idref");
    let linear_name = xot.add_name("linear");
    let properties_name = xot.add_name("properties");

    for attr in [id_name, idref_name, linear_name, properties_name] {
        xot.remove_attribute(itemref_node, attr);
    }

    if let Some(id) = itemref.id() {
        xot.set_attribute(itemref_node, id_name, id.to_string());
    }
    let idref = itemref.idref().ok_or(PackageError::EmptyField {
        field: PackageField::ItemRefIdref,
    })?;
    xot.set_attribute(itemref_node, idref_name, idref.to_string());
    if itemref.linear() == Linear::No {
        xot.set_attribute(itemref_node, linear_name, itemref.linear().to_string());
    }
    if !itemref.properties().is_empty() {
        let properties = itemref
            .properties()
            .iter()
            .map(|property| property.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        xot.set_attribute(itemref_node, properties_name, properties);
    }
    Ok(())
}

pub(super) fn find_spine_node(xot: &Xot, doc: Node, package_path: &EpubPath) -> Result<Node> {
    find_opf_package_child(xot, doc, "spine", package_path)
}

pub(super) fn find_metadata_node(xot: &Xot, doc: Node, package_path: &EpubPath) -> Result<Node> {
    find_opf_package_child(xot, doc, "metadata", package_path)
}

pub(super) fn find_metadata_element_node(
    xot: &Xot,
    doc: Node,
    namespace: &str,
    local_name: &str,
    index: usize,
    package_path: &EpubPath,
) -> Result<Option<Node>> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    Ok(xot
        .children(metadata)
        .filter(|child| is_element_ns(xot, *child, namespace, local_name))
        .nth(index))
}

pub(super) fn find_epub3_meta_node(
    xot: &Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<Option<Node>> {
    let metadata = find_metadata_node(xot, doc, package_path)?;
    let property_name = xot.name("property");
    Ok(xot
        .children(metadata)
        .filter(|child| {
            is_element_ns(xot, *child, OPF_NS, META)
                && property_name.is_some_and(|name| xot.get_attribute(*child, name).is_some())
        })
        .nth(index))
}

pub(super) fn find_spine_itemref_node(
    xot: &Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<Option<Node>> {
    let spine = find_spine_node(xot, doc, package_path)?;
    Ok(xot
        .children(spine)
        .filter(|child| is_opf_element(xot, *child, "itemref"))
        .nth(index))
}

pub(super) fn find_manifest_item_node(
    xot: &Xot,
    doc: Node,
    id: &str,
    package_path: &EpubPath,
) -> Result<Option<Node>> {
    let manifest = find_opf_package_child(xot, doc, "manifest", package_path)?;
    let id_name = xot.name("id");
    Ok(xot.children(manifest).find(|child| {
        xot.element(*child)
            .map(|element| {
                let (name, namespace) = xot.name_ns_str(element.name());
                name == "item"
                    && namespace == OPF_NS
                    && id_name.is_some_and(|name| {
                        xot.get_attribute(*child, name)
                            .is_some_and(|authored_id| manifest_ids_equal(authored_id, id))
                    })
            })
            .unwrap_or(false)
    }))
}

pub(super) fn find_manifest_item_node_at(
    xot: &Xot,
    doc: Node,
    index: usize,
    package_path: &EpubPath,
) -> Result<Option<Node>> {
    let manifest = find_opf_package_child(xot, doc, "manifest", package_path)?;
    Ok(xot
        .children(manifest)
        .filter(|child| is_opf_element(xot, *child, "item"))
        .nth(index))
}

pub(super) fn find_opf_child(xot: &Xot, parent: Node, local_name: &str) -> Option<Node> {
    xot.children(parent)
        .find(|child| is_opf_element(xot, *child, local_name))
}

pub(super) fn find_opf_package_child(
    xot: &Xot,
    doc: Node,
    local_name: &str,
    package_path: &EpubPath,
) -> Result<Node> {
    let package = xot
        .document_element(doc)
        .map_err(|source| EditError::structural_xml(package_path, source))?;
    ensure_opf_element(xot, package, "package", package_path)?;
    match find_opf_child(xot, package, local_name) {
        Some(node) => Ok(node),
        None => {
            if let Some(node) = find_child_by_local_name(xot, package, local_name) {
                return Err(unsupported_package_namespace_error(
                    xot,
                    node,
                    package_path,
                    local_name,
                ));
            }
            Err(EditError::source_node_missing(package_path))
        }
    }
}

pub(super) fn ensure_opf_element(
    xot: &Xot,
    node: Node,
    local_name: &str,
    package_path: &EpubPath,
) -> Result<()> {
    if is_opf_element(xot, node, local_name) {
        return Ok(());
    }
    if is_element_local_name(xot, node, local_name) {
        return Err(unsupported_package_namespace_error(
            xot,
            node,
            package_path,
            local_name,
        ));
    }
    Err(EditError::source_node_missing(package_path))
}

pub(super) fn find_child_by_local_name(xot: &Xot, parent: Node, local_name: &str) -> Option<Node> {
    xot.children(parent)
        .find(|child| is_element_local_name(xot, *child, local_name))
}

pub(super) fn is_element_local_name(xot: &Xot, node: Node, local_name: &str) -> bool {
    xot.element(node)
        .map(|element| {
            let (name, _namespace) = xot.name_ns_str(element.name());
            name == local_name
        })
        .unwrap_or(false)
}

pub(super) fn is_element_ns(xot: &Xot, node: Node, namespace: &str, local_name: &str) -> bool {
    xot.element(node)
        .map(|element| {
            let (name, element_namespace) = xot.name_ns_str(element.name());
            name == local_name && element_namespace == namespace
        })
        .unwrap_or(false)
}

pub(super) fn unsupported_package_namespace_error(
    xot: &Xot,
    node: Node,
    package_path: &EpubPath,
    local_name: &str,
) -> EditError {
    let namespace = xot
        .element(node)
        .map(|element| {
            let (_name, namespace) = xot.name_ns_str(element.name());
            namespace.to_string()
        })
        .unwrap_or_default();
    EditError::UnsupportedPackageNamespace {
        path: package_path.clone(),
        element: local_name.to_string(),
        namespace,
        expected: OPF_NS,
    }
}

pub(super) fn is_opf_element(xot: &Xot, node: Node, local_name: &str) -> bool {
    xot.element(node)
        .map(|element| {
            let (name, namespace) = xot.name_ns_str(element.name());
            name == local_name && namespace == OPF_NS
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub(super) struct NavPointEditTarget {
    pub(super) nav_path: EpubPath,
    pub(super) list_index: usize,
    pub(super) point_path: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(super) struct NavInsertionResolved {
    pub(super) nav_path: EpubPath,
    pub(super) list_index: usize,
    pub(super) parent_path: Option<Vec<usize>>,
    pub(super) existing_len: usize,
}

#[derive(Debug, Clone)]
pub(super) struct NavInsertionEditTarget {
    pub(super) nav_path: EpubPath,
    pub(super) list_index: usize,
    pub(super) parent_path: Option<Vec<usize>>,
}

pub(super) fn resolve_nav_insertion_target(
    navigation: &Option<NavigationDocument>,
    target: &InsertionTarget,
) -> Result<NavInsertionResolved> {
    let document = navigation
        .as_ref()
        .filter(|document| document.is_epub_nav())
        .ok_or(EditError::MissingEpubNavigation)?;
    let (list_index, parent_path) = match target {
        InsertionTarget::List(selector) => {
            (resolve_nav_list_index(document.lists(), *selector)?, None)
        }
        InsertionTarget::ChildOf(selector) => {
            let target = resolve_navigation_point(navigation, selector)?;
            (target.list_index, Some(target.point_path))
        }
    };
    let list = &document.lists()[list_index];
    let existing_len = if let Some(path) = parent_path.as_deref() {
        navigation_point_by_path(list.points(), path)
            .ok_or_else(|| EditError::model_mismatch(document.path()))?
            .children()
            .len()
    } else {
        list.points().len()
    };
    Ok(NavInsertionResolved {
        nav_path: document.path().clone(),
        list_index,
        parent_path,
        existing_len,
    })
}

pub(super) fn nav_parent_path(path: &[usize]) -> Option<Vec<usize>> {
    (path.len() > 1).then(|| path[..path.len() - 1].to_vec())
}

pub(super) fn navigation_points_at_parent<'a>(
    list: &'a NavigationList,
    parent_path: Option<&[usize]>,
) -> Option<&'a [NavigationPoint]> {
    match parent_path {
        Some(path) => navigation_point_by_path(list.points(), path).map(NavigationPoint::children),
        None => Some(list.points()),
    }
}

pub(super) fn validate_nav_move_target(
    source: &NavPointTarget<'_>,
    destination: &NavInsertionResolved,
) -> Result<()> {
    if source.list_index != destination.list_index {
        return Ok(());
    }
    let Some(parent_path) = destination.parent_path.as_deref() else {
        return Ok(());
    };
    if parent_path == source.point_path.as_slice()
        || parent_path.starts_with(source.point_path.as_slice())
    {
        return Err(EditError::NavigationPointCycle);
    }
    Ok(())
}

pub(super) fn nav_move_destination_index(
    source: &NavPointTarget<'_>,
    destination: &NavInsertionResolved,
) -> usize {
    let source_parent = nav_parent_path(&source.point_path);
    if source.list_index == destination.list_index && source_parent == destination.parent_path {
        destination.existing_len.saturating_sub(1)
    } else {
        destination.existing_len
    }
}

pub(super) fn resolve_navigation_point<'a>(
    navigation: &'a Option<NavigationDocument>,
    selector: &PointSelector,
) -> Result<NavPointTarget<'a>> {
    let document = navigation
        .as_ref()
        .filter(|document| document.is_epub_nav())
        .ok_or(EditError::MissingEpubNavigation)?;
    let list_index = resolve_nav_list_index(document.lists(), selector.list)?;
    let list = &document.lists()[list_index];
    let (point_path, point) = resolve_navigation_point_in_list(list, selector)?;
    Ok(NavPointTarget {
        nav_path: document.path().clone(),
        list_index,
        point_path,
        point,
    })
}

pub(super) fn resolve_nav_list_index(
    lists: &[NavigationList],
    selector: ListSelector,
) -> Result<usize> {
    let matches: fn(&NavigationList) -> bool = match selector {
        ListSelector::Index(index) => {
            return lists
                .get(index)
                .map(|_| index)
                .ok_or_else(|| EditError::selection(selector, SelectionFailure::NotFound));
        }
        ListSelector::Toc => is_toc_list,
        ListSelector::PageList => is_page_list,
        ListSelector::Landmarks => is_landmarks_list,
    };
    unique_position(
        lists
            .iter()
            .enumerate()
            .filter(|(_, list)| matches(list))
            .map(|(index, _)| index),
        selector,
    )
}

pub(super) fn is_toc_list(list: &NavigationList) -> bool {
    list.semantic() == Some(crate::semantics::EpubStructuralSemantic::Toc)
}

pub(super) fn is_page_list(list: &NavigationList) -> bool {
    list.semantic() == Some(crate::semantics::EpubStructuralSemantic::PageList)
}

pub(super) fn is_landmarks_list(list: &NavigationList) -> bool {
    list.semantic() == Some(crate::semantics::EpubStructuralSemantic::Landmarks)
}

pub(super) fn resolve_navigation_point_in_list<'a>(
    list: &'a NavigationList,
    selector: &PointSelector,
) -> Result<(Vec<usize>, &'a NavigationPoint)> {
    if let PointMatch::Path(path) = &selector.point {
        return navigation_point_by_path(list.points(), path)
            .map(|point| (path.clone(), point))
            .ok_or_else(|| EditError::selection(selector.clone(), SelectionFailure::NotFound));
    }
    let mut matches = Vec::new();
    collect_matching_navigation_points(
        list.points(),
        &selector.point,
        &mut Vec::new(),
        &mut matches,
    );
    let index = unique_position(0..matches.len(), selector.clone())?;
    Ok(matches.swap_remove(index))
}

pub(super) fn navigation_point_by_path<'a>(
    points: &'a [NavigationPoint],
    path: &[usize],
) -> Option<&'a NavigationPoint> {
    let (first, rest) = path.split_first()?;
    let point = points.get(*first)?;
    if rest.is_empty() {
        Some(point)
    } else {
        navigation_point_by_path(point.children(), rest)
    }
}

pub(super) fn collect_matching_navigation_points<'a>(
    points: &'a [NavigationPoint],
    point_match: &PointMatch,
    path: &mut Vec<usize>,
    matches: &mut Vec<(Vec<usize>, &'a NavigationPoint)>,
) {
    for (index, point) in points.iter().enumerate() {
        path.push(index);
        if navigation_point_matches(point, point_match) {
            matches.push((path.clone(), point));
        }
        collect_matching_navigation_points(point.children(), point_match, path, matches);
        path.pop();
    }
}

pub(super) fn navigation_point_matches(point: &NavigationPoint, point_match: &PointMatch) -> bool {
    match point_match {
        PointMatch::Path(_) => false,
        PointMatch::Href(href) => point.href().as_ref() == Some(href),
        PointMatch::AuthoredHref(href) => point.authored_href() == Some(href),
        PointMatch::Label(label) => point.label() == Some(label),
    }
}

pub(super) fn navigation_point_matches_written_model(
    actual: &NavigationPoint,
    expected: &NavigationPoint,
) -> bool {
    actual.label() == expected.label()
        && actual.authored_href() == expected.authored_href()
        && actual.hidden() == expected.hidden()
        && actual.semantic() == expected.semantic()
        && (expected.authored_semantic_tokens().is_empty()
            || actual.authored_semantic_tokens() == expected.authored_semantic_tokens())
        && actual.children().len() == expected.children().len()
        && actual
            .children()
            .iter()
            .zip(expected.children())
            .all(|(actual, expected)| navigation_point_matches_written_model(actual, expected))
}

pub(super) fn replace_navigation_point_label_in_xml(
    xot: &mut Xot,
    doc: Node,
    target: &NavPointEditTarget,
    label: &EpubString,
) -> Result<()> {
    let li = find_navigation_point_li_node(xot, doc, target)?;
    let label_node = first_direct_navigation_point_child(xot, li, &["a", "span"])
        .ok_or_else(|| EditError::source_node_missing(&target.nav_path))?;
    replace_nav_xml_text_content(xot, label_node, label.as_str(), &target.nav_path)
}

pub(super) fn replace_navigation_point_href_in_xml(
    xot: &mut Xot,
    doc: Node,
    target: &NavPointEditTarget,
    href: &EpubHref,
) -> Result<()> {
    let li = find_navigation_point_li_node(xot, doc, target)?;
    let anchor = first_direct_navigation_point_child(xot, li, &["a"])
        .ok_or_else(|| EditError::source_node_missing(&target.nav_path))?;
    let href_name = xot.add_name("href");
    xot.set_attribute(anchor, href_name, href.to_string());
    Ok(())
}

pub(super) fn append_navigation_point_in_xml(
    xot: &mut Xot,
    doc: Node,
    target: &NavInsertionEditTarget,
    point: &NavigationPoint,
) -> Result<()> {
    let nav = find_nav_list_node(
        xot,
        doc,
        &NavPointEditTarget {
            nav_path: target.nav_path.clone(),
            list_index: target.list_index,
            point_path: target.parent_path.clone().unwrap_or_default(),
        },
    )?;
    let container = if let Some(parent_path) = target.parent_path.as_deref() {
        let parent = nav_li_by_path(xot, nav, parent_path)
            .ok_or_else(|| EditError::source_node_missing(&target.nav_path))?;
        first_direct_ol_child(xot, parent)
            .map_or_else(|| create_ol_child(xot, parent, &target.nav_path), Ok)?
    } else {
        first_direct_ol_child(xot, nav)
            .map_or_else(|| create_ol_child(xot, nav, &target.nav_path), Ok)?
    };
    let li = navigation_point_to_xml(xot, point, &target.nav_path)?;
    append_nav_xml_child(xot, container, li, &target.nav_path)
}

pub(super) fn remove_navigation_point_from_xml(
    xot: &mut Xot,
    doc: Node,
    target: &NavPointEditTarget,
) -> Result<()> {
    let li = find_navigation_point_li_node(xot, doc, target)?;
    xot.remove(li)
        .map_err(|source| EditError::structural_xml(&target.nav_path, source))?;
    Ok(())
}

pub(super) fn move_navigation_point_in_xml(
    xot: &mut Xot,
    doc: Node,
    source: &NavPointEditTarget,
    destination: &NavInsertionEditTarget,
) -> Result<()> {
    let source_nav = find_nav_list_node(xot, doc, source)?;
    let source_li = nav_li_by_path(xot, source_nav, &source.point_path)
        .ok_or_else(|| EditError::source_node_missing(&source.nav_path))?;
    let destination_container = find_nav_insertion_container(xot, doc, destination)?;
    xot.detach(source_li)
        .map_err(|source| EditError::structural_xml(&destination.nav_path, source))?;
    append_nav_xml_child(xot, destination_container, source_li, &destination.nav_path)
}

pub(super) fn find_nav_insertion_container(
    xot: &mut Xot,
    doc: Node,
    target: &NavInsertionEditTarget,
) -> Result<Node> {
    let nav = find_nav_list_node(
        xot,
        doc,
        &NavPointEditTarget {
            nav_path: target.nav_path.clone(),
            list_index: target.list_index,
            point_path: target.parent_path.clone().unwrap_or_default(),
        },
    )?;
    if let Some(parent_path) = target.parent_path.as_deref() {
        let parent = nav_li_by_path(xot, nav, parent_path)
            .ok_or_else(|| EditError::source_node_missing(&target.nav_path))?;
        first_direct_ol_child(xot, parent)
            .map_or_else(|| create_ol_child(xot, parent, &target.nav_path), Ok)
    } else {
        first_direct_ol_child(xot, nav)
            .map_or_else(|| create_ol_child(xot, nav, &target.nav_path), Ok)
    }
}

pub(super) fn navigation_point_to_xml(
    xot: &mut Xot,
    point: &NavigationPoint,
    nav_path: &EpubPath,
) -> Result<Node> {
    if point
        .authored_semantic_tokens()
        .iter()
        .any(|token| matches!(token, SemanticToken::NcxClass { .. }))
    {
        return Err(EditError::NcxSemanticInNavigation);
    }
    if point.authored_href().is_none()
        && point.label().is_none()
        && (point.semantic().is_some() || !point.authored_semantic_tokens().is_empty())
    {
        return Err(EditError::NavigationSemanticsWithoutLabel);
    }

    let xhtml_ns = xot.add_namespace(XHTML_NS);
    let li_name = xot.add_name_ns("li", xhtml_ns);
    let li = xot.new_element(li_name);
    set_nav_hidden_attribute(xot, li, point.hidden());

    match (point.authored_href(), point.label()) {
        (Some(authored_href), label) => {
            let a_name = xot.add_name_ns("a", xhtml_ns);
            let a = xot.new_element(a_name);
            let href_name = xot.add_name("href");
            xot.set_attribute(a, href_name, authored_href.to_string());
            set_nav_semantic_attributes(xot, a, point);
            if let Some(label) = label {
                let text = xot.new_text(label.as_str());
                append_nav_xml_child(xot, a, text, nav_path)?;
            }
            append_nav_xml_child(xot, li, a, nav_path)?;
        }
        (None, Some(label)) => {
            let span_name = xot.add_name_ns("span", xhtml_ns);
            let span = xot.new_element(span_name);
            set_nav_semantic_attributes(xot, span, point);
            let text = xot.new_text(label.as_str());
            append_nav_xml_child(xot, span, text, nav_path)?;
            append_nav_xml_child(xot, li, span, nav_path)?;
        }
        (None, None) => {}
    }

    if !point.children().is_empty() {
        let ol_name = xot.add_name_ns("ol", xhtml_ns);
        let ol = xot.new_element(ol_name);
        for child in point.children() {
            let child_li = navigation_point_to_xml(xot, child, nav_path)?;
            append_nav_xml_child(xot, ol, child_li, nav_path)?;
        }
        append_nav_xml_child(xot, li, ol, nav_path)?;
    }

    Ok(li)
}

pub(super) fn set_nav_hidden_attribute(xot: &mut Xot, node: Node, hidden: bool) {
    if hidden {
        let hidden_name = xot.add_name("hidden");
        xot.set_attribute(node, hidden_name, "hidden");
    }
}

pub(super) fn set_nav_semantic_attributes(xot: &mut Xot, node: Node, point: &NavigationPoint) {
    let epub_type = point
        .authored_semantic_tokens()
        .iter()
        .filter(|token| matches!(token, SemanticToken::EpubType { .. }))
        .map(|token| token.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let epub_type = if epub_type.is_empty() {
        point
            .semantic()
            .map(|semantic| semantic.to_string())
            .unwrap_or_default()
    } else {
        epub_type
    };
    if !epub_type.is_empty() {
        let ops_ns = xot.add_namespace(OPS_NS);
        let epub_prefix = xot.add_prefix("epub");
        let type_name = xot.add_name_ns("type", ops_ns);
        xot.namespaces_mut(node).insert(epub_prefix, ops_ns);
        xot.set_attribute(node, type_name, epub_type);
    }

    let role = point
        .authored_semantic_tokens()
        .iter()
        .filter(|token| matches!(token, SemanticToken::AriaRole { .. }))
        .map(|token| token.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if !role.is_empty() {
        let role_name = xot.add_name("role");
        xot.set_attribute(node, role_name, role);
    }
}

pub(super) fn find_navigation_point_li_node(
    xot: &Xot,
    doc: Node,
    target: &NavPointEditTarget,
) -> Result<Node> {
    let nav = find_nav_list_node(xot, doc, target)?;
    nav_li_by_path(xot, nav, &target.point_path)
        .ok_or_else(|| EditError::source_node_missing(&target.nav_path))
}

pub(super) fn find_nav_list_node(
    xot: &Xot,
    doc: Node,
    target: &NavPointEditTarget,
) -> Result<Node> {
    let root = xot
        .document_element(doc)
        .map_err(|source| EditError::structural_xml(&target.nav_path, source))?;
    let mut navs = Vec::new();
    collect_nav_nodes(xot, root, &mut navs);
    navs.get(target.list_index)
        .copied()
        .ok_or_else(|| EditError::source_node_missing(&target.nav_path))
}

pub(super) fn collect_nav_nodes(xot: &Xot, node: Node, navs: &mut Vec<Node>) {
    if is_element_local_name(xot, node, "nav") {
        navs.push(node);
        return;
    }
    for child in xot.children(node) {
        collect_nav_nodes(xot, child, navs);
    }
}

pub(super) fn nav_li_by_path(xot: &Xot, nav: Node, path: &[usize]) -> Option<Node> {
    let (first, rest) = path.split_first()?;
    let li = direct_nav_li_children(xot, nav).get(*first).copied()?;
    if rest.is_empty() {
        Some(li)
    } else {
        nav_li_by_path(xot, li, rest)
    }
}

pub(super) fn direct_nav_li_children(xot: &Xot, node: Node) -> Vec<Node> {
    let mut lis = Vec::new();
    collect_direct_nav_li_children(xot, node, &mut lis);
    lis
}

pub(super) fn collect_direct_nav_li_children(xot: &Xot, node: Node, lis: &mut Vec<Node>) {
    for child in xot.children(node) {
        if is_element_local_name(xot, child, "nav") {
            continue;
        }
        if is_element_local_name(xot, child, "li") {
            lis.push(child);
        } else {
            collect_direct_nav_li_children(xot, child, lis);
        }
    }
}

pub(super) fn first_direct_navigation_point_child(
    xot: &Xot,
    li: Node,
    names: &[&str],
) -> Option<Node> {
    xot.children(li).find_map(|child| {
        if is_element_local_name(xot, child, "li") || is_element_local_name(xot, child, "nav") {
            return None;
        }
        if names
            .iter()
            .any(|name| is_element_local_name(xot, child, name))
        {
            Some(child)
        } else {
            first_direct_navigation_point_child(xot, child, names)
        }
    })
}

pub(super) fn first_direct_ol_child(xot: &Xot, node: Node) -> Option<Node> {
    xot.children(node)
        .find(|child| is_element_local_name(xot, *child, "ol"))
}

pub(super) fn create_ol_child(xot: &mut Xot, parent: Node, nav_path: &EpubPath) -> Result<Node> {
    let xhtml_ns = xot.add_namespace(XHTML_NS);
    let ol_name = xot.add_name_ns("ol", xhtml_ns);
    let ol = xot.new_element(ol_name);
    append_nav_xml_child(xot, parent, ol, nav_path)?;
    Ok(ol)
}

pub(super) fn append_nav_xml_child(
    xot: &mut Xot,
    parent: Node,
    child: Node,
    nav_path: &EpubPath,
) -> Result<()> {
    xot.append(parent, child)
        .map_err(|source| EditError::structural_xml(nav_path, source))?;
    Ok(())
}

pub(super) fn replace_nav_xml_text_content(
    xot: &mut Xot,
    node: Node,
    text: &str,
    nav_path: &EpubPath,
) -> Result<()> {
    let alt = xot.add_name("alt");
    let title = xot.add_name("title");
    let mut pending = xot.children(node).collect::<Vec<_>>();
    pending.reverse();
    let mut replaced = false;
    while let Some(child) = pending.pop() {
        let value = if replaced { "" } else { text };
        if let Some(text_node) = xot.text_mut(child) {
            text_node.set(value);
            replaced = true;
            continue;
        }
        let non_text = xot.element(child).is_some_and(|element| {
            let (name, namespace) = xot.name_ns_str(element.name());
            (namespace == XHTML_NS
                && matches!(
                    name,
                    "img" | "object" | "embed" | "iframe" | "audio" | "video" | "canvas" | "input"
                ))
                || (namespace == "http://www.w3.org/2000/svg" && name == "svg")
                || (namespace == "http://www.w3.org/1998/Math/MathML" && name == "math")
        });
        if non_text {
            let attribute = [alt, title]
                .into_iter()
                .find(|name| xot.attributes(child).get(*name).is_some());
            if let Some(attribute) = attribute {
                xot.attributes_mut(child)
                    .insert(attribute, value.to_owned());
                replaced = true;
                continue;
            }
        }
        let children = xot.children(child).collect::<Vec<_>>();
        pending.extend(children.into_iter().rev());
    }
    if replaced {
        return Ok(());
    }

    let text_node = xot.new_text(text);
    xot.append(node, text_node)
        .map_err(|source| EditError::structural_xml(nav_path, source))?;
    Ok(())
}
