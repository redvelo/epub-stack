use super::*;

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    /// Stages migration of an OPF 2.0 package to this library's EPUB 3 form.
    ///
    /// The operation updates package version, sets `dcterms:modified` to `modified`, converts known cover
    /// and guide semantics, generates EPUB NAV from selected navigation, and removes only the
    /// resolved `spine toc` NCX resource. Content documents are unchanged; EPUB 2 compatibility
    /// is not preserved.
    ///
    /// # Errors
    ///
    /// Returns [`EditError`] unless every coordinated package, NAV, and NCX step can be staged
    /// and semantically verified.
    pub fn migrate_opf2_to_epub3(mut self, modified: time::OffsetDateTime) -> Result<Self> {
        let modified = crate::package::dcterms_modified(modified)?;
        let staged_package = self.package_override.as_ref().unwrap_or(&self.epub.package);
        if staged_package.version() != Some(EpubVersion::Two) {
            return Err(EditError::NotOpf2);
        }

        let ncx_index = staged_package.ncx_item().and_then(|selected| {
            staged_package
                .manifest()
                .items()
                .iter()
                .position(|item| std::ptr::eq(item, selected))
        });
        let ncx_item = staged_package.ncx_item().cloned();
        let ncx_path = if let Some(ncx_item) = &ncx_item {
            let path = structural_manifest_href_path(ncx_item, self.epub.resources.package_path())
                .ok_or_else(|| invalid_navigation_href_error(ncx_item))?;
            Some(path)
        } else {
            None
        };
        let ncx_id = ncx_item
            .as_ref()
            .map(|item| {
                item.id()
                    .map(ToString::to_string)
                    .ok_or_else(|| invalid_navigation_href_error(item))
            })
            .transpose()?;
        let cover_ids = opf2_cover_meta_ids(staged_package)?;
        for cover_id in &cover_ids {
            if staged_package.manifest_item_by_id(cover_id).is_none() {
                return Err(EditError::InvalidOpf2Cover {
                    id: Some(cover_id.clone()),
                });
            }
        }

        let guide_landmarks = guide_landmark_points(staged_package)?;
        let (nav_path, nav_item) = if let Some(nav_item) = staged_package.nav_item() {
            let path = structural_manifest_href_path(nav_item, self.epub.resources.package_path())
                .ok_or_else(|| invalid_navigation_href_error(nav_item))?;
            (path, None)
        } else {
            let href = self.unique_generated_nav_href_for_edit();
            let (path, _) = resolve_local_href_from_source(
                &AuthoredHref::new(href.clone()),
                self.epub.resources.package_path(),
            )
            .expect("generated navigation href is a resolvable local href");
            let id = self.unique_manifest_id_for_edit("nav");
            let item = ManifestItem::builder()
                .id(EpubString::try_new(id).expect("generated manifest ID is non-empty"))
                .href(EpubHref::try_new(&href).expect("generated navigation href is valid"))
                .media_type(
                    MediaType::try_from("application/xhtml+xml")
                        .expect("static media type is valid"),
                )
                .properties(vec![KnownManifestProperty::Nav.into()])
                .build()?;
            (path, Some(item))
        };

        if let (Some(_), Some(ncx_path)) = (&ncx_id, &ncx_path) {
            reject_shared_manifest_resource_path(
                staged_package,
                ncx_index.expect("selected NCX belongs to the staged manifest"),
                ncx_path,
                self.epub.resources.package_path(),
                ncx_path == &nav_path,
            )?;
        }

        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let source_navigation = staged_navigation
            .as_ref()
            .filter(|document| document.is_epub_nav())
            .or_else(|| {
                staged_navigation
                    .as_ref()
                    .filter(|document| document.is_ncx())
            })
            .ok_or(EditError::MissingSourceNavigation)?;
        let source_nav_path = source_navigation.path().clone();
        let guide_landmarks = rebase_guide_landmarks(
            guide_landmarks,
            self.epub.resources.package_path(),
            &source_nav_path,
        )?;
        let migrated_lists = migrated_navigation_lists(source_navigation, guide_landmarks)?;
        let nav_title = staged_package
            .metadata()
            .elements(DcElement::Title)
            .iter()
            .find_map(Element::content)
            .cloned()
            .unwrap_or_else(|| {
                EpubString::try_new("Navigation").expect("static string is non-empty")
            });
        let nav_size = self.stage_generated_navigation(
            nav_path.clone(),
            source_nav_path,
            migrated_lists,
            &nav_title,
        )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path.clone(),
                kind: StructuralResourceKind::Navigation,
                size_bytes: nav_size,
            });

        let package_path = self.epub.resources.package_path().clone();
        let package_path_for_mutate = package_path.clone();
        let package_path_for_verify = package_path.clone();
        let modified_for_mutate = modified.clone();
        let modified_for_verify = modified.clone();
        let cover_ids_for_mutate = cover_ids.clone();
        let cover_ids_for_verify = cover_ids.clone();
        let nav_item_for_mutate = nav_item.clone();
        let package_edit = Opf2MigrationPackageEdit {
            modified: modified_for_mutate,
            nav_item: nav_item_for_mutate,
            cover_ids: cover_ids_for_mutate,
            ncx_id: ncx_id.clone(),
        };
        let package_size = self.stage_package_edit(
            |xot, doc| migrate_opf2_package_xml(xot, doc, &package_edit, &package_path_for_mutate),
            |package| {
                verify_opf2_migration_package(
                    package,
                    &modified_for_verify,
                    &cover_ids_for_verify,
                    &package_path_for_verify,
                )
            },
        )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: package_path,
                kind: StructuralResourceKind::Package,
                size_bytes: package_size,
            });

        if let Some(ncx_path) = ncx_path
            && ncx_path != nav_path
        {
            self.stage_semantic_resource_removal(ncx_path);
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::provider::MemoryResourceProvider;

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

    fn opf2_migration_provider(package: &str) -> MemoryResourceProvider {
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            (
                "EPUB/toc.ncx",
                br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap><navList class="landmarks"><navLabel><text>Illustrations</text></navLabel><navTarget><navLabel><text>Cover illustration</text></navLabel><content src="images/cover.jpg" /></navTarget></navList></ncx>"#.to_vec(),
            ),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
            ("EPUB/images/cover.jpg", b"cover".to_vec()),
        ])
        .unwrap()
    }

    #[test]
    fn migrate_opf2_to_epub3_generates_nav_and_removes_ncx() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><meta name="cover" content="cover-img" /></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
    <item id="cover-img" href="images/cover.jpg" media-type="image/jpeg" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
  <guide><reference type="text" title="Start" href="text/chapter.xhtml" /><reference type="cover" title="Cover" href="images/cover.jpg" /><reference type="notes" title="Notes" href="text/chapter.xhtml" /></guide>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().version(), Some(EpubVersion::Three));
        assert!(epub.package().nav_item().is_some());
        assert!(epub.package().ncx_item().is_none());
        assert!(epub.package().spine().toc().is_none());
        assert!(epub.package().guide().is_none());
        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_some()
        );
        assert!(
            epub.navigation()
                .filter(|document| document.is_ncx())
                .is_none()
        );
        assert!(
            epub.navigation()
                .as_ref()
                .filter(|document| document.is_epub_nav())
                .unwrap()
                .lists()
                .iter()
                .any(|list| list.semantic().is_none())
        );
        assert!(
            epub.package()
                .manifest_item_by_id("cover-img")
                .unwrap()
                .has_property(KnownManifestProperty::CoverImage)
        );
        assert!(epub.package().metadata().opf2meta().is_empty());
        assert!(epub.package().metadata().meta().iter().any(|meta| {
            meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
        }));

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("version=\"3.0\""));
        assert!(package_xml.contains("properties=\"nav\""));
        assert!(package_xml.contains("cover-image"));
        assert!(package_xml.contains("property=\"dcterms:modified\""));
        assert!(!package_xml.contains("toc=\"ncx\""));
        assert!(!package_xml.contains("toc.ncx"));
        assert!(!package_xml.contains("<guide"));
        assert!(!package_xml.contains("name=\"cover\""));

        let nav_xml = epub
            .utf8_text(epub.resources().epub_nav().unwrap().local_path().unwrap())
            .unwrap();
        assert!(nav_xml.contains("<title>T</title>"));
        assert!(nav_xml.contains("epub:type=\"toc\""));
        assert!(nav_xml.contains("epub:type=\"landmarks\""));
        assert_eq!(nav_xml.matches("epub:type=\"landmarks\"").count(), 1);
        assert!(nav_xml.contains("Illustrations"));
        assert!(nav_xml.contains("Start"));
        assert!(nav_xml.contains("Cover"));
        assert!(nav_xml.contains("Notes"));
        assert!(nav_xml.contains("cover"));
        assert!(!nav_xml.contains("epub:type=\"footnotes\""));
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/toc.ncx").unwrap()),
            Err(crate::resource::ResourceReadError::Missing { .. })
        ));
    }

    #[test]
    fn migrate_opf2_to_epub3_preserves_duplicate_modified_meta() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><meta id="selected" property="dcterms:modified">2000-01-01T00:00:00Z</meta><meta id="duplicate" property="dcterms:modified">2001-01-01T00:00:00Z</meta></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(package_xml.contains("id=\"selected\""));
        assert!(package_xml.contains("id=\"duplicate\""));
        assert!(package_xml.contains("2001-01-01T00:00:00Z"));
        assert!(!package_xml.contains("2000-01-01T00:00:00Z"));
        assert_eq!(
            package_xml.matches("property=\"dcterms:modified\"").count(),
            2
        );
    }

    #[test]
    fn migrate_opf2_to_epub3_preserves_foreign_xml_and_adjacent_comment() {
        const VENDOR_NS: &str = "urn:example:vendor";
        const COMMENT: &str = "<!--preserve migration boundary-->";
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:v="urn:example:vendor" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><v:extension v:flag="keep">Vendor data</v:extension></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx" v:state="keep"><itemref idref="chap" /></spine>
  <!--preserve migration boundary-->
  <guide><reference type="text" title="Start" href="text/chapter.xhtml" /></guide>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        epub.edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(epub.package().version(), Some(EpubVersion::Three));
        assert!(epub.package().nav_item().is_some());
        assert!(epub.package().ncx_item().is_none());
        assert!(epub.package().spine().toc().is_none());
        assert!(epub.package().guide().is_none());

        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        let mut xot = Xot::new();
        let doc = xot.parse(&package_xml).unwrap();
        let package = xot.document_element(doc).unwrap();
        let foreign_element = xot
            .descendants(package)
            .find(|node| is_element_ns(&xot, *node, VENDOR_NS, "extension"))
            .unwrap();
        let vendor_namespace = xot.add_namespace(VENDOR_NS);
        let flag_name = xot.add_name_ns("flag", vendor_namespace);
        let state_name = xot.add_name_ns("state", vendor_namespace);
        assert_eq!(xot.get_attribute(foreign_element, flag_name), Some("keep"));
        let spine = find_opf_child(&xot, package, "spine").unwrap();
        assert_eq!(xot.get_attribute(spine, state_name), Some("keep"));
        assert!(package_xml.contains(COMMENT));
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_non_opf2_package() {
        let mut epub = memory_provider_epub();
        let error = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(error, EditError::NotOpf2));
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_missing_cover_manifest_item() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><meta name="cover" content="missing-cover" /></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let error = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(error, EditError::InvalidOpf2Cover { .. }));
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_cover_meta_without_content() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language><meta name="cover" /></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let error = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(error, EditError::InvalidOpf2Cover { .. }));
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_invalid_spine_toc_href() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav" /><item id="ncx" href="https://example.com/toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
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
        let error = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(error, EditError::InvalidNavigationHref { .. }));
    }

    #[test]
    fn migrate_opf2_to_epub3_preserves_unrelated_ncx_item() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="archive-ncx" href="archive.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/toc.ncx", br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap></ncx>"#.to_vec()),
            ("EPUB/archive.ncx", br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap /></ncx>"#.to_vec()),
            ("EPUB/text/chapter.xhtml", b"<html><body>Chapter</body></html>".to_vec()),
        ]).unwrap();
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        epub.edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        let package_xml = epub
            .utf8_text(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        assert!(!package_xml.contains("href=\"toc.ncx\""));
        assert!(package_xml.contains("href=\"archive.ncx\""));
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/toc.ncx").unwrap()),
            Err(crate::resource::ResourceReadError::Missing { .. })
        ));
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/archive.ncx").unwrap())
                .unwrap()
                .contains("<ncx")
        );
    }

    #[test]
    fn opf2_guide_types_map_to_epub3_landmark_semantics() {
        use crate::semantics::EpubStructuralSemantic::{Bodymatter, Dedication, Loi, Lot};
        assert_eq!(
            guide_semantics(&ReferenceType::Dedication),
            Some(Dedication)
        );
        assert_eq!(guide_semantics(&ReferenceType::Loi), Some(Loi));
        assert_eq!(guide_semantics(&ReferenceType::Lot), Some(Lot));
        assert_eq!(guide_semantics(&ReferenceType::Text), Some(Bodymatter));
        assert_eq!(guide_semantics(&ReferenceType::Notes), None);
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_malformed_guide_hrefs() {
        for (attribute, expected) in [
            ("", GuideHrefFailure::Missing),
            (
                r#" href="""#,
                GuideHrefFailure::Blank {
                    href: String::new(),
                },
            ),
            (
                r#" href="   ""#,
                GuideHrefFailure::Blank {
                    href: "   ".to_string(),
                },
            ),
            (
                r#" href="chapter%ZZ.xhtml""#,
                GuideHrefFailure::InvalidSyntax {
                    href: "chapter%ZZ.xhtml".to_string(),
                },
            ),
        ] {
            let package = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
  <guide><reference type="text" title="Start"{attribute} /></guide>
</package>"#
            );
            let provider = opf2_migration_provider(&package);
            let mut epub =
                Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
            let error = epub
                .edit()
                .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
                .unwrap_err();
            assert!(
                matches!(error, EditError::InvalidGuideHref { failure } if failure == expected)
            );
        }
    }

    #[test]
    fn migrate_opf2_to_epub3_avoids_canonical_manifest_href_aliases() {
        for alias in [
            "./nav.xhtml",
            "generated/../nav.xhtml",
            "./nav.xhtml#section",
        ] {
            let package = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /><item id="reserved" href="{alias}" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#
            );
            let provider = opf2_migration_provider(&package);
            let mut epub =
                Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
            let preview = epub
                .edit()
                .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
                .unwrap()
                .preview()
                .unwrap();
            assert_eq!(
                preview
                    .package()
                    .nav_item()
                    .unwrap()
                    .authored_href()
                    .unwrap()
                    .as_str(),
                "nav-1.xhtml"
            );
        }
    }

    #[test]
    fn migrate_opf2_to_epub3_avoids_ambiguous_duplicate_nav_ids() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="nav" href="reserved-a.xhtml" media-type="application/xhtml+xml" />
    <item id="nav" href="reserved-b.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let preview = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap();

        assert_eq!(preview.package().nav_item().unwrap().id(), Some("nav-1"));
        assert_eq!(
            preview
                .package()
                .manifest_items_by_id("nav")
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn migrate_opf2_to_epub3_rejects_shared_selected_ncx_transactionally() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="shared" href="./toc.ncx#section" media-type="application/xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#;
        let provider = opf2_migration_provider(package);
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let error = epub
            .edit()
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(error, EditError::SharedResourcePath { .. }));
        assert_eq!(epub.package().version(), Some(EpubVersion::Two));
        assert!(epub.package().manifest_item_by_id("ncx").is_some());
        assert!(epub.package().manifest_item_by_id("shared").is_some());
        assert!(
            epub.utf8_text(&EpubPath::new("EPUB/toc.ncx").unwrap())
                .unwrap()
                .contains("<ncx")
        );
    }

    #[test]
    fn migrate_opf2_to_epub3_keeps_generated_nav_at_selected_ncx_path() {
        let provider = opf2_migration_provider(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
        );
        let mut epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let nav_item = ManifestItem::builder()
            .id(EpubString::try_new("nav").unwrap())
            .href(EpubHref::try_new("./toc.ncx").unwrap())
            .media_type(MediaType::try_from("application/xhtml+xml").unwrap())
            .properties(vec![KnownManifestProperty::Nav.into()])
            .build()
            .unwrap();
        let mut edit = epub.edit();
        let package_path = edit.epub.resources.package_path().clone();
        let package_path_for_mutate = package_path.clone();
        let nav_item_for_mutate = nav_item.clone();
        edit.stage_package_edit(
            |xot, doc| {
                append_manifest_item_to_package_xml(
                    xot,
                    doc,
                    &nav_item_for_mutate,
                    &package_path_for_mutate,
                )
            },
            |package| {
                assert!(package.nav_item().is_some());
                Ok(())
            },
        )
        .unwrap();
        let preview = edit
            .migrate_opf2_to_epub3(time::OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .preview()
            .unwrap();
        assert!(!preview.changes().iter().any(|change| matches!(change, EditChange::RemoveResource { path } if path.as_str() == "EPUB/toc.ncx")));
        preview.commit();
        assert!(epub.package().manifest_item_by_id("ncx").is_none());
        assert_eq!(
            epub.package()
                .nav_item()
                .unwrap()
                .authored_href()
                .unwrap()
                .as_str(),
            "./toc.ncx"
        );
        assert!(
            epub.utf8_text(epub.resources().epub_nav().unwrap().local_path().unwrap())
                .unwrap()
                .contains("<html")
        );
    }
}
