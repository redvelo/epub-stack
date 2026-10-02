use super::*;

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    /// Writes bytes to a path, adding the file or replacing what is there.
    ///
    /// This is the raw operation: nothing declares the file in the manifest and no links are
    /// rewritten to point at it. The package document, the navigation documents and `mimetype`
    /// are off limits — edit those through the methods that understand them.
    pub fn upsert_resource(mut self, path: EpubPath, bytes: Vec<u8>) -> Result<Self> {
        reject_mimetype_edit(&path)?;
        self.reject_structural_path(&path)?;
        let size_bytes = bytes.len();
        self.changes.upsert(path.clone(), bytes);
        self.edit_changes
            .push(EditChange::UpsertResource { path, size_bytes });
        Ok(self)
    }

    /// Deletes the file at a path, leaving any links to it dangling.
    ///
    /// The file must exist in the staged book, which includes files added earlier in this same
    /// transaction.
    pub fn remove_resource(mut self, path: EpubPath) -> Result<Self> {
        reject_mimetype_edit(&path)?;
        self.reject_structural_path(&path)?;
        if !self.staged_resource_exists(&path) {
            return Err(EditError::MissingResource { path });
        }
        self.changes.remove(path.clone());
        self.edit_changes.push(EditChange::RemoveResource { path });
        Ok(self)
    }

    /// Stages removal of a declared resource path whether or not it has staged bytes.
    ///
    /// Compound manifest and reading-order removals use this so that a declaration whose href
    /// has no container entry stays removable.
    fn remove_declared_resource(mut self, path: EpubPath) -> Result<Self> {
        reject_mimetype_edit(&path)?;
        self.reject_structural_path(&path)?;
        if !self.staged_resource_exists(&path) {
            return Ok(self);
        }
        self.changes.remove(path.clone());
        self.edit_changes.push(EditChange::RemoveResource { path });
        Ok(self)
    }

    /// Stages replacement of the embedded annotation set and its referenced body resources.
    ///
    /// `removal` decides whether resources referenced only by the replaced set are removed.
    pub fn set_embedded_annotations(
        mut self,
        annotations: AnnotationBundle,
        removal: EmbeddedAnnotationResourceRemoval,
    ) -> Result<Self> {
        let current_paths = self
            .effective_embedded_annotation_resource_paths()?
            .unwrap_or_default();
        let new_paths = annotations
            .set()
            .audiovisual_body_resource_paths()
            .collect::<HashSet<_>>();
        for resource in annotations.resources() {
            if is_protected_embedded_annotation_resource_path(resource.path()) {
                return Err(AnnotationBundleError::ReservedPath {
                    path: resource.path().to_string(),
                }
                .into());
            }
            if !current_paths.contains(resource.path())
                && self.staged_resource_exists(&annotation_resource_path(resource.path())?)
            {
                return Err(EditError::AnnotationResourceExists {
                    path: resource.path().to_string(),
                });
            }
        }

        let annotations_bytes = annotations.set().to_json_string()?.into_bytes();
        self.stage_annotation_resource(annotations_json_path(), annotations_bytes);
        for resource in annotations.resources() {
            let path = annotation_resource_path(resource.path())?;
            self.stage_annotation_resource(path, resource.bytes().to_vec());
        }
        if removal == EmbeddedAnnotationResourceRemoval::SetAndReferencedResources {
            for path in current_paths.difference(&new_paths) {
                self.stage_annotation_removal(annotation_resource_path(path)?);
            }
        }
        Ok(self)
    }

    /// Stages removal of the embedded annotation set using the requested resource policy.
    pub fn remove_embedded_annotations(
        mut self,
        removal: EmbeddedAnnotationResourceRemoval,
    ) -> Result<Self> {
        let resource_paths = self.effective_embedded_annotation_resource_paths()?;
        self.stage_annotation_removal(annotations_json_path());
        if removal == EmbeddedAnnotationResourceRemoval::SetAndReferencedResources {
            for resource_path in resource_paths.into_iter().flatten() {
                if is_protected_embedded_annotation_resource_path(&resource_path) {
                    return Err(AnnotationBundleError::ReservedPath {
                        path: resource_path,
                    }
                    .into());
                }
                self.stage_annotation_removal(annotation_resource_path(&resource_path)?);
            }
        }
        Ok(self)
    }

    /// Stages resource bytes and a matching package manifest item together.
    ///
    /// The item's href must resolve to `path`; NAV resources require a dedicated structural
    /// operation and are rejected here.
    pub fn add_manifest_resource(
        self,
        path: EpubPath,
        bytes: Vec<u8>,
        item: ManifestItem,
    ) -> Result<Self> {
        required_manifest_item_id(&item)?;
        ensure_manifest_item_targets_path(&item, &path, self.epub.resources.package_path())?;
        if item.has_property(KnownManifestProperty::Nav) {
            return Err(EditError::NavigationManifestItem {
                id: item.id().map(ToString::to_string),
            });
        }
        self.upsert_resource(path, bytes)?.add_manifest_item(item)
    }

    /// Stages removal of one manifest item and its uniquely owned local resource.
    ///
    /// Structural and shared resource paths are rejected rather than silently broken.
    pub fn remove_manifest_resource(
        self,
        selector: impl Into<ManifestItemSelector>,
    ) -> Result<Self> {
        let selector = selector.into();
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        let (selected_index, selected) = unique_manifest_item(staged_package, &selector)?;
        let resource_path =
            manifest_item_resource_path(selected, self.epub.resources.package_path())?;
        reject_manifest_resource_structural_removal(&self, selected, &resource_path)?;
        reject_shared_manifest_resource_path(
            staged_package,
            selected_index,
            &resource_path,
            self.epub.resources.package_path(),
            false,
        )?;
        self.remove_manifest_item(selector)?
            .remove_declared_resource(resource_path)
    }

    /// Stages resource bytes, a matching manifest item, and a spine itemref together.
    pub fn add_spine_resource(
        self,
        path: EpubPath,
        bytes: Vec<u8>,
        item: ManifestItem,
        itemref: ItemRef,
    ) -> Result<Self> {
        let id = required_manifest_item_id(&item)?;
        let idref = required_spine_itemref_idref(&itemref)?;
        if !manifest_ids_equal(id, idref) {
            return Err(EditError::ItemRefTargetMismatch {
                idref: idref.to_string(),
                id: id.to_string(),
            });
        }
        self.add_manifest_resource(path, bytes, item)?
            .add_spine_itemref(itemref)
    }

    /// Stages removal of one itemref, its uniquely used manifest item, and its local bytes.
    pub fn remove_spine_resource(self, selector: impl Into<SpineItemRefSelector>) -> Result<Self> {
        let selector = selector.into();
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        let (_, _, idref) = unique_spine_itemref(staged_package, &selector)?;
        let references = staged_package
            .spine()
            .itemrefs()
            .iter()
            .filter(|itemref| {
                itemref
                    .idref()
                    .is_some_and(|value| manifest_ids_equal(value, idref))
            })
            .count();
        if references > 1 {
            return Err(EditError::SharedSpineItem {
                idref: idref.to_string(),
            });
        }
        let (item_index, item) = staged_package
            .manifest()
            .items()
            .iter()
            .enumerate()
            .find(|(_, item)| item.id().is_some_and(|id| manifest_ids_equal(id, idref)))
            .ok_or_else(|| PackageError::ManifestItemMissing {
                id: idref.to_string(),
            })?;
        let resource_path = manifest_item_resource_path(item, self.epub.resources.package_path())?;
        reject_manifest_resource_structural_removal(&self, item, &resource_path)?;
        let item = crate::resource::ManifestOrdinal::from_index(item_index);
        self.remove_spine_itemref(selector)?
            .remove_manifest_resource(item)
    }

    fn stage_annotation_resource(&mut self, path: EpubPath, bytes: Vec<u8>) {
        let size_bytes = bytes.len();
        self.changes.upsert(path.clone(), bytes);
        self.edit_changes
            .push(EditChange::UpsertResource { path, size_bytes });
    }

    fn stage_annotation_removal(&mut self, path: EpubPath) {
        self.changes.remove(path.clone());
        self.edit_changes.push(EditChange::RemoveResource { path });
    }

    fn effective_embedded_annotation_resource_paths(&self) -> Result<Option<HashSet<String>>> {
        let annotations_path = annotations_json_path();
        if !self.staged_resource_exists(&annotations_path) {
            return Ok(None);
        }
        let bytes = self.staged_bytes_bounded(&annotations_path, MAX_ANNOTATIONS_JSON_BYTES)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|source| AnnotationBundleError::InvalidUtf8 { source })?;
        let set = AnnotationSet::parse_json(text)?;
        Ok(Some(set.audiovisual_body_resource_paths().collect()))
    }
}

fn annotation_resource_path(path: &str) -> Result<EpubPath> {
    annotation_epub_path(format!("META-INF/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{AnnotationError, AnnotationResource};
    use crate::resource::provider::{
        MemoryResourceProvider, ProviderIndexError, ProviderReadError,
    };
    use std::cell::Cell;
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    fn memory_provider_epub() -> Epub<MemoryResourceProvider> {
        Epub::from_provider(
            memory_provider(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap()
    }

    fn memory_provider() -> MemoryResourceProvider {
        memory_provider_for(
            r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />"#,
            r#"<itemref idref="chap" />"#,
            [],
        )
    }

    fn memory_provider_for<const N: usize>(
        manifest: &str,
        spine: &str,
        extra: [(&str, &[u8]); N],
    ) -> MemoryResourceProvider {
        let package = format!(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>{manifest}</manifest><spine>{spine}</spine>
</package>"#
        );
        MemoryResourceProvider::from_entries(
            [
                ("EPUB/package.opf", package.into_bytes()),
                (
                    "EPUB/nav.xhtml",
                    br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol/></nav></body></html>"#.to_vec(),
                ),
                (
                    "EPUB/text/chapter.xhtml",
                    b"<html><body>Chapter</body></html>".to_vec(),
                ),
            ]
            .into_iter()
            .chain(extra.map(|(path, bytes)| (path, bytes.to_vec()))),
        )
        .unwrap()
    }

    fn memory_provider_with_extra_manifest_item() -> MemoryResourceProvider {
        memory_provider_for(
            r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="img" href="images/cover.jpg" media-type="image/jpeg" custom="keep" />"#,
            r#"<itemref idref="chap" />"#,
            [("EPUB/images/cover.jpg", b"jpeg")],
        )
    }

    fn memory_provider_with_duplicate_manifest_hrefs() -> MemoryResourceProvider {
        memory_provider_for(
            r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="img-a" href="images/cover.jpg" media-type="image/jpeg" />
    <item id="img-b" href="images/cover.jpg" media-type="image/jpeg" />"#,
            r#"<itemref idref="chap" />"#,
            [("EPUB/images/cover.jpg", b"jpeg")],
        )
    }

    fn memory_provider_with_two_spine_items() -> MemoryResourceProvider {
        memory_provider_for(
            r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="chap2" href="text/chapter2.xhtml" media-type="application/xhtml+xml" />"#,
            r#"<itemref idref="chap" /><itemref idref="chap2" />"#,
            [(
                "EPUB/text/chapter2.xhtml",
                b"<html><body>Chapter 2</body></html>",
            )],
        )
    }

    fn memory_provider_with_duplicate_spine_itemrefs() -> MemoryResourceProvider {
        memory_provider_for(
            r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />"#,
            r#"<itemref idref="chap" /><itemref idref="chap" />"#,
            [],
        )
    }

    fn embedded_annotations(target: &str, resources: &[(&str, &[u8])]) -> AnnotationBundle {
        let items: Vec<serde_json::Value> = if resources.is_empty() {
            vec![serde_json::json!({
                "id": "urn:uuid:annotation-0", "type": "Annotation",
                "created": "2026-07-15T00:00:00Z", "target": {"source": target}
            })]
        } else {
            resources
                .iter()
                .enumerate()
                .map(|(index, (path, _))| {
                    serde_json::json!({
                        "id": format!("urn:uuid:annotation-{index}"), "type": "Annotation",
                        "created": "2026-07-15T00:00:00Z", "target": {"source": target},
                        "body": {"type": "Audio", "id": path}
                    })
                })
                .collect()
        };
        let set = AnnotationSet::from_json_value(serde_json::json!({
            "@context": "https://www.w3.org/ns/epub-anno.jsonld", "id": "urn:uuid:set",
            "type": "AnnotationSet", "about": {}, "items": items
        }))
        .unwrap();
        let resources = resources
            .iter()
            .map(|(path, bytes)| AnnotationResource::new(*path, bytes.to_vec()))
            .collect();
        AnnotationBundle::new(set, resources).unwrap()
    }

    fn exported_entry<R: ResourceProvider>(epub: &Epub<R>, path: &str) -> Option<Vec<u8>> {
        let output = epub.export(Cursor::new(Vec::new())).unwrap().into_inner();
        let mut archive = ZipArchive::new(Cursor::new(output)).unwrap();
        let mut file = archive.by_name(path).ok()?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        Some(contents)
    }

    #[derive(Debug)]
    struct FailingIndexProvider {
        inner: MemoryResourceProvider,
        index_calls: Cell<usize>,
        entry_reader_calls: Cell<usize>,
        annotation_reads: Cell<usize>,
        fail_on_index_call: usize,
    }

    impl ResourceProvider for FailingIndexProvider {
        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> std::result::Result<T, ProviderReadError> {
            self.entry_reader_calls
                .set(self.entry_reader_calls.get() + 1);
            if path.as_str() == "META-INF/annotations.json" {
                self.annotation_reads.set(self.annotation_reads.get() + 1);
            }
            self.inner.read_with(path, read)
        }

        fn entries(
            &self,
        ) -> std::result::Result<impl Iterator<Item = (EpubPath, Option<u64>)>, ProviderIndexError>
        {
            let calls = self.index_calls.get() + 1;
            self.index_calls.set(calls);
            if calls == self.fail_on_index_call {
                return Err(ProviderIndexError::backend(std::io::Error::other(
                    "index failed",
                )));
            }
            self.inner.entries()
        }
    }

    #[test]
    fn edit_upsert_resource_preview_exposes_provider_only_resource_and_commits() {
        let mut epub = memory_provider_epub();
        let preview = epub
            .edit()
            .upsert_resource(
                EpubPath::new("EPUB/extra.xhtml").unwrap(),
                b"extra".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap();
        assert!(preview.changes().iter().any(|change| matches!(change, EditChange::UpsertResource { path, size_bytes } if path.as_str() == "EPUB/extra.xhtml" && *size_bytes == 5)));
        let staged = preview
            .resources()
            .resource_by_path(&EpubPath::new("EPUB/extra.xhtml").unwrap())
            .unwrap();
        assert!(staged.declarations().len() == 0);
        preview.commit();
        assert_eq!(
            epub.bytes(&EpubPath::new("EPUB/extra.xhtml").unwrap())
                .unwrap(),
            b"extra"
        );
    }

    #[test]
    fn edit_commit_applies_new_provider_only_resource() {
        let mut epub = memory_provider_epub();
        let report = epub
            .edit()
            .upsert_resource(
                EpubPath::new("EPUB/extra.xhtml").unwrap(),
                b"extra".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(report.len(), 1);
        assert_eq!(
            epub.bytes(&EpubPath::new("EPUB/extra.xhtml").unwrap())
                .unwrap(),
            b"extra".to_vec()
        );
    }

    #[test]
    fn edit_add_manifest_resource_stages_bytes_and_manifest_item() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("img").unwrap())
            .href(EpubHref::try_new("images/cover.jpg").unwrap())
            .media_type(EpubString::try_new("image/jpeg").unwrap().into())
            .build()
            .unwrap();
        epub.edit()
            .add_manifest_resource(
                EpubPath::new("EPUB/images/cover.jpg").unwrap(),
                b"jpeg".to_vec(),
                item,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("img").is_some());
        assert_eq!(
            epub.bytes(&EpubPath::new("EPUB/images/cover.jpg").unwrap())
                .unwrap(),
            b"jpeg"
        );
    }

    #[test]
    fn edit_add_manifest_resource_rejects_href_path_mismatch() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("img").unwrap())
            .href(EpubHref::try_new("images/cover.jpg").unwrap())
            .media_type(EpubString::try_new("image/jpeg").unwrap().into())
            .build()
            .unwrap();
        let err = epub
            .edit()
            .add_manifest_resource(
                EpubPath::new("EPUB/images/other.jpg").unwrap(),
                b"jpeg".to_vec(),
                item,
            )
            .unwrap_err();
        assert!(matches!(err, EditError::ManifestHrefMismatch { .. }));
    }

    #[test]
    fn edit_remove_manifest_resource_removes_item_and_provider_bytes() {
        let mut epub = Epub::from_provider(
            memory_provider_with_extra_manifest_item(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap();
        epub.edit()
            .remove_manifest_resource(ManifestItemSelector::Id(
                EpubString::try_new("img").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("img").is_none());
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/images/cover.jpg").unwrap()),
            Err(crate::resource::ResourceReadError::Missing { .. })
        ));
    }

    #[test]
    fn edit_remove_manifest_resource_accepts_a_declaration_without_provider_bytes() {
        let mut epub = Epub::from_provider(
            memory_provider_for(
                r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="img" href="images/cover.jpg" media-type="image/jpeg" />"#,
                r#"<itemref idref="chap" />"#,
                [],
            ),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap();
        let changes = epub
            .edit()
            .remove_manifest_resource(ManifestItemSelector::Id(
                EpubString::try_new("img").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("img").is_none());
        assert!(
            !changes
                .iter()
                .any(|change| matches!(change, EditChange::RemoveResource { .. }))
        );
    }

    #[test]
    fn edit_remove_resource_rejects_a_path_without_staged_bytes() {
        let mut epub = memory_provider_epub();
        let err = epub
            .edit()
            .remove_resource(EpubPath::new("EPUB/images/absent.jpg").unwrap())
            .unwrap_err();
        assert!(matches!(err, EditError::MissingResource { .. }));
    }

    #[test]
    fn edit_remove_manifest_resource_rejects_shared_provider_path() {
        let mut epub = Epub::from_provider(
            memory_provider_with_duplicate_manifest_hrefs(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap();
        let err = epub
            .edit()
            .remove_manifest_resource(ManifestItemSelector::Id(
                EpubString::try_new("img-a").unwrap(),
            ))
            .unwrap_err();
        assert!(matches!(err, EditError::SharedResourcePath { .. }));
    }

    #[test]
    fn edit_add_then_remove_manifest_resource_in_same_transaction() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("img").unwrap())
            .href(EpubHref::try_new("images/cover.jpg").unwrap())
            .media_type(EpubString::try_new("image/jpeg").unwrap().into())
            .build()
            .unwrap();
        epub.edit()
            .add_manifest_resource(
                EpubPath::new("EPUB/images/cover.jpg").unwrap(),
                b"jpeg".to_vec(),
                item,
            )
            .unwrap()
            .remove_manifest_resource(ManifestItemSelector::Id(
                EpubString::try_new("img").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("img").is_none());
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/images/cover.jpg").unwrap()),
            Err(crate::resource::ResourceReadError::Missing { .. })
        ));
    }

    #[test]
    fn edit_add_spine_resource_stages_chapter_manifest_and_spine() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("chap2").unwrap())
            .href(EpubHref::try_new("text/chapter2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();
        epub.edit()
            .add_spine_resource(
                EpubPath::new("EPUB/text/chapter2.xhtml").unwrap(),
                b"<html><body>Chapter 2</body></html>".to_vec(),
                item,
                ItemRef::new("chap2").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("chap2").is_some());
        assert_eq!(epub.package().spine().itemrefs().len(), 2);
        assert_eq!(epub.package().spine().itemrefs()[1].idref(), Some("chap2"));
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/text/chapter2.xhtml").unwrap())
                .unwrap()
                .contains("Chapter 2")
        );
    }

    #[test]
    fn edit_remove_spine_resource_removes_itemref_manifest_and_provider_bytes() {
        let mut epub = Epub::from_provider(
            memory_provider_with_two_spine_items(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap();
        epub.edit()
            .remove_spine_resource(SpineItemRefSelector::Idref(
                EpubString::try_new("chap2").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.package().manifest_item_by_id("chap2").is_none());
        assert_eq!(epub.package().spine().itemrefs().len(), 1);
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/text/chapter2.xhtml").unwrap()),
            Err(crate::resource::ResourceReadError::Missing { .. })
        ));
    }

    #[test]
    fn edit_remove_spine_resource_rejects_shared_itemref() {
        let mut epub = Epub::from_provider(
            memory_provider_with_duplicate_spine_itemrefs(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap();
        let err = epub
            .edit()
            .remove_spine_resource(SpineItemRefSelector::Ordinal(
                crate::resource::ReadingOrderOrdinal::from_index(0),
            ))
            .unwrap_err();
        assert!(matches!(err, EditError::SharedSpineItem { .. }));
    }

    #[test]
    fn staged_addition_then_removal_is_not_reported() {
        let mut epub = memory_provider_epub();
        let path = EpubPath::new("EPUB/extra.xhtml").unwrap();
        let preview = epub
            .edit()
            .upsert_resource(path.clone(), b"extra".to_vec())
            .unwrap()
            .remove_resource(path.clone())
            .unwrap()
            .preview()
            .unwrap();
        assert!(preview.changes().is_empty());
        assert!(preview.resources().resource_by_path(&path).is_none());
    }

    #[test]
    fn raw_navigation_edit_is_rejected_when_staged() {
        let mut epub = memory_provider_epub();
        let error = epub
            .edit()
            .upsert_resource(EpubPath::new("EPUB/nav.xhtml").unwrap(), b"nav".to_vec())
            .unwrap_err();
        assert!(matches!(
            error,
            EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Navigation,
                ..
            }
        ));
    }

    #[test]
    fn edit_replace_resource_commits() {
        let mut epub = memory_provider_epub();
        let report = epub
            .edit()
            .upsert_resource(
                EpubPath::new("EPUB/text/chapter.xhtml").unwrap(),
                b"replacement".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(report.len(), 1);
        assert_eq!(
            epub.bytes(&EpubPath::new("EPUB/text/chapter.xhtml").unwrap())
                .unwrap(),
            b"replacement".to_vec()
        );
    }

    #[test]
    fn coordinated_resource_removal_rejects_the_canonical_mimetype_entry() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="mime" href="../mimetype" media-type="text/plain"/></manifest><spine/></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("mimetype", b"application/epub+zip".to_vec()),
            ("EPUB/package.opf", package.to_vec()),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let error = epub
            .edit()
            .remove_manifest_resource(ManifestItemSelector::Id(
                EpubString::try_new("mime").unwrap(),
            ))
            .unwrap_err();
        assert!(matches!(error, EditError::MimetypeResourceEdit));
        assert!(epub.package().manifest_item_by_id("mime").is_some());
        assert_eq!(
            exported_entry(&epub, "mimetype").unwrap(),
            b"application/epub+zip"
        );
    }

    #[test]
    fn structural_xml_decode_errors_preserve_edit_path() {
        let path = EpubPath::new("EPUB/package.opf").unwrap();
        let error =
            decode_structural_xml(b"<?xml version='1.0' encoding='UTF-32'?><package/>", &path)
                .unwrap_err();
        assert!(
            matches!(error, EditError::StructuralXmlDecode { path: error_path, source: crate::XmlDecodeError::UnsupportedEncoding { ref encoding } } if error_path == path && encoding == "UTF-32")
        );
    }

    #[test]
    fn edit_sets_embedded_annotations_and_resources() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("audio/note.mp3", b"audio")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(
            epub.embedded_annotations().unwrap().unwrap().resources()[0].bytes(),
            b"audio"
        );
    }

    #[test]
    fn edit_removes_embedded_annotation_set_only() {
        let mut epub = memory_provider_epub();
        epub.resource_changes.upsert(
            EpubPath::new("META-INF/annotations.json").unwrap(),
            br#"{"id":"urn:uuid:set","type":"AnnotationSet","about":{},"items":[]}"#.to_vec(),
        );
        epub.resource_changes.upsert(
            EpubPath::new("META-INF/audio/kept.mp3").unwrap(),
            b"audio".to_vec(),
        );
        epub.edit()
            .remove_embedded_annotations(EmbeddedAnnotationResourceRemoval::SetOnly)
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.embedded_annotations().unwrap().is_none());
        assert_eq!(
            epub.resource_changes
                .entry(&EpubPath::new("META-INF/audio/kept.mp3").unwrap())
                .unwrap()
                .unwrap(),
            b"audio"
        );
    }

    #[test]
    fn embedded_annotation_replacement_does_not_remove_stale_resources() {
        let mut epub = memory_provider_epub();
        let first = embedded_annotations(
            "text/chapter.xhtml",
            &[("a.mp3", b"a"), ("shared.mp3", b"old")],
        );
        let second = embedded_annotations(
            "text/chapter.xhtml",
            &[("shared.mp3", b"new"), ("b.mp3", b"b")],
        );
        epub.edit()
            .set_embedded_annotations(first, EmbeddedAnnotationResourceRemoval::SetOnly)
            .unwrap()
            .set_embedded_annotations(second, EmbeddedAnnotationResourceRemoval::SetOnly)
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        let embedded = epub.embedded_annotations().unwrap().unwrap();
        let paths = embedded
            .resources()
            .iter()
            .map(|resource| resource.path())
            .collect::<HashSet<_>>();
        assert_eq!(paths, HashSet::from(["shared.mp3", "b.mp3"]));
        assert_eq!(embedded.resources()[0].bytes(), b"new");
        assert_eq!(exported_entry(&epub, "META-INF/a.mp3").unwrap(), b"a");
    }

    #[test]
    fn replacement_can_remove_resources_only_the_replaced_set_referenced() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"a")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("b.mp3", b"b")]),
                EmbeddedAnnotationResourceRemoval::SetAndReferencedResources,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(exported_entry(&epub, "META-INF/a.mp3").is_none());
        assert_eq!(
            epub.embedded_annotations().unwrap().unwrap().resources()[0].path(),
            "b.mp3"
        );
    }

    #[test]
    fn embedded_annotation_resource_path_can_be_reused_in_same_transaction() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"old")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("b.mp3", b"b")]),
                EmbeddedAnnotationResourceRemoval::SetAndReferencedResources,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"new")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        let embedded = epub.embedded_annotations().unwrap().unwrap();
        assert_eq!(embedded.resources()[0].path(), "a.mp3");
        assert_eq!(embedded.resources()[0].bytes(), b"new");
        assert_eq!(exported_entry(&epub, "META-INF/a.mp3").unwrap(), b"new");
    }

    #[test]
    fn retained_stale_annotation_resource_is_not_overwritten() {
        let mut epub = memory_provider_epub();
        let error = epub
            .edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"old")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("b.mp3", b"b")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"new")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            EditError::AnnotationResourceExists { ref path } if path == "a.mp3"
        ));
    }

    #[test]
    fn malformed_current_embedded_annotation_set_error_propagates() {
        let provider = MemoryResourceProvider::from_entries(
            memory_provider()
                .into_inner()
                .into_iter()
                .map(|(path, bytes)| (path.as_str().to_string(), bytes))
                .chain([("META-INF/annotations.json".into(), b"{".to_vec())]),
        )
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let error = epub
            .edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            EditError::Annotations {
                source: EmbeddedAnnotationsError::Annotation {
                    source: AnnotationError::Json { .. }
                }
            }
        ));
        assert_eq!(
            exported_entry(&epub, "META-INF/annotations.json").unwrap(),
            b"{"
        );
    }

    #[test]
    fn staged_embedded_annotations_can_be_removed_with_their_resources() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"a")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .remove_embedded_annotations(
                EmbeddedAnnotationResourceRemoval::SetAndReferencedResources,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(epub.embedded_annotations().unwrap().is_none());
        assert!(exported_entry(&epub, "META-INF/a.mp3").is_none());
    }

    #[test]
    fn removed_embedded_annotations_can_be_replaced_in_the_same_transaction() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"old")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        epub.edit()
            .remove_embedded_annotations(
                EmbeddedAnnotationResourceRemoval::SetAndReferencedResources,
            )
            .unwrap()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"new")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(
            epub.embedded_annotations().unwrap().unwrap().resources()[0].bytes(),
            b"new"
        );
    }

    #[test]
    fn embedded_annotation_target_addition_is_order_independent() {
        for set_first in [false, true] {
            let mut provider = memory_provider();
            provider.remove(&EpubPath::new("EPUB/text/chapter.xhtml").unwrap());
            let mut epub =
                Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
            let annotations = embedded_annotations("text/chapter.xhtml", &[]);
            let edit = epub.edit();
            let edit = if set_first {
                edit.set_embedded_annotations(
                    annotations,
                    EmbeddedAnnotationResourceRemoval::SetOnly,
                )
                .unwrap()
                .upsert_resource(
                    EpubPath::new("EPUB/text/chapter.xhtml").unwrap(),
                    b"<html><body>Restored</body></html>".to_vec(),
                )
                .unwrap()
            } else {
                edit.upsert_resource(
                    EpubPath::new("EPUB/text/chapter.xhtml").unwrap(),
                    b"<html><body>Restored</body></html>".to_vec(),
                )
                .unwrap()
                .set_embedded_annotations(annotations, EmbeddedAnnotationResourceRemoval::SetOnly)
                .unwrap()
            };
            edit.preview().unwrap();
        }
    }

    #[test]
    fn embedded_annotation_final_target_removal_is_inspectable() {
        let mut epub = memory_provider_epub();
        let preview = epub
            .edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .remove_resource(EpubPath::new("EPUB/text/chapter.xhtml").unwrap())
            .unwrap()
            .preview()
            .unwrap();
        assert!(matches!(
            preview
                .embedded_annotations()
                .unwrap()
                .unwrap()
                .set()
                .items()[0]
                .target()
                .unwrap()
                .resolve_source(preview.resources()),
            Err(crate::annotation::AnnotationSourceError::ProviderMissing { .. })
        ));
    }

    #[test]
    fn generic_annotations_json_upsert_exposes_missing_target_at_preview() {
        let mut epub = memory_provider_epub();
        let bytes = embedded_annotations("missing.xhtml", &[])
            .set()
            .to_json_string()
            .unwrap()
            .into_bytes();
        let preview = epub
            .edit()
            .upsert_resource(EpubPath::new("META-INF/annotations.json").unwrap(), bytes)
            .unwrap()
            .preview()
            .unwrap();
        assert!(matches!(
            preview
                .embedded_annotations()
                .unwrap()
                .unwrap()
                .set()
                .items()[0]
                .target()
                .unwrap()
                .resolve_source(preview.resources()),
            Err(crate::annotation::AnnotationSourceError::Missing)
        ));
    }

    #[test]
    fn generic_oversized_annotation_adjacent_resource_is_not_annotation_preview_work() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .set_embedded_annotations(
                embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"audio")]),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        let preview = epub
            .edit()
            .upsert_resource(
                EpubPath::new("META-INF/a.mp3").unwrap(),
                vec![0; (crate::annotation::MAX_ARCHIVE_RESOURCE_BYTES + 1) as usize],
            )
            .unwrap()
            .preview()
            .unwrap();
        assert!(preview.changes().iter().any(|change| matches!(change, EditChange::UpsertResource { path, .. } if path.as_str() == "META-INF/a.mp3")));
    }

    #[test]
    fn generic_meta_inf_edit_does_not_load_unchanged_annotations() {
        let annotations = embedded_annotations("text/chapter.xhtml", &[("a.mp3", b"audio")]);
        let provider = MemoryResourceProvider::from_entries(
            memory_provider()
                .into_inner()
                .into_iter()
                .map(|(path, bytes)| (path.as_str().to_string(), bytes))
                .chain([
                    (
                        "META-INF/annotations.json".into(),
                        annotations.set().to_json_string().unwrap().into_bytes(),
                    ),
                    ("META-INF/a.mp3".into(), b"audio".to_vec()),
                ]),
        )
        .unwrap();
        let provider = FailingIndexProvider {
            inner: provider,
            index_calls: Cell::new(0),
            entry_reader_calls: Cell::new(0),
            annotation_reads: Cell::new(0),
            fail_on_index_call: usize::MAX,
        };
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        epub.container.annotation_reads.set(0);
        let preview = epub
            .edit()
            .upsert_resource(
                EpubPath::new("META-INF/a.mp3").unwrap(),
                b"replacement".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap();
        assert_eq!(preview.epub.container.annotation_reads.get(), 0);
    }

    #[test]
    fn generic_annotations_json_upsert_accepts_recovered_target_state() {
        let mut epub = memory_provider_epub();
        let bytes = br#"{"id":"urn:test:set","type":"AnnotationSet","about":{},"items":[{"id":"urn:test:annotation","type":"Annotation","created":"2026-07-16T12:00:00Z","target":{"source":"https://example.com/chapter.xhtml"}}]}"#.to_vec();
        epub.edit()
            .upsert_resource(EpubPath::new("META-INF/annotations.json").unwrap(), bytes)
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(
            epub.embedded_annotations().unwrap().unwrap().set().items()[0]
                .target()
                .unwrap()
                .source(),
            Some("https://example.com/chapter.xhtml")
        );
    }

    #[test]
    fn semantic_embedded_annotations_accept_recovered_target_state() {
        let mut epub = memory_provider_epub();
        let set = AnnotationSet::parse_json(r#"{"id":"urn:test:set","type":"AnnotationSet","about":{},"items":[{"id":"urn:test:annotation","type":"Annotation","created":"2026-07-16T12:00:00Z","target":{"source":"https://example.com/chapter.xhtml"}}]}"#).unwrap();
        epub.edit()
            .set_embedded_annotations(
                AnnotationBundle::new(set, Vec::new()).unwrap(),
                EmbeddedAnnotationResourceRemoval::SetOnly,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert_eq!(
            epub.embedded_annotations().unwrap().unwrap().set().items()[0]
                .target()
                .unwrap()
                .source(),
            Some("https://example.com/chapter.xhtml")
        );
    }

    fn extra_manifest_item() -> ManifestItem {
        ManifestItem::builder()
            .id(EpubString::try_new("extra").unwrap())
            .href(EpubHref::try_new("extra.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap()
    }

    #[test]
    fn embedded_annotation_staged_manifest_addition_is_order_independent() {
        for set_first in [false, true] {
            let mut epub = memory_provider_epub();
            let annotations = embedded_annotations("extra.xhtml", &[]);
            let edit = epub.edit();
            let edit = if set_first {
                edit.set_embedded_annotations(
                    annotations,
                    EmbeddedAnnotationResourceRemoval::SetOnly,
                )
                .unwrap()
                .add_manifest_resource(
                    EpubPath::new("EPUB/extra.xhtml").unwrap(),
                    b"<html><body>Extra</body></html>".to_vec(),
                    extra_manifest_item(),
                )
                .unwrap()
            } else {
                edit.add_manifest_resource(
                    EpubPath::new("EPUB/extra.xhtml").unwrap(),
                    b"<html><body>Extra</body></html>".to_vec(),
                    extra_manifest_item(),
                )
                .unwrap()
                .set_embedded_annotations(annotations, EmbeddedAnnotationResourceRemoval::SetOnly)
                .unwrap()
            };
            edit.preview().unwrap();
        }
    }

    #[test]
    fn embedded_annotation_staged_manifest_removal_is_inspectable_in_either_order() {
        let mut epub = memory_provider_epub();
        epub.edit()
            .add_manifest_resource(
                EpubPath::new("EPUB/extra.xhtml").unwrap(),
                b"<html><body>Extra</body></html>".to_vec(),
                extra_manifest_item(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        for set_first in [false, true] {
            let annotations = embedded_annotations("extra.xhtml", &[]);
            let edit = epub.edit();
            let selector = || ManifestItemSelector::AuthoredHref(AuthoredHref::new("extra.xhtml"));
            let edit = if set_first {
                edit.set_embedded_annotations(
                    annotations,
                    EmbeddedAnnotationResourceRemoval::SetOnly,
                )
                .unwrap()
                .remove_manifest_resource(selector())
                .unwrap()
            } else {
                edit.remove_manifest_resource(selector())
                    .unwrap()
                    .set_embedded_annotations(
                        annotations,
                        EmbeddedAnnotationResourceRemoval::SetOnly,
                    )
                    .unwrap()
            };
            let preview = edit.preview().unwrap();
            assert!(matches!(
                preview
                    .embedded_annotations()
                    .unwrap()
                    .unwrap()
                    .set()
                    .items()[0]
                    .target()
                    .unwrap()
                    .resolve_source(preview.resources()),
                Err(crate::annotation::AnnotationSourceError::Missing)
            ));
        }
    }

    #[test]
    fn embedded_annotations_expose_non_manifest_targets() {
        let provider = MemoryResourceProvider::from_entries(
            memory_provider()
                .into_inner()
                .into_iter()
                .map(|(path, bytes)| (path.as_str().to_string(), bytes))
                .chain([("EPUB/orphan.xhtml".into(), b"orphan".to_vec())]),
        )
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        for target in ["package.opf", "orphan.xhtml"] {
            let preview = epub
                .edit()
                .set_embedded_annotations(
                    embedded_annotations(target, &[]),
                    EmbeddedAnnotationResourceRemoval::SetOnly,
                )
                .unwrap()
                .preview()
                .unwrap();
            assert!(matches!(
                preview
                    .embedded_annotations()
                    .unwrap()
                    .unwrap()
                    .set()
                    .items()[0]
                    .target()
                    .unwrap()
                    .resolve_source(preview.resources()),
                Err(crate::annotation::AnnotationSourceError::Missing)
            ));
        }
    }
}
