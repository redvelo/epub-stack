use super::*;

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    /// Validates staged edits and returns a snapshot for inspection or commit.
    ///
    /// The live publication remains unchanged. The preview includes the effective resource
    /// inventory and loads embedded annotations when this transaction touched them.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] when staged resources or structural models cannot form a consistent
    /// publication snapshot.
    pub fn preview(self) -> Result<EpubEditPreview<'a, R>> {
        self.validate_structural_coherence()?;
        let combined_changes = merged_resource_changes(&self.epub.resource_changes, &self.changes)?;
        let provider_index = combined_changes
            .apply_to_index(
                &self.epub.provider_index,
                self.epub.open_limits.provider_index_limits(),
            )
            .map_err(EditError::from)?;
        let staged_package = self
            .package_override
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.epub.package.clone());
        let staged_navigation = self
            .navigation_override
            .clone()
            .unwrap_or_else(|| self.epub.navigation.clone());
        let resources =
            ResourceIndex::new(&staged_package, &self.epub.package_path, &provider_index)?;
        let annotations_path = annotation_epub_path("META-INF/annotations.json")?;
        let annotations_json_upserted = matches!(
            self.changes.entry(annotations_path.as_path()),
            Some(Some(_))
        );
        let annotations = if annotations_json_upserted {
            Some(
                self.epub
                    .embedded_annotations_with_changes(&combined_changes, &provider_index)
                    .map_err(|source| EditError::EmbeddedAnnotations { source })?
                    .ok_or_else(|| EditError::UnsupportedSemanticEdit {
                        message: "staged embedded annotations are missing".to_string(),
                    })?,
            )
        } else if self.annotations_touched {
            self.epub
                .embedded_annotations_with_changes(&combined_changes, &provider_index)
                .map_err(|source| EditError::EmbeddedAnnotations { source })?
        } else {
            None
        };
        let changes = coalesced_edit_changes(self.edit_changes);
        Ok(EpubEditPreview {
            epub: self.epub,
            resource_changes: combined_changes,
            changes,
            package: staged_package,
            navigation: staged_navigation,
            annotations,
            resources,
        })
    }

    pub(super) fn selected_local_path(&self, selector: ResourceSelector) -> Result<EpubPath> {
        let value = format!("{selector:?}");
        let record = self.epub.resources.select(&selector)?;
        record
            .local_path()
            .cloned()
            .ok_or(EditError::NonLocalResource { selector: value })
    }

    pub(super) fn stage_package_edit(
        &mut self,
        mutate: impl FnOnce(&mut Xot, Node) -> Result<()>,
        verify: impl FnOnce(&Package) -> Result<()>,
    ) -> Result<usize> {
        self.reject_dirty_package_base_for_semantic_edit()?;
        let package_path = self.epub.package_path.clone();
        let combined_changes = merged_resource_changes(&self.epub.resource_changes, &self.changes)?;
        let package_bytes = resource_bytes_from_parts(
            &self.epub.container,
            &combined_changes,
            &ResourceAddress::Local(package_path.clone()),
        )?;
        let package_xml = decode_structural_xml(&package_bytes, &package_path)?;

        let mut xot = Xot::new();
        let doc = xot
            .parse(package_xml.as_ref())
            .map_err(|source| structural_xml_operation(package_path.clone(), source))?;
        mutate(&mut xot, doc)?;
        let package_xml = xot
            .to_string(doc)
            .map_err(|source| structural_xml_operation(package_path.clone(), source))?;

        let parsed_package = Package::parse(&package_xml)?;
        verify(&parsed_package)?;

        let package_bytes = package_xml.into_bytes();
        let size_bytes = package_bytes.len();
        self.changes.upsert(package_path.clone(), package_bytes);
        self.structural_edits
            .insert(package_path, StructuralEdit::Upsert);
        self.package_override = Some(parsed_package);
        Ok(size_bytes)
    }

    pub(super) fn stage_navigation_edit(
        &mut self,
        mutate: impl FnOnce(&mut Xot, Node) -> Result<()>,
        verify: impl FnOnce(&Navigation) -> Result<()>,
    ) -> Result<usize> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let nav_document =
            staged_navigation
                .epub_nav()
                .ok_or_else(|| EditError::UnsupportedSemanticEdit {
                    message: "NAV semantic edits require an EPUB navigation document".to_string(),
                })?;
        let nav_path = nav_document.path().clone();
        self.reject_dirty_structural_base_for_semantic_edit(&nav_path)?;
        let combined_changes = merged_resource_changes(&self.epub.resource_changes, &self.changes)?;
        let nav_bytes = resource_bytes_from_parts(
            &self.epub.container,
            &combined_changes,
            &ResourceAddress::Local(nav_path.clone()),
        )?;
        let nav_xml = decode_structural_xml(&nav_bytes, &nav_path)?;

        let mut xot = Xot::new();
        let doc = xot
            .parse(nav_xml.as_ref())
            .map_err(|source| structural_xml_operation(nav_path.clone(), source))?;
        mutate(&mut xot, doc)?;
        let nav_xml = xot
            .to_string(doc)
            .map_err(|source| structural_xml_operation(nav_path.clone(), source))?;

        let parsed_nav = parse::epub_nav(nav_path.clone(), &nav_xml)?;
        let mut parsed_navigation = staged_navigation.clone();
        parsed_navigation.replace_epub_nav(parsed_nav);
        verify(&parsed_navigation)?;

        let nav_bytes = nav_xml.into_bytes();
        let size_bytes = nav_bytes.len();
        self.changes.upsert(nav_path.clone(), nav_bytes);
        self.structural_edits
            .insert(nav_path, StructuralEdit::Upsert);
        self.navigation_override = Some(parsed_navigation);
        Ok(size_bytes)
    }

    pub(super) fn stage_generated_navigation(
        &mut self,
        nav_path: EpubPath,
        source_path: EpubPath,
        lists: Vec<NavigationList>,
        title: &EpubString,
    ) -> Result<usize> {
        self.reject_dirty_structural_base_for_semantic_edit(&nav_path)?;
        let source_document = NavigationDocument::builder()
            .path(source_path)
            .lists(lists)
            .build()?;
        let nav_xml = source_document.generate_epub_nav_xhtml(&nav_path, title)?;
        let parsed_nav = parse::epub_nav(nav_path.clone(), &nav_xml)?;
        let mut parsed_navigation = self
            .navigation_override
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.epub.navigation.clone());
        parsed_navigation.remove_source(NavigationSource::Ncx);
        parsed_navigation.replace_epub_nav(parsed_nav);

        let nav_bytes = nav_xml.into_bytes();
        let size_bytes = nav_bytes.len();
        self.changes.upsert(nav_path.clone(), nav_bytes);
        self.structural_edits
            .insert(nav_path, StructuralEdit::Upsert);
        self.navigation_override = Some(parsed_navigation);
        Ok(size_bytes)
    }

    pub(super) fn stage_semantic_resource_removal(&mut self, path: EpubPath) -> Result<()> {
        self.reject_dirty_structural_base_for_semantic_edit(&path)?;
        self.changes.remove(path.clone());
        self.structural_edits
            .insert(path.clone(), StructuralEdit::Remove);
        self.edit_changes.push(EditChange::RemoveResource { path });
        Ok(())
    }

    pub(super) fn unique_manifest_id_for_edit(&self, base: &str) -> String {
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        let occupied = |candidate: &str| {
            staged_package.manifest().items().iter().any(|item| {
                item.id()
                    .is_some_and(|id| manifest_ids_equal(id, candidate))
            })
        };
        if !occupied(base) {
            return base.to_string();
        }
        (1usize..)
            .map(|idx| format!("{base}-{idx}"))
            .find(|id| !occupied(id))
            .expect("unbounded id generator")
    }

    pub(super) fn unique_generated_nav_href_for_edit(&self) -> String {
        (0usize..)
            .map(|idx| {
                if idx == 0 {
                    "nav.xhtml".to_string()
                } else {
                    format!("nav-{idx}.xhtml")
                }
            })
            .find(|href| !self.nav_href_in_use_for_edit(href))
            .expect("unbounded href generator")
    }

    pub(super) fn nav_href_in_use_for_edit(&self, href: &str) -> bool {
        let Some((path, _)) = resolve_local_href_from_source(
            &AuthoredHref::new(href.to_string()),
            &self.epub.package_path,
        ) else {
            return true;
        };
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        if staged_package.manifest().items().iter().any(|item| {
            manifest_item_local_resource_path(item, &self.epub.package_path)
                .is_some_and(|item_path| item_path == path)
        }) {
            return true;
        }
        if let Some(change) = self.changes.entry(&path) {
            return change.is_some();
        }
        if let Some(change) = self.epub.resource_changes.entry(&path) {
            return change.is_some();
        }
        self.epub.provider_index.get(&path).is_some()
    }

    pub(super) fn reject_dirty_package_base_for_semantic_edit(&self) -> Result<()> {
        self.reject_dirty_structural_base_for_semantic_edit(&self.epub.package_path)
    }

    pub(super) fn reject_dirty_structural_base_for_semantic_edit(
        &self,
        path: &EpubPath,
    ) -> Result<()> {
        let Some(staged_bytes) = self.changes.entry(path.as_path()) else {
            return Ok(());
        };
        let coherent = matches!(
            (self.structural_edits.get(path), staged_bytes),
            (Some(StructuralEdit::Upsert), Some(_)) | (Some(StructuralEdit::Remove), None)
        );
        if coherent {
            return Ok(());
        }
        Err(self.structural_resource_edit_error(path))
    }

    pub(super) fn mark_raw_structural_overwrite(&mut self, path: &EpubPath) {
        if self.structural_edits.contains_key(path) {
            self.structural_edits
                .insert(path.clone(), StructuralEdit::Overwritten);
        }
    }

    fn validate_structural_coherence(&self) -> Result<()> {
        if self.structural_edits.is_empty() {
            return self
                .epub
                .reject_structural_resource_edits(&self.changes, &BTreeMap::new());
        }
        for path in self.changes.entries().keys() {
            let structural = self.epub.structural_resource_kind(path).is_some()
                || self.structural_edits.contains_key(path);
            if !structural {
                continue;
            }
            let coherent = matches!(
                (
                    self.structural_edits.get(path),
                    self.changes.entry(path.as_path())
                ),
                (Some(StructuralEdit::Upsert), Some(Some(_)))
                    | (Some(StructuralEdit::Remove), Some(None))
            );
            if !coherent {
                return Err(self.structural_resource_edit_error(path));
            }
        }
        Ok(())
    }

    fn structural_resource_edit_error(&self, path: &EpubPath) -> EditError {
        EditError::StructuralResourceEdit {
            path: path.clone(),
            kind: self
                .epub
                .structural_resource_kind(path)
                .unwrap_or(StructuralResourceKind::Navigation),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::provider::{
        MemoryResourceProvider, ResourceProviderIndex, ResourceProviderIndexError,
        ResourceProviderIndexLimits,
    };
    use std::cell::Cell;
    use std::io::Read;

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

    #[derive(Debug)]
    struct FailingIndexProvider {
        inner: MemoryResourceProvider,
        index_calls: Cell<usize>,
        read_calls: Cell<usize>,
        entry_reader_calls: Cell<usize>,
        annotation_reads: Cell<usize>,
        fail_on_index_call: usize,
    }

    impl ResourceProvider for FailingIndexProvider {
        fn read(&self, path: &EpubPath) -> crate::resource::provider::ReadResult<Vec<u8>> {
            self.read_calls.set(self.read_calls.get() + 1);
            if path.as_str() == "META-INF/annotations.json" {
                self.annotation_reads.set(self.annotation_reads.get() + 1);
            }
            self.inner.read(path)
        }

        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> crate::resource::provider::ReadResult<T> {
            self.entry_reader_calls
                .set(self.entry_reader_calls.get() + 1);
            if path.as_str() == "META-INF/annotations.json" {
                self.annotation_reads.set(self.annotation_reads.get() + 1);
            }
            self.inner.read_with(path, read)
        }

        fn index(
            &self,
            limits: &ResourceProviderIndexLimits,
        ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
            let calls = self.index_calls.get() + 1;
            self.index_calls.set(calls);
            if calls == self.fail_on_index_call {
                return Err(ResourceProviderIndexError::enumeration(
                    std::io::Error::other("index failed"),
                ));
            }
            self.inner.index(limits)
        }
    }

    #[test]
    fn edit_preview_and_commit_use_the_retained_provider_index() {
        let provider = FailingIndexProvider {
            inner: memory_provider(),
            index_calls: Cell::new(0),
            read_calls: Cell::new(0),
            entry_reader_calls: Cell::new(0),
            annotation_reads: Cell::new(0),
            fail_on_index_call: 2,
        };
        let mut epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let preview = epub
            .edit()
            .upsert_resource("EPUB/extra.xhtml", b"extra".to_vec())
            .unwrap()
            .preview()
            .unwrap();
        let ordinal = preview
            .resources()
            .select(&ResourceSelector::path("EPUB/extra.xhtml").unwrap())
            .unwrap()
            .ordinal();
        let calls_before_commit = (
            preview.epub.container.index_calls.get(),
            preview.epub.container.read_calls.get(),
            preview.epub.container.entry_reader_calls.get(),
        );

        preview.commit();

        assert!(epub.resources().resource(ordinal).is_ok());
        assert!(
            epub.resource(ResourceSelector::path("EPUB/extra.xhtml").unwrap())
                .is_ok()
        );
        assert_eq!(epub.container.index_calls.get(), 1);
        assert_eq!(
            (
                epub.container.index_calls.get(),
                epub.container.read_calls.get(),
                epub.container.entry_reader_calls.get(),
            ),
            calls_before_commit
        );
    }
}
