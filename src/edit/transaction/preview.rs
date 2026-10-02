use super::*;

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    /// Checks the staged changes and shows you the book they would produce.
    ///
    /// The open publication is untouched until you commit.
    ///
    /// # Errors
    ///
    /// [`EditError`] when the staged changes would not make a coherent publication.
    pub fn preview(self) -> Result<EpubEditPreview<'a, R>> {
        let mut resource_changes = self.epub.resource_changes.clone();
        let mut changes = std::collections::BTreeMap::new();
        for change in self.edit_changes {
            changes.insert(change.path().clone(), change);
        }
        for (path, change) in self.changes.entries() {
            match change {
                ResourceChange::Upsert(bytes) => {
                    resource_changes.upsert(path.clone(), bytes.clone())
                }
                ResourceChange::Remove if self.epub.committed_resource_exists(path) => {
                    resource_changes.remove(path.clone());
                }
                ResourceChange::Remove => {
                    changes.remove(path);
                }
            }
        }
        let provider_index =
            resource_changes.apply_to_index(&self.epub.provider_index, &self.epub.open_limits)?;
        let package = self
            .package_override
            .unwrap_or_else(|| self.epub.package.clone());
        let navigation = self
            .navigation_override
            .unwrap_or_else(|| self.epub.navigation.clone());
        let resources = ResourceIndex::new(
            &package,
            self.epub.resources.package_path(),
            &provider_index,
        )?;
        Ok(EpubEditPreview {
            epub: self.epub,
            resource_changes,
            provider_index,
            changes: changes.into_values().collect(),
            package,
            navigation,
            resources,
        })
    }

    pub(super) fn staged_resource_exists(&self, path: &EpubPath) -> bool {
        match self.changes.entry(path) {
            Some(change) => change.is_some(),
            None => self.epub.committed_resource_exists(path),
        }
    }

    pub(super) fn staged_bytes(&self, path: &EpubPath) -> Result<Vec<u8>> {
        match self.changes.entry(path) {
            Some(Some(bytes)) => Ok(bytes.to_vec()),
            Some(None) => Err(ResourceReadError::Missing { path: path.clone() }.into()),
            None => Ok(committed_bytes(
                &self.epub.container,
                &self.epub.resource_changes,
                path,
            )?),
        }
    }

    pub(super) fn staged_bytes_bounded(&self, path: &EpubPath, limit: u64) -> Result<Vec<u8>> {
        match self.changes.entry(path) {
            Some(Some(bytes)) => {
                let mut staged = ResourceChanges::new();
                staged.upsert(path.clone(), bytes.to_vec());
                provider_bytes_with_changes_bounded(&self.epub.container, &staged, path, limit)
            }
            Some(None) => Err(ResourceReadError::Missing { path: path.clone() }.into()),
            None => provider_bytes_with_changes_bounded(
                &self.epub.container,
                &self.epub.resource_changes,
                path,
                limit,
            ),
        }
    }

    /// Rejects raw edits of the package document and of loaded or staged navigation documents.
    pub(super) fn reject_structural_path(&self, path: &EpubPath) -> Result<()> {
        let kind = self.epub.structural_resource_kind(path).or_else(|| {
            self.semantic_structural_paths
                .contains(path)
                .then_some(StructuralResourceKind::Navigation)
        });
        match kind {
            Some(kind) => Err(EditError::StructuralResourceEdit {
                path: path.clone(),
                kind,
            }),
            None => Ok(()),
        }
    }

    pub(super) fn stage_package_edit(
        &mut self,
        mutate: impl FnOnce(&mut Xot, Node) -> Result<()>,
        verify: impl FnOnce(&Package) -> Result<()>,
    ) -> Result<usize> {
        let package_path = self.epub.resources.package_path().clone();
        let package_bytes = self.staged_bytes(&package_path)?;
        let package_xml = decode_structural_xml(&package_bytes, &package_path)?;

        let mut xot = Xot::new();
        let doc = xot
            .parse(package_xml.as_ref())
            .map_err(|source| EditError::structural_xml(&package_path, source))?;
        mutate(&mut xot, doc)?;
        let package_xml = xot
            .to_string(doc)
            .map_err(|source| EditError::structural_xml(&package_path, source))?;

        let parsed_package = Package::parse(&package_xml)?;
        verify(&parsed_package)?;

        let package_bytes = package_xml.into_bytes();
        let size_bytes = package_bytes.len();
        self.changes.upsert(package_path.clone(), package_bytes);
        self.semantic_structural_paths.insert(package_path);
        self.package_override = Some(parsed_package);
        Ok(size_bytes)
    }

    pub(super) fn stage_navigation_edit(
        &mut self,
        mutate: impl FnOnce(&mut Xot, Node) -> Result<()>,
        verify: impl FnOnce(&Option<NavigationDocument>) -> Result<()>,
    ) -> Result<usize> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let nav_path = staged_navigation
            .as_ref()
            .filter(|document| document.is_epub_nav())
            .ok_or(EditError::MissingEpubNavigation)?
            .path()
            .clone();
        let nav_bytes = self.staged_bytes(&nav_path)?;
        let nav_xml = decode_structural_xml(&nav_bytes, &nav_path)?;

        let mut xot = Xot::new();
        let doc = xot
            .parse(nav_xml.as_ref())
            .map_err(|source| EditError::structural_xml(&nav_path, source))?;
        mutate(&mut xot, doc)?;
        let nav_xml = xot
            .to_string(doc)
            .map_err(|source| EditError::structural_xml(&nav_path, source))?;

        let parsed_nav = parse::epub_nav(nav_path.clone(), &nav_xml)?;
        let parsed_navigation = Some(parsed_nav);
        verify(&parsed_navigation)?;

        let nav_bytes = nav_xml.into_bytes();
        let size_bytes = nav_bytes.len();
        self.changes.upsert(nav_path.clone(), nav_bytes);
        self.semantic_structural_paths.insert(nav_path);
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
        let source_document = NavigationDocument::builder()
            .path(source_path)
            .lists(lists)
            .build()?;
        let nav_xml = source_document
            .generate_epub_nav_xhtml(&nav_path, title)
            .map_err(|error| navigation_generate_error(&nav_path, error))?;
        let parsed_navigation = Some(parse::epub_nav(nav_path.clone(), &nav_xml)?);

        let nav_bytes = nav_xml.into_bytes();
        let size_bytes = nav_bytes.len();
        self.changes.upsert(nav_path.clone(), nav_bytes);
        self.semantic_structural_paths.insert(nav_path);
        self.navigation_override = Some(parsed_navigation);
        Ok(size_bytes)
    }

    pub(super) fn stage_semantic_resource_removal(&mut self, path: EpubPath) {
        self.changes.remove(path.clone());
        self.semantic_structural_paths.insert(path.clone());
        self.edit_changes.push(EditChange::RemoveResource { path });
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

    fn nav_href_in_use_for_edit(&self, href: &str) -> bool {
        let Some((path, _)) = resolve_local_href_from_source(
            &AuthoredHref::new(href.to_string()),
            self.epub.resources.package_path(),
        ) else {
            return true;
        };
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        staged_package.manifest().items().iter().any(|item| {
            manifest_item_local_resource_path(item, self.epub.resources.package_path())
                .is_some_and(|item_path| item_path == path)
        }) || self.staged_resource_exists(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::provider::{
        MemoryResourceProvider, ProviderIndexError, ProviderReadError,
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
    fn edit_preview_and_commit_use_the_retained_provider_index() {
        let provider = FailingIndexProvider {
            inner: memory_provider(),
            index_calls: Cell::new(0),
            entry_reader_calls: Cell::new(0),
            annotation_reads: Cell::new(0),
            fail_on_index_call: 2,
        };
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let preview = epub
            .edit()
            .upsert_resource(
                EpubPath::new("EPUB/extra.xhtml").unwrap(),
                b"extra".to_vec(),
            )
            .unwrap()
            .preview()
            .unwrap();
        let ordinal = preview
            .resources()
            .resource_by_path(&EpubPath::new("EPUB/extra.xhtml").unwrap())
            .unwrap()
            .ordinal();
        let calls_before_commit = (
            preview.epub.container.index_calls.get(),
            preview.epub.container.entry_reader_calls.get(),
        );

        preview.commit();

        assert!(epub.resources().resource(ordinal).is_some());
        assert!(
            epub.bytes(&EpubPath::new("EPUB/extra.xhtml").unwrap())
                .is_ok()
        );
        assert_eq!(epub.container.index_calls.get(), 1);
        assert_eq!(
            (
                epub.container.index_calls.get(),
                epub.container.entry_reader_calls.get(),
            ),
            calls_before_commit
        );
    }
}
