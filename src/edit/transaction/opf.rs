use super::*;

fn same_manifest(left: &Package, right: &Package) -> bool {
    left.manifest() == right.manifest()
}

fn same_metadata(left: &Package, right: &Package) -> bool {
    left.metadata() == right.metadata()
}

fn same_spine(left: &Package, right: &Package) -> bool {
    left.spine().itemrefs() == right.spine().itemrefs()
}

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    fn staged_package(&self) -> Package {
        self.package_override
            .clone()
            .unwrap_or_else(|| self.epub.package.clone())
    }

    fn reject_navigation_manifest_item(package: &Package, item: &ManifestItem) -> Result<()> {
        let id = item.id();
        let is_ncx = package
            .spine()
            .toc()
            .is_some_and(|toc| id.is_some_and(|id| manifest_ids_equal(toc, id)));
        if item.has_property(KnownManifestProperty::Nav) || is_ncx {
            return Err(EditError::NavigationManifestItem {
                id: id.map(ToString::to_string),
            });
        }
        Ok(())
    }

    /// Rewrites package XML and checks that the reparsed package agrees with `expected`.
    fn apply_package_edit(
        mut self,
        expected: Package,
        same: fn(&Package, &Package) -> bool,
        mutate: impl FnOnce(&mut Xot, Node, &EpubPath) -> Result<()>,
    ) -> Result<Self> {
        let package_path = self.epub.resources.package_path().clone();
        let size_bytes = self.stage_package_edit(
            |xot, doc| mutate(xot, doc, &package_path),
            |package| {
                if same(package, &expected) {
                    Ok(())
                } else {
                    Err(EditError::model_mismatch(&package_path))
                }
            },
        )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: package_path,
                kind: StructuralResourceKind::Package,
                size_bytes,
            });
        Ok(self)
    }

    /// Stages a non-NAV manifest item at the end of the package manifest.
    pub fn add_manifest_item(self, item: ManifestItem) -> Result<Self> {
        required_manifest_item_id(&item)?;
        if item.has_property(KnownManifestProperty::Nav) {
            return Err(EditError::NavigationManifestItem {
                id: item.id().map(ToString::to_string),
            });
        }
        let mut expected = self.staged_package();
        expected.add_manifest_item(item.clone())?;
        self.apply_package_edit(expected, same_manifest, |xot, doc, path| {
            append_manifest_item_to_package_xml(xot, doc, &item, path)
        })
    }

    /// Stages removal of one uniquely selected manifest item not used by the spine.
    pub fn remove_manifest_item(self, selector: impl Into<ManifestItemSelector>) -> Result<Self> {
        let mut expected = self.staged_package();
        let (index, selected) = unique_manifest_item(&expected, &selector.into())?;
        Self::reject_navigation_manifest_item(&expected, selected)?;
        expected.remove_manifest_item_at(index)?;
        self.apply_package_edit(expected, same_manifest, |xot, doc, path| {
            remove_manifest_item_at_from_package_xml(xot, doc, index, path)
        })
    }

    /// Stages replacement of one uniquely selected non-NAV manifest item.
    pub fn replace_manifest_item(
        self,
        selector: impl Into<ManifestItemSelector>,
        item: ManifestItem,
    ) -> Result<Self> {
        required_manifest_item_id(&item)?;
        let mut expected = self.staged_package();
        let (index, selected) = unique_manifest_item(&expected, &selector.into())?;
        Self::reject_navigation_manifest_item(&expected, selected)?;
        if item.has_property(KnownManifestProperty::Nav) {
            return Err(EditError::NavigationManifestItem {
                id: item.id().map(ToString::to_string),
            });
        }
        expected.replace_manifest_item_at(index, item.clone())?;
        self.apply_package_edit(expected, same_manifest, |xot, doc, path| {
            replace_manifest_item_at_in_package_xml(xot, doc, index, &item, path)
        })
    }

    /// Stages a Dublin Core element at the end of package metadata.
    pub fn add_metadata_element(self, kind: DcElement, element: Element) -> Result<Self> {
        let mut expected = self.staged_package();
        expected.metadata_mut().add_element(kind, element.clone());
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            append_metadata_element_to_package_xml(xot, doc, kind, &element, path)
        })
    }

    /// Stages an EPUB 3 `meta` element at the end of package metadata.
    pub fn add_meta(self, meta: Meta) -> Result<Self> {
        let mut expected = self.staged_package();
        expected.metadata_mut().add_meta(meta.clone());
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            append_meta_to_package_xml(xot, doc, &meta, path)
        })
    }

    /// Stages a `link` element at the end of package metadata.
    pub fn add_metadata_link(self, link: MetadataLink) -> Result<Self> {
        let mut expected = self.staged_package();
        expected.metadata_mut().add_link(link.clone());
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            append_metadata_link_to_package_xml(xot, doc, &link, path)
        })
    }

    /// Stages removal of one uniquely selected Dublin Core metadata element.
    pub fn remove_metadata_element(self, selector: MetadataElementSelector) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_metadata_element(&expected, &selector)?;
        let element = selector.element;
        expected.metadata_mut().remove_element_at(element, index)?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            remove_metadata_element_from_package_xml(xot, doc, element.local_name(), index, path)
        })
    }

    /// Stages replacement of one uniquely selected Dublin Core element.
    pub fn replace_metadata_element(
        self,
        selector: MetadataElementSelector,
        value: Element,
    ) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_metadata_element(&expected, &selector)?;
        let element = selector.element;
        expected
            .metadata_mut()
            .replace_element_at(element, index, value.clone())?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            replace_metadata_element_in_package_xml(
                xot,
                doc,
                element.local_name(),
                index,
                &value,
                path,
            )
        })
    }

    /// Stages removal of one uniquely selected EPUB 3 metadata `meta` element.
    pub fn remove_meta(self, selector: MetaSelector) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_meta(&expected, &selector)?;
        expected.metadata_mut().remove_meta_at(index)?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            remove_meta_from_package_xml(xot, doc, index, path)
        })
    }

    /// Stages replacement of one uniquely selected EPUB 3 metadata `meta` element.
    pub fn replace_meta(self, selector: MetaSelector, meta: Meta) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_meta(&expected, &selector)?;
        expected
            .metadata_mut()
            .replace_meta_at(index, meta.clone())?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            replace_meta_in_package_xml(xot, doc, index, &meta, path)
        })
    }

    /// Stages removal of one uniquely selected package metadata `link` element.
    pub fn remove_metadata_link(self, selector: MetadataLinkSelector) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_metadata_link(&expected, &selector)?;
        expected.metadata_mut().remove_link_at(index)?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            remove_metadata_link_from_package_xml(xot, doc, index, path)
        })
    }

    /// Stages replacement of one uniquely selected package metadata `link` element.
    pub fn replace_metadata_link(
        self,
        selector: MetadataLinkSelector,
        link: MetadataLink,
    ) -> Result<Self> {
        let mut expected = self.staged_package();
        let index = unique_metadata_link(&expected, &selector)?;
        expected
            .metadata_mut()
            .replace_link_at(index, link.clone())?;
        self.apply_package_edit(expected, same_metadata, |xot, doc, path| {
            replace_metadata_link_in_package_xml(xot, doc, index, &link, path)
        })
    }

    /// Stages an itemref at the end of the spine for an existing manifest target.
    pub fn add_spine_itemref(self, itemref: ItemRef) -> Result<Self> {
        required_spine_itemref_idref(&itemref)?;
        let mut expected = self.staged_package();
        expected.add_spine_itemref(itemref.clone())?;
        self.apply_package_edit(expected, same_spine, |xot, doc, path| {
            append_spine_itemref_to_package_xml(xot, doc, &itemref, path)
        })
    }

    /// Stages removal of one uniquely selected spine itemref.
    pub fn remove_spine_itemref(self, selector: impl Into<SpineItemRefSelector>) -> Result<Self> {
        let mut expected = self.staged_package();
        let (index, _, _) = unique_spine_itemref(&expected, &selector.into())?;
        expected.remove_spine_itemref_at(index)?;
        self.apply_package_edit(expected, same_spine, |xot, doc, path| {
            remove_spine_itemref_from_package_xml(xot, doc, index, path)
        })
    }

    /// Stages replacement of one uniquely selected spine itemref with a resolvable itemref.
    pub fn replace_spine_itemref(
        self,
        selector: impl Into<SpineItemRefSelector>,
        itemref: ItemRef,
    ) -> Result<Self> {
        required_spine_itemref_idref(&itemref)?;
        let mut expected = self.staged_package();
        let (index, _, _) = unique_spine_itemref(&expected, &selector.into())?;
        expected.replace_spine_itemref_at(index, itemref.clone())?;
        self.apply_package_edit(expected, same_spine, |xot, doc, path| {
            replace_spine_itemref_in_package_xml(xot, doc, index, &itemref, path)
        })
    }

    /// Stages moving one selected itemref to a zero-based final spine index.
    pub fn move_spine_itemref(
        self,
        selector: impl Into<SpineItemRefSelector>,
        index: usize,
    ) -> Result<Self> {
        let mut expected = self.staged_package();
        let (from_index, _, _) = unique_spine_itemref(&expected, &selector.into())?;
        expected.move_spine_itemref(from_index, index)?;
        self.apply_package_edit(expected, same_spine, |xot, doc, path| {
            move_spine_itemref_in_package_xml(xot, doc, from_index, index, path)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::DcElement;
    use crate::{container::EpubZip, resource::provider::MemoryResourceProvider};
    use std::io::{Cursor, Read, Write};
    use zip::write::SimpleFileOptions;
    use zip::{ZipArchive, ZipWriter};

    fn memory_provider_epub() -> Epub<MemoryResourceProvider> {
        Epub::from_provider(
            memory_provider(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap()
    }

    fn memory_provider() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap()
    }

    fn memory_provider_with_extra_manifest_item() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="img" href="images/cover.jpg" media-type="image/jpeg" custom="keep" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            ("EPUB/images/cover.jpg", b"jpeg".to_vec()),
        ])
        .unwrap()
    }

    fn memory_provider_with_duplicate_manifest_hrefs() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="img-a" href="images/cover.jpg" media-type="image/jpeg" />
    <item id="img-b" href="images/cover.jpg" media-type="image/jpeg" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            ("EPUB/images/cover.jpg", b"jpeg".to_vec()),
        ])
        .unwrap()
    }

    fn duplicate_manifest_id_edit_epub() -> Epub<EpubZip<Cursor<Vec<u8>>>> {
        package_source_edit_epub(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="duplicate" href="images/first.jpg" media-type="image/jpeg" marker="first" />
    <item id="duplicate" href="images/second.jpg" media-type="image/jpeg" marker="second" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#,
        )
    }

    fn memory_provider_with_two_spine_items() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="chap2" href="text/chapter2.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /><itemref idref="chap2" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            (
                "EPUB/text/chapter2.xhtml",
                b"<html><body>Chapter 2</body></html>".to_vec(),
            ),
        ])
        .unwrap()
    }

    fn memory_provider_with_duplicate_spine_itemrefs() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap()
    }

    fn memory_provider_with_prefixed_opf_package() -> MemoryResourceProvider {
        let package = r##"<opf:package xmlns:opf="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <opf:metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></opf:metadata>
  <opf:manifest>
    <opf:item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <opf:item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </opf:manifest>
  <opf:spine><opf:itemref idref="chap" /></opf:spine>
</opf:package>"##;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap()
    }
    fn ncx_only_edit_epub() -> Epub<EpubZip<Cursor<Vec<u8>>>> {
        let mut data = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut data);
            let options = SimpleFileOptions::default();
            write_zip_entry(&mut zip, "mimetype", "application/epub+zip", options);
            write_zip_entry(
                &mut zip,
                "META-INF/container.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/package.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/toc.ncx",
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap></ncx>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/text/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Text</p></body></html>"#,
                options,
            );
            zip.finish().unwrap();
        }
        data.set_position(0);
        EpubZip::from_reader(data)
            .unwrap()
            .default_rendition()
            .unwrap()
    }

    fn package_source_edit_epub(package: &str) -> Epub<EpubZip<Cursor<Vec<u8>>>> {
        let mut data = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut data);
            let options = SimpleFileOptions::default();
            write_zip_entry(&mut zip, "mimetype", "application/epub+zip", options);
            write_zip_entry(
                &mut zip,
                "META-INF/container.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
                options,
            );
            write_zip_entry(&mut zip, "EPUB/package.opf", package, options);
            write_zip_entry(
                &mut zip,
                "EPUB/text/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Text</p></body></html>"#,
                options,
            );
            zip.finish().unwrap();
        }
        data.set_position(0);
        EpubZip::from_reader(data)
            .unwrap()
            .default_rendition()
            .unwrap()
    }
    fn write_zip_entry(
        zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
        path: &str,
        contents: &str,
        options: SimpleFileOptions,
    ) {
        zip.start_file(path, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }

    fn zip_entry(data: &[u8], path: &str) -> Option<Vec<u8>> {
        let mut archive = ZipArchive::new(Cursor::new(data)).unwrap();
        let mut file = archive.by_name(path).ok()?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        Some(contents)
    }
    fn export_after_unrelated_spine_edit(package: &str) -> String {
        let mut epub = package_source_edit_epub(package);
        epub.edit()
            .replace_spine_itemref(
                SpineItemRefSelector::Ordinal(crate::resource::ReadingOrderOrdinal::from_index(0)),
                ItemRef::new("chap").unwrap().with_linear(Linear::No),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let exported = epub.export(Cursor::new(Vec::new())).unwrap().into_inner();
        String::from_utf8(zip_entry(&exported, "EPUB/package.opf").unwrap()).unwrap()
    }

    #[test]
    fn edit_add_manifest_item_rewrites_opf_and_commits_resource() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("extra").unwrap())
            .href(EpubHref::try_new("extra.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        let report = epub
            .edit()
            .add_manifest_item(item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/extra.xhtml").unwrap(),
                b"<html><body>Extra</body></html>".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(report.iter().any(|change| matches!(
            change,
            EditChange::RewriteStructuralResource { path, .. }
                if path.as_str() == "EPUB/package.opf"
        )));
        assert!(epub.package().manifest_item_by_id("extra").is_some());
        assert_eq!(
            epub.bytes(&EpubPath::new("EPUB/extra.xhtml").unwrap())
                .unwrap(),
            b"<html><body>Extra</body></html>".to_vec()
        );
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
                .unwrap()
                .contains("media-type=\"application/xhtml+xml\"")
        );
    }

    #[test]
    fn raw_opf_overwrite_is_rejected_when_staged() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("extra").unwrap())
            .href(EpubHref::try_new("extra.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        let err = epub
            .edit()
            .add_manifest_item(item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/package.opf").unwrap(),
                b"not opf".to_vec(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Package,
                ..
            }
        ));
        assert!(epub.package().manifest_item_by_id("extra").is_none());
    }

    #[test]
    fn raw_opf_overwrite_after_semantic_edit_is_rejected() {
        let mut epub = memory_provider_epub();
        let package_bytes = epub.package().to_normalized_xml().unwrap().into_bytes();
        let first = ManifestItem::builder()
            .id(EpubString::try_new("extra-a").unwrap())
            .href(EpubHref::try_new("extra-a.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        let err = epub
            .edit()
            .add_manifest_item(first)
            .unwrap()
            .upsert_resource(EpubPath::new("EPUB/package.opf").unwrap(), package_bytes)
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Package,
                ..
            }
        ));
        assert!(epub.package().manifest_item_by_id("extra-a").is_none());
    }

    #[test]
    fn raw_opf_edit_is_rejected_when_staged() {
        let mut epub = memory_provider_epub();
        let package_bytes = epub.package().to_normalized_xml().unwrap().into_bytes();

        let err = epub
            .edit()
            .upsert_resource(EpubPath::new("EPUB/package.opf").unwrap(), package_bytes)
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Package,
                ..
            }
        ));
    }

    #[test]
    fn edit_add_manifest_item_rejects_nav_item() {
        let mut epub = memory_provider_epub();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("new-nav").unwrap())
            .href(EpubHref::try_new("new-nav.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .properties(vec![KnownManifestProperty::Nav.into()])
            .build()
            .unwrap();

        let err = epub.edit().add_manifest_item(item).unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::NavigationManifestItem { .. }
        ));
        assert!(epub.package().manifest_item_by_id("new-nav").is_none());
    }

    #[test]
    fn edit_remove_manifest_item_rewrites_opf_and_stages_provider_only_resource() {
        let provider = memory_provider_with_extra_manifest_item();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let preview = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::AuthoredHref(AuthoredHref::new(
                "images/cover.jpg",
            )))
            .unwrap()
            .preview()
            .unwrap();

        assert!(preview.changes().iter().any(|change| matches!(
            change,
            EditChange::RewriteStructuralResource { path, .. }
                if path.as_str() == "EPUB/package.opf"
        )));
        let cover = preview
            .resources()
            .resource_by_path(&EpubPath::new("EPUB/images/cover.jpg").unwrap())
            .unwrap();
        assert!(cover.declarations().len() == 0);

        preview.commit();

        assert!(epub.package().manifest_item_by_id("img").is_none());
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
                .unwrap()
                .contains("id=\"chap\"")
        );
        assert!(
            !epub
                .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
                .unwrap()
                .contains("id=\"img\"")
        );
    }

    #[test]
    fn edit_remove_manifest_item_rejects_spine_item() {
        let mut epub = memory_provider_epub();

        let err = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::Href(
                EpubHref::try_new("text/chapter.xhtml").unwrap(),
            ))
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Package {
                source: PackageError::ManifestItemInUse { id }
            } if id == "chap"
        ));
    }

    #[test]
    fn edit_remove_manifest_item_rejects_nav_item() {
        let mut epub = memory_provider_epub();

        let err = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::Id(
                EpubString::try_new("nav").unwrap(),
            ))
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::NavigationManifestItem { .. }
        ));
    }

    #[test]
    fn edit_remove_manifest_item_rejects_ambiguous_authored_href() {
        let provider = memory_provider_with_duplicate_manifest_hrefs();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::AuthoredHref(AuthoredHref::new(
                "images/cover.jpg",
            )))
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Selection {
                target: SelectionTarget::ManifestItem(ManifestItemSelector::AuthoredHref(_)),
                failure: SelectionFailure::Ambiguous,
            }
        ));
    }

    #[test]
    fn edit_replace_manifest_item_rewrites_opf_and_preserves_unknown_attrs() {
        let provider = memory_provider_with_extra_manifest_item();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let replacement = ManifestItem::builder()
            .id(EpubString::try_new("img2").unwrap())
            .href(EpubHref::try_new("images/new-cover.jpg").unwrap())
            .media_type(EpubString::try_new("image/jpeg").unwrap().into())
            .properties(vec![KnownManifestProperty::CoverImage.into()])
            .build()
            .unwrap();

        let preview = epub
            .edit()
            .replace_manifest_item(
                ManifestItemSelector::Id(EpubString::try_new("img").unwrap()),
                replacement,
            )
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/images/new-cover.jpg").unwrap(),
                b"newjpeg".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap();

        assert!(preview.changes().iter().any(|change| matches!(
            change,
            EditChange::RewriteStructuralResource { path, .. }
                if path.as_str() == "EPUB/package.opf"
        )));

        preview.commit();

        assert!(epub.package().manifest_item_by_id("img").is_none());
        assert!(epub.package().manifest_item_by_id("img2").is_some());
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("id=\"img2\""));
        assert!(package_xml.contains("href=\"images/new-cover.jpg\""));
        assert!(package_xml.contains("properties=\"cover-image\""));
        assert!(package_xml.contains("custom=\"keep\""));
    }

    #[test]
    fn edit_replace_manifest_item_by_href_preserves_duplicate_id_row_alignment() {
        let mut epub = duplicate_manifest_id_edit_epub();
        let replacement = ManifestItem::builder()
            .id(EpubString::try_new("duplicate").unwrap())
            .href(EpubHref::try_new("images/replacement.jpg").unwrap())
            .media_type(MediaType::try_from("image/jpeg").unwrap())
            .build()
            .unwrap();

        let preview = epub
            .edit()
            .replace_manifest_item(
                ManifestItemSelector::Href(EpubHref::try_new("images/second.jpg").unwrap()),
                replacement.clone(),
            )
            .unwrap()
            .preview()
            .unwrap();

        assert_eq!(
            preview.package().manifest().items()[1]
                .authored_href()
                .unwrap()
                .as_str(),
            "images/first.jpg"
        );
        assert_eq!(&preview.package().manifest().items()[2], &replacement);
        let preview_xml = preview
            .resource_changes
            .entry(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap()
            .unwrap();
        let preview_xml = std::str::from_utf8(preview_xml).unwrap();
        assert!(preview_xml.contains("href=\"images/first.jpg\""));
        assert!(preview_xml.contains("href=\"images/replacement.jpg\""));
        assert!(!preview_xml.contains("href=\"images/second.jpg\""));
        assert!(
            preview_xml.find("marker=\"first\"").unwrap()
                < preview_xml.find("href=\"images/replacement.jpg\"").unwrap()
        );

        preview.commit();
        assert_eq!(
            epub.package().manifest().items()[1]
                .authored_href()
                .unwrap()
                .as_str(),
            "images/first.jpg"
        );
        assert_eq!(&epub.package().manifest().items()[2], &replacement);
    }

    #[test]
    fn edit_remove_manifest_item_by_href_preserves_duplicate_id_row_alignment() {
        let mut epub = duplicate_manifest_id_edit_epub();

        let preview = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::Href(
                EpubHref::try_new("images/second.jpg").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap();

        let items = preview.package().manifest().items();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[1].authored_href().unwrap().as_str(),
            "images/first.jpg"
        );
        let preview_xml = preview
            .resource_changes
            .entry(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap()
            .unwrap();
        let preview_xml = std::str::from_utf8(preview_xml).unwrap();
        assert!(preview_xml.contains("marker=\"first\""));
        assert!(!preview_xml.contains("marker=\"second\""));

        preview.commit();
        assert_eq!(epub.package().manifest().items().len(), 2);
        assert_eq!(
            epub.package().manifest().items()[1]
                .authored_href()
                .unwrap()
                .as_str(),
            "images/first.jpg"
        );
    }

    #[test]
    fn edit_replace_manifest_item_rejects_duplicate_href() {
        let provider = memory_provider_with_extra_manifest_item();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let replacement = ManifestItem::builder()
            .id(EpubString::try_new("img2").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml").unwrap())
            .media_type(EpubString::try_new("image/jpeg").unwrap().into())
            .build()
            .unwrap();

        let err = epub
            .edit()
            .replace_manifest_item(
                ManifestItemSelector::Id(EpubString::try_new("img").unwrap()),
                replacement,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Package {
                source: PackageError::ManifestHrefDuplicate { href }
            } if href == "text/chapter.xhtml"
        ));
    }

    #[test]
    fn edit_replace_manifest_item_rejects_nav_item() {
        let mut epub = memory_provider_epub();
        let replacement = ManifestItem::builder()
            .id(EpubString::try_new("nav2").unwrap())
            .href(EpubHref::try_new("nav2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        let err = epub
            .edit()
            .replace_manifest_item(
                ManifestItemSelector::Id(EpubString::try_new("nav").unwrap()),
                replacement,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::NavigationManifestItem { .. }
        ));
    }

    #[test]
    fn edit_manifest_item_changes_reject_selected_ncx() {
        let mut epub = ncx_only_edit_epub();
        let error = epub
            .edit()
            .remove_manifest_item(ManifestItemSelector::Id(
                EpubString::try_new("ncx").unwrap(),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            crate::edit::EditError::NavigationManifestItem { .. }
        ));

        let replacement = ManifestItem::builder()
            .id(EpubString::try_new("ncx").unwrap())
            .href(EpubHref::try_new("replacement.ncx").unwrap())
            .media_type(
                EpubString::try_new("application/x-dtbncx+xml")
                    .unwrap()
                    .into(),
            )
            .build()
            .unwrap();
        let error = epub
            .edit()
            .replace_manifest_item(
                ManifestItemSelector::Id(EpubString::try_new("ncx").unwrap()),
                replacement,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            crate::edit::EditError::NavigationManifestItem { .. }
        ));
    }

    #[test]
    fn edit_add_metadata_element_appends_dc_element() {
        let mut epub = memory_provider_epub();
        let title = Element::builder()
            .content(EpubString::try_new("Alternate Title").unwrap())
            .xml_lang(EpubString::try_new("en").unwrap())
            .build();

        epub.edit()
            .add_metadata_element(DcElement::Title, title)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(
            epub.package().metadata().elements(DcElement::Title).len(),
            2
        );
        assert_eq!(
            epub.package().metadata().elements(DcElement::Title)[1]
                .content()
                .map(EpubString::as_str),
            Some("Alternate Title")
        );
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("Alternate Title"));
    }

    #[test]
    fn edit_add_meta_appends_epub3_meta() {
        let mut epub = memory_provider_epub();
        let meta = Meta::new(
            MetaPropertyToken::try_new("dcterms:modified").unwrap(),
            EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
        );

        epub.edit()
            .add_meta(meta)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(epub.package().metadata().meta().iter().any(|meta| {
            meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
                && meta.content().map(EpubString::as_str) == Some("2026-06-28T00:00:00Z")
        }));
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("property=\"dcterms:modified\""));
        assert!(package_xml.contains("2026-06-28T00:00:00Z"));
    }

    #[test]
    fn edit_add_metadata_link_appends_link_and_preserves_media_type_attr() {
        let mut epub = memory_provider_epub();
        let link = MetadataLink::builder()
            .href(EpubHref::try_new("records/onix.xml").unwrap())
            .rel(crate::package::metadata::KnownLinkRel::Record.into())
            .media_type(EpubString::try_new("application/xml").unwrap())
            .id(EpubString::try_new("onix").unwrap())
            .build();

        epub.edit()
            .upsert_resource(
                EpubPath::new("EPUB/records/onix.xml").unwrap(),
                b"<record/>".to_vec(),
            )
            .unwrap()
            .add_metadata_link(link)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().metadata().link().len(), 1);
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("href=\"records/onix.xml\""));
        assert!(package_xml.contains("rel=\"record\""));
        assert!(package_xml.contains("media-type=\"application/xml\""));
        assert!(!package_xml.contains("media_type=\"application/xml\""));
    }

    #[test]
    fn edit_add_metadata_preserves_unknown_metadata_child() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:custom="https://example.com/custom" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><custom:thing custom:attr="keep">Unknown</custom:thing><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("custom:thing"));
        assert!(package_xml.contains("custom:attr=\"keep\""));
        assert!(package_xml.contains("Unknown"));
    }

    #[test]
    fn edit_add_metadata_handles_prefixed_opf_package() {
        let provider = memory_provider_with_prefixed_opf_package();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let title = Element::builder()
            .content(EpubString::try_new("Titre secondaire").unwrap())
            .xml_lang(EpubString::try_new("fr").unwrap())
            .build();
        let link = MetadataLink::builder()
            .href(EpubHref::try_new("records/onix.xml").unwrap())
            .rel(crate::package::metadata::KnownLinkRel::Record.into())
            .media_type(EpubString::try_new("application/xml").unwrap())
            .build();

        epub.edit()
            .add_metadata_element(DcElement::Title, title)
            .unwrap()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
            ))
            .unwrap()
            .add_metadata_link(link)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(
            epub.package().metadata().elements(DcElement::Title).len(),
            2
        );
        assert!(epub.package().metadata().meta().iter().any(|meta| {
            meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
                && meta.content().map(EpubString::as_str) == Some("2026-06-28T00:00:00Z")
        }));
        assert_eq!(epub.package().metadata().link().len(), 1);
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("opf:package"));
        assert!(package_xml.contains("Titre secondaire"));
        assert!(package_xml.contains("property=\"dcterms:modified\""));
        assert!(package_xml.contains("href=\"records/onix.xml\""));
        assert!(package_xml.contains("media-type=\"application/xml\""));
    }

    #[test]
    fn edit_replace_metadata_element_preserves_unknown_attrs() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:custom="https://example.com/custom" version="3.0" unique-identifier="uid">
  <metadata><dc:title id="title" custom:attr="keep">T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let replacement = Element::builder()
            .content(EpubString::try_new("Replacement").unwrap())
            .id(EpubString::try_new("title2").unwrap())
            .build();

        epub.edit()
            .replace_metadata_element(
                MetadataElementSelector {
                    element: DcElement::Title,
                    node: MetadataNodeSelector::Id(EpubString::try_new("title").unwrap()),
                },
                replacement,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(
            epub.package().metadata().elements(DcElement::Title)[0]
                .content()
                .map(EpubString::as_str),
            Some("Replacement")
        );
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("id=\"title2\""));
        assert!(package_xml.contains("custom:attr=\"keep\""));
        assert!(package_xml.contains("Replacement"));
        assert!(!package_xml.contains(">T<"));
    }

    #[test]
    fn edit_remove_metadata_is_not_blocked_by_requirement_policy() {
        let mut epub = memory_provider_epub();
        let preview = epub
            .edit()
            .remove_metadata_element(MetadataElementSelector {
                element: DcElement::Title,
                node: MetadataNodeSelector::Index(0),
            })
            .unwrap()
            .preview()
            .unwrap();

        preview.commit();
        assert!(
            epub.package()
                .metadata()
                .elements(DcElement::Title)
                .is_empty()
        );
    }

    #[test]
    fn edit_replace_meta_by_property_and_rejects_ambiguous_property() {
        let mut epub = memory_provider_epub();

        let err = epub
            .edit()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("belongs-to-collection").unwrap(),
                EpubString::try_new("A").unwrap(),
            ))
            .unwrap()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("belongs-to-collection").unwrap(),
                EpubString::try_new("B").unwrap(),
            ))
            .unwrap()
            .replace_meta(
                MetaSelector::Property(EpubString::try_new("belongs-to-collection").unwrap()),
                Meta::new(
                    MetaPropertyToken::try_new("belongs-to-collection").unwrap(),
                    EpubString::try_new("C").unwrap(),
                ),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Selection {
                target: SelectionTarget::Meta(MetaSelector::Property(_)),
                failure: SelectionFailure::Ambiguous,
            }
        ));

        epub.edit()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
            ))
            .unwrap()
            .replace_meta(
                MetaSelector::Property(EpubString::try_new("dcterms:modified").unwrap()),
                Meta::new(
                    MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                    EpubString::try_new("2026-06-29T00:00:00Z").unwrap(),
                ),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(epub.package().metadata().meta().iter().any(|meta| {
            meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
                && meta.content().map(EpubString::as_str) == Some("2026-06-29T00:00:00Z")
        }));
    }

    #[test]
    fn edit_remove_metadata_link_by_authored_href() {
        let mut epub = memory_provider_epub();
        let link = MetadataLink::builder()
            .href(EpubHref::try_new("https://example.com/onix.xml").unwrap())
            .rel(crate::package::metadata::KnownLinkRel::Record.into())
            .media_type(EpubString::try_new("application/xml").unwrap())
            .build();

        epub.edit()
            .add_metadata_link(link)
            .unwrap()
            .remove_metadata_link(MetadataLinkSelector::AuthoredHref(AuthoredHref::new(
                "https://example.com/onix.xml",
            )))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(epub.package().metadata().link().is_empty());
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(!package_xml.contains("https://example.com/onix.xml"));
    }

    #[test]
    fn edit_remove_meta_and_replace_metadata_link() {
        let mut epub = memory_provider_epub();
        let first_link = MetadataLink::builder()
            .href(EpubHref::try_new("https://example.com/old.xml").unwrap())
            .rel(crate::package::metadata::KnownLinkRel::Record.into())
            .media_type(EpubString::try_new("application/xml").unwrap())
            .id(EpubString::try_new("record").unwrap())
            .build();
        let replacement_link = MetadataLink::builder()
            .href(EpubHref::try_new("https://example.com/new.xml").unwrap())
            .rel(crate::package::metadata::KnownLinkRel::Alternate.into())
            .media_type(EpubString::try_new("application/xml").unwrap())
            .id(EpubString::try_new("record2").unwrap())
            .build();

        epub.edit()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
            ))
            .unwrap()
            .add_metadata_link(first_link)
            .unwrap()
            .remove_meta(MetaSelector::Property(
                EpubString::try_new("dcterms:modified").unwrap(),
            ))
            .unwrap()
            .replace_metadata_link(
                MetadataLinkSelector::Id(EpubString::try_new("record").unwrap()),
                replacement_link,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(epub.package().metadata().meta().is_empty());
        assert_eq!(epub.package().metadata().link().len(), 1);
        assert_eq!(
            epub.package().metadata().link()[0]
                .id()
                .map(EpubString::as_str),
            Some("record2")
        );
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(!package_xml.contains("2026-06-28T00:00:00Z"));
        assert!(!package_xml.contains("https://example.com/old.xml"));
        assert!(package_xml.contains("https://example.com/new.xml"));
        assert!(package_xml.contains("rel=\"alternate\""));
    }

    #[test]
    fn edit_replace_meta_skips_legacy_opf2_meta_nodes() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><meta name="cover" content="cover-image" /><meta property="dcterms:modified">2026-06-28T00:00:00Z</meta></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .replace_meta(
                MetaSelector::Index(0),
                Meta::new(
                    MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                    EpubString::try_new("2026-06-29T00:00:00Z").unwrap(),
                ),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().metadata().opf2meta().len(), 1);
        assert!(epub.package().metadata().meta().iter().any(|meta| {
            meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
                && meta.content().map(EpubString::as_str) == Some("2026-06-29T00:00:00Z")
        }));
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("name=\"cover\""));
        assert!(package_xml.contains("content=\"cover-image\""));
        assert!(package_xml.contains("2026-06-29T00:00:00Z"));
        assert!(!package_xml.contains("2026-06-28T00:00:00Z"));
    }

    #[test]
    fn edit_replace_metadata_handles_prefixed_opf_package() {
        let provider = memory_provider_with_prefixed_opf_package();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let replacement = Element::builder()
            .content(EpubString::try_new("Titre remplace").unwrap())
            .build();

        epub.edit()
            .replace_metadata_element(
                MetadataElementSelector {
                    element: DcElement::Title,
                    node: MetadataNodeSelector::Index(0),
                },
                replacement,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(
            epub.package().metadata().elements(DcElement::Title)[0]
                .content()
                .map(EpubString::as_str),
            Some("Titre remplace")
        );
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("opf:package"));
        assert!(package_xml.contains("Titre remplace"));
    }

    #[test]
    fn edit_manifest_and_spine_handle_prefixed_opf_package() {
        let provider = memory_provider_with_prefixed_opf_package();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let item = ManifestItem::builder()
            .id(EpubString::try_new("chap2").unwrap())
            .href(EpubHref::try_new("text/chapter2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        epub.edit()
            .add_manifest_item(item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/text/chapter2.xhtml").unwrap(),
                b"<html><body>Chapter 2</body></html>".to_vec(),
            )
            .unwrap()
            .add_spine_itemref(ItemRef::new("chap2").unwrap())
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert!(epub.package().manifest_item_by_id("chap2").is_some());
        assert_eq!(epub.package().spine().itemrefs().len(), 2);
        assert_eq!(epub.package().spine().itemrefs()[1].idref(), Some("chap2"));
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("opf:package"));
        assert!(package_xml.contains("id=\"chap2\""));
        assert!(package_xml.contains("idref=\"chap2\""));
    }

    #[test]
    fn edit_add_metadata_rejects_wrong_metadata_namespace_with_specific_error() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata xmlns=""><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .add_meta(Meta::new(
                MetaPropertyToken::try_new("dcterms:modified").unwrap(),
                EpubString::try_new("2026-06-28T00:00:00Z").unwrap(),
            ))
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::UnsupportedPackageNamespace {
                element,
                namespace,
                expected,
                ..
            } if element == "metadata" && namespace.is_empty() && expected == OPF_NS
        ));
    }

    #[test]
    fn edit_add_spine_itemref_rewrites_opf_and_updates_reading_order() {
        let mut epub = memory_provider_epub();
        let manifest_item = ManifestItem::builder()
            .id(EpubString::try_new("chap2").unwrap())
            .href(EpubHref::try_new("text/chapter2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();
        let itemref = ItemRef::new("chap2").unwrap();

        epub.edit()
            .add_manifest_item(manifest_item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/text/chapter2.xhtml").unwrap(),
                b"<html><body>Chapter 2</body></html>".to_vec(),
            )
            .unwrap()
            .add_spine_itemref(itemref)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().spine().itemrefs().len(), 2);
        assert_eq!(epub.resources().reading_order().count(), 2);
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
                .unwrap()
                .contains("idref=\"chap2\"")
        );
    }

    #[test]
    fn edit_add_spine_itemref_omits_default_linear() {
        let mut epub = memory_provider_epub();
        let manifest_item = ManifestItem::builder()
            .id(EpubString::try_new("chap2").unwrap())
            .href(EpubHref::try_new("text/chapter2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        epub.edit()
            .add_manifest_item(manifest_item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/text/chapter2.xhtml").unwrap(),
                b"<html><body>Chapter 2</body></html>".to_vec(),
            )
            .unwrap()
            .add_spine_itemref(ItemRef::new("chap2").unwrap())
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("idref=\"chap2\""));
        assert!(!package_xml.contains("linear=\"yes\""));
    }

    #[test]
    fn edit_add_spine_itemref_keeps_non_default_linear() {
        let mut epub = memory_provider_epub();
        let manifest_item = ManifestItem::builder()
            .id(EpubString::try_new("chap2").unwrap())
            .href(EpubHref::try_new("text/chapter2.xhtml").unwrap())
            .media_type(EpubString::try_new("application/xhtml+xml").unwrap().into())
            .build()
            .unwrap();

        epub.edit()
            .add_manifest_item(manifest_item)
            .unwrap()
            .upsert_resource(
                EpubPath::new("EPUB/text/chapter2.xhtml").unwrap(),
                b"<html><body>Chapter 2</body></html>".to_vec(),
            )
            .unwrap()
            .add_spine_itemref(ItemRef::new("chap2").unwrap().with_linear(Linear::No))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("idref=\"chap2\""));
        assert!(package_xml.contains("linear=\"no\""));
    }

    #[test]
    fn edit_add_spine_itemref_rejects_missing_manifest_id() {
        let mut epub = memory_provider_epub();

        let err = epub
            .edit()
            .add_spine_itemref(ItemRef::new("missing").unwrap())
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Package {
                source: PackageError::ManifestItemMissing { id }
            } if id == "missing"
        ));
    }

    #[test]
    fn edit_remove_spine_itemref_by_index_rewrites_opf() {
        let provider = memory_provider_with_two_spine_items();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .remove_spine_itemref(SpineItemRefSelector::Ordinal(
                crate::resource::ReadingOrderOrdinal::from_index(1),
            ))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().spine().itemrefs().len(), 1);
        assert_eq!(epub.package().spine().itemrefs()[0].idref(), Some("chap"));
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("idref=\"chap\""));
        assert!(!package_xml.contains("idref=\"chap2\""));
    }

    #[test]
    fn edit_remove_last_spine_itemref_is_not_blocked_by_requirement_policy() {
        let mut epub = memory_provider_epub();

        let preview = epub
            .edit()
            .remove_spine_itemref(SpineItemRefSelector::Ordinal(
                crate::resource::ReadingOrderOrdinal::from_index(0),
            ))
            .unwrap()
            .preview()
            .unwrap();

        preview.commit();
        assert!(epub.package().spine().itemrefs().is_empty());
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(!package_xml.contains("<itemref"));
    }

    #[test]
    fn edit_remove_spine_itemref_rejects_ambiguous_idref() {
        let provider = memory_provider_with_duplicate_spine_itemrefs();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .remove_spine_itemref(SpineItemRefSelector::Idref(
                EpubString::try_new("chap").unwrap(),
            ))
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Selection {
                target: SelectionTarget::SpineItemRef(SpineItemRefSelector::Idref(_)),
                failure: SelectionFailure::Ambiguous,
            }
        ));
    }

    #[test]
    fn edit_replace_spine_itemref_rewrites_opf() {
        let provider = memory_provider_with_two_spine_items();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let replacement = ItemRef::new("chap").unwrap().with_id("again").unwrap();

        epub.edit()
            .replace_spine_itemref(
                SpineItemRefSelector::Ordinal(crate::resource::ReadingOrderOrdinal::from_index(1)),
                replacement,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().spine().itemrefs().len(), 2);
        assert_eq!(epub.package().spine().itemrefs()[1].idref(), Some("chap"));
        assert_eq!(
            epub.package().spine().itemrefs()[1]
                .id()
                .map(EpubString::as_str),
            Some("again")
        );
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("id=\"again\""));
        assert!(!package_xml.contains("idref=\"chap2\""));
    }

    #[test]
    fn edit_replace_spine_itemref_rejects_missing_manifest_id() {
        let provider = memory_provider_with_two_spine_items();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .replace_spine_itemref(
                SpineItemRefSelector::Ordinal(crate::resource::ReadingOrderOrdinal::from_index(1)),
                ItemRef::new("missing").unwrap(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Package {
                source: PackageError::ManifestItemMissing { id }
            } if id == "missing"
        ));
    }

    #[test]
    fn edit_replace_spine_itemref_preserves_unknown_attrs() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="chap2" href="text/chapter2.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /><itemref idref="chap2" custom="keep" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            (
                "EPUB/text/chapter2.xhtml",
                b"<html><body>Chapter 2</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .replace_spine_itemref(
                SpineItemRefSelector::Ordinal(crate::resource::ReadingOrderOrdinal::from_index(1)),
                ItemRef::new("chap").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("idref=\"chap\""));
        assert!(package_xml.contains("custom=\"keep\""));
    }

    #[test]
    fn edit_move_spine_itemref_reorders_reading_order() {
        let provider = memory_provider_with_two_spine_items();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .move_spine_itemref(
                SpineItemRefSelector::Idref(EpubString::try_new("chap").unwrap()),
                1,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().spine().itemrefs()[0].idref(), Some("chap2"));
        assert_eq!(epub.package().spine().itemrefs()[1].idref(), Some("chap"));
        let order = epub
            .resources()
            .reading_order()
            .map(|entry| {
                let declaration = entry
                    .declaration()
                    .expect("reading-order entry should resolve to a declaration");
                match declaration.id() {
                    crate::resource::ManifestIdValue::Valid(id) => id.to_string(),
                    _ => panic!("reading-order declaration should have a valid id"),
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["chap2".to_string(), "chap".to_string()]);
    }

    #[test]
    fn edit_move_spine_itemref_rejects_out_of_range_target() {
        let provider = memory_provider_with_two_spine_items();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .move_spine_itemref(
                SpineItemRefSelector::Ordinal(crate::resource::ReadingOrderOrdinal::from_index(0)),
                2,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            EditError::Package {
                source: PackageError::SpineItemrefIndexMissing { index }
            } if index == 2
        ));
    }

    #[test]
    fn opening_rejects_missing_package_namespace() {
        let package = r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="chap2" href="text/chapter2.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            (
                "EPUB/text/chapter2.xhtml",
                b"<html><body>Chapter 2</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let error =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap_err();

        assert!(matches!(
            error.failure(),
            crate::EpubOpenFailure::PackageParse {
                source: PackageError::RootInvalid,
                ..
            }
        ));
    }
    #[test]
    fn edit_spine_itemref_rejects_wrong_spine_namespace_with_specific_error() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="chap2" href="text/chapter2.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine xmlns=""><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            (
                "EPUB/text/chapter2.xhtml",
                b"<html><body>Chapter 2</body></html>".to_vec(),
            ),
        ])
        .unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let err = epub
            .edit()
            .add_spine_itemref(ItemRef::new("chap2").unwrap())
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::UnsupportedPackageNamespace {
                element,
                namespace,
                expected,
                ..
            } if element == "spine" && namespace.is_empty() && expected == OPF_NS
        ));
    }

    #[test]
    fn edit_remove_manifest_resource_preview_models_missing_provider_entry() {
        let mut epub = memory_provider_epub();
        let preview = epub
            .edit()
            .remove_resource(EpubPath::new("EPUB/text/chapter.xhtml").unwrap())
            .unwrap()
            .preview()
            .unwrap();

        let chapter = preview
            .resources()
            .declaration_by_id("chap")
            .unwrap()
            .resource()
            .unwrap();
        assert_eq!(
            chapter.presence(),
            crate::resource::ProviderPresence::Missing
        );
        preview.commit();
    }

    #[test]
    fn raw_structural_resource_edits_are_rejected_when_staged() {
        let mut epub = memory_provider_epub();
        let err = epub
            .edit()
            .upsert_resource(
                EpubPath::new("EPUB/package.opf").unwrap(),
                b"not opf".to_vec(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Package,
                ..
            }
        ));

        let err = epub
            .edit()
            .remove_resource(EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap_err();

        assert!(matches!(
            err,
            crate::edit::EditError::StructuralResourceEdit {
                kind: StructuralResourceKind::Package,
                ..
            }
        ));
    }
    #[test]
    fn unrelated_xot_edit_export_preserves_malformed_optional_package_metadata() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><link href="records/onix.xml" data-keep="missing-rel"/><meta property="missing-content" data-keep="empty-meta"/></metadata>
  <manifest><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chap"/></spine>
</package>"#;

        let exported = export_after_unrelated_spine_edit(package);

        assert!(exported.contains("<link href=\"records/onix.xml\" data-keep=\"missing-rel\""));
        assert!(exported.contains("<meta property=\"missing-content\" data-keep=\"empty-meta\""));
        assert!(exported.contains("<itemref idref=\"chap\" linear=\"no\""));
    }

    #[test]
    fn unrelated_xot_edit_export_preserves_bindings_subtree() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:vendor="https://example.com/vendor" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chap"/></spine>
  <bindings data-keep="bindings"><mediaType media-type="application/x-demo" handler="chap"><vendor:config enabled="yes"/></mediaType></bindings>
</package>"#;

        let exported = export_after_unrelated_spine_edit(package);

        assert!(exported.contains("<bindings data-keep=\"bindings\">"));
        assert!(
            exported.contains("<mediaType media-type=\"application/x-demo\" handler=\"chap\">")
        );
        assert!(exported.contains("<vendor:config enabled=\"yes\""));
    }

    #[test]
    fn unrelated_xot_edit_export_preserves_repeated_package_block_structure() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata data-block="metadata-one"><dc:title>First</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest data-block="manifest-one"><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <guide data-block="guide-one"><reference title="First" href="text/chapter.xhtml"/></guide>
  <metadata data-block="metadata-two"><dc:title>Second</dc:title></metadata>
  <manifest data-block="manifest-two"><item id="image" href="cover.jpg" media-type="image/jpeg"/></manifest>
  <guide data-block="guide-two"><reference title="Second" href="cover.jpg"/></guide>
  <spine><itemref idref="chap"/></spine>
</package>"#;

        let exported = export_after_unrelated_spine_edit(package);

        assert_eq!(exported.matches("<metadata").count(), 2);
        assert_eq!(exported.matches("<manifest").count(), 2);
        assert_eq!(exported.matches("<guide").count(), 2);
        for marker in [
            "metadata-one",
            "manifest-one",
            "guide-one",
            "metadata-two",
            "manifest-two",
            "guide-two",
        ] {
            assert!(exported.contains(&format!("data-block=\"{marker}\"")));
        }
        assert!(exported.find("metadata-one") < exported.find("metadata-two"));
        assert!(exported.find("manifest-one") < exported.find("manifest-two"));
        assert!(exported.find("guide-one") < exported.find("guide-two"));
    }
}
