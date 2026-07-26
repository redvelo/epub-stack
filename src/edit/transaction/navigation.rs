use super::*;

impl<'a, R: ResourceProvider> EpubEdit<'a, R> {
    /// Stages replacement of one point label in the EPUB navigation document.
    ///
    /// [`PointSelector`] first selects one normalized list and then one point. Path matches
    /// are zero-based child paths; normalized href, exact authored href, and label matches
    /// search the whole selected tree and must be unique. The edit changes the selected NAV
    /// label's text while retaining its inline elements. It does not update an NCX.
    ///
    /// # Errors
    ///
    /// Returns [`crate::edit::EditError`] if there is no EPUB NAV document, the list or point is
    /// missing or ambiguous, the point has no editable anchor or span label, the staged NAV
    /// source cannot be read, parsed, edited, or verified, or an earlier raw edit replaced
    /// that structural source.
    pub fn set_nav_point_label(
        mut self,
        selector: PointSelector,
        label: EpubString,
    ) -> Result<Self> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let target = resolve_nav_point(staged_navigation, &selector)?;
        let edit_target = NavPointEditTarget::from(&target);
        let nav_path = edit_target.nav_path.clone();
        let point_path = edit_target.point_path.clone();
        let list_index = edit_target.list_index;
        let label_for_mutate = label.clone();
        let label_for_verify = label.clone();
        let size_bytes = self.stage_navigation_edit(
            |xot, doc| replace_nav_point_label_in_xml(xot, doc, &edit_target, &label_for_mutate),
            |navigation| {
                let target = resolve_nav_point(
                    navigation,
                    &PointSelector::new(
                        ListSelector::Index(list_index),
                        PointMatch::Path(point_path.clone()),
                    ),
                )?;
                if target.point.label() != Some(&label_for_verify) {
                    return Err(EditError::StructuralXml {
                        path: nav_path.clone(),
                        message: "navigation point label could not be updated while preserving inline markup"
                            .to_string(),
                    });
                }
                Ok(())
            },
        )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path,
                kind: StructuralEditKind::Navigation,
                size_bytes,
            });
        Ok(self)
    }

    /// Stages replacement of one anchored point's href in the EPUB navigation document.
    ///
    /// [`PointSelector`] uses a zero-based path or a unique normalized href, exact authored
    /// href, or normalized label match within one selected list. Only the selected NAV anchor's
    /// `href` attribute is changed. No NCX is updated.
    ///
    /// # Errors
    ///
    /// Returns [`crate::edit::EditError`] if there is no EPUB NAV document, the list or point is
    /// missing or ambiguous, the selected point has no anchor, the staged NAV source cannot
    /// be read, parsed, edited, or verified, or an earlier raw edit replaced that structural
    /// source.
    pub fn set_nav_point_href(mut self, selector: PointSelector, href: EpubHref) -> Result<Self> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let target = resolve_nav_point(staged_navigation, &selector)?;
        let edit_target = NavPointEditTarget::from(&target);
        let nav_path = edit_target.nav_path.clone();
        let point_path = edit_target.point_path.clone();
        let list_index = edit_target.list_index;
        let href_for_mutate = href.clone();
        let href_for_verify = href.clone();
        let size_bytes = self.stage_navigation_edit(
            |xot, doc| replace_nav_point_href_in_xml(xot, doc, &edit_target, &href_for_mutate),
            |navigation| {
                let target = resolve_nav_point(
                    navigation,
                    &PointSelector::new(
                        ListSelector::Index(list_index),
                        PointMatch::Path(point_path.clone()),
                    ),
                )?;
                if target.point.href().as_ref() != Some(&href_for_verify) {
                    return Err(EditError::StructuralXml {
                        path: nav_path.clone(),
                        message: "navigation point href was not updated".to_string(),
                    });
                }
                Ok(())
            },
        )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path,
                kind: StructuralEditKind::Navigation,
                size_bytes,
            });
        Ok(self)
    }

    /// Stages a new point at the end of a list or selected point's children in EPUB NAV.
    ///
    /// [`InsertionTarget::List`] appends at the selected list's top level.
    /// [`InsertionTarget::ChildOf`] resolves its [`PointSelector`] uniquely and appends to
    /// that point's children, creating the immediate `ol` when absent. The supplied model is
    /// serialized as new XHTML NAV markup; no NCX is updated.
    ///
    /// # Errors
    ///
    /// Returns [`crate::edit::EditError`] if there is no EPUB NAV document, the target list or
    /// parent is missing or ambiguous, the point cannot be represented as NAV markup, the
    /// staged NAV source cannot be read, parsed, edited, or verified, or an earlier raw edit
    /// replaced that structural source.
    pub fn add_nav_point(
        mut self,
        target: InsertionTarget,
        point: NavigationPoint,
    ) -> Result<Self> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let target = resolve_nav_insertion_target(staged_navigation, &target)?;
        let edit_target = NavInsertionEditTarget::from(&target);
        let nav_path = edit_target.nav_path.clone();
        let verify_list_index = edit_target.list_index;
        let verify_parent_path = edit_target.parent_path.clone();
        let expected_index = target.existing_len;
        let point_for_mutate = point.clone();
        let point_for_verify = point.clone();
        let size_bytes =
            self.stage_navigation_edit(
                |xot, doc| append_nav_point_in_xml(xot, doc, &edit_target, &point_for_mutate),
                |navigation| {
                    let document = navigation.epub_nav().ok_or_else(|| {
                        EditError::UnsupportedSemanticEdit {
                            message: "NAV semantic edits require an EPUB navigation document"
                                .to_string(),
                        }
                    })?;
                    let list = document.lists().get(verify_list_index).ok_or_else(|| {
                        EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: format!("navigation list {verify_list_index} was not present"),
                        }
                    })?;
                    let points = if let Some(parent_path) = verify_parent_path.as_deref() {
                        nav_point_by_path(list.points(), parent_path)
                            .map(NavigationPoint::children)
                            .ok_or_else(|| EditError::StructuralXml {
                                path: nav_path.clone(),
                                message: format!(
                                    "navigation insertion parent {parent_path:?} was not present"
                                ),
                            })?
                    } else {
                        list.points()
                    };
                    if !points.get(expected_index).is_some_and(|point| {
                        nav_point_matches_written_model(point, &point_for_verify)
                    }) {
                        return Err(EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: "navigation point was not appended".to_string(),
                        });
                    }
                    Ok(())
                },
            )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path,
                kind: StructuralEditKind::Navigation,
                size_bytes,
            });
        Ok(self)
    }

    /// Stages removal of one point and its subtree from the EPUB navigation document.
    ///
    /// [`PointSelector`] uses a zero-based path or a unique normalized href, exact authored
    /// href, or normalized label match within one selected list. Only the selected NAV `li`
    /// subtree is removed. No NCX is updated.
    ///
    /// # Errors
    ///
    /// Returns [`crate::edit::EditError`] if there is no EPUB NAV document, the list or point is
    /// missing or ambiguous, the staged NAV source cannot be read, parsed, edited, or
    /// verified, or an earlier raw edit replaced that structural source.
    pub fn remove_nav_point(mut self, selector: PointSelector) -> Result<Self> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let target = resolve_nav_point(staged_navigation, &selector)?;
        let edit_target = NavPointEditTarget::from(&target);
        let nav_path = edit_target.nav_path.clone();
        let list_index = edit_target.list_index;
        let parent_path = nav_parent_path(&edit_target.point_path);
        let expected_len = nav_points_at_parent(
            &staged_navigation.epub_nav().unwrap().lists()[list_index],
            parent_path.as_deref(),
        )
        .map(|points| points.len().saturating_sub(1))
        .unwrap_or(0);
        let size_bytes =
            self.stage_navigation_edit(
                |xot, doc| remove_nav_point_from_xml(xot, doc, &edit_target),
                |navigation| {
                    let document = navigation.epub_nav().ok_or_else(|| {
                        EditError::UnsupportedSemanticEdit {
                            message: "NAV semantic edits require an EPUB navigation document"
                                .to_string(),
                        }
                    })?;
                    let list = document.lists().get(list_index).ok_or_else(|| {
                        EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: format!("navigation list {list_index} was not present"),
                        }
                    })?;
                    let points =
                        nav_points_at_parent(list, parent_path.as_deref()).ok_or_else(|| {
                            EditError::StructuralXml {
                                path: nav_path.clone(),
                                message: "navigation removal parent was not present".to_string(),
                            }
                        })?;
                    if points.len() != expected_len {
                        return Err(EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: "navigation point was not removed".to_string(),
                        });
                    }
                    Ok(())
                },
            )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path,
                kind: StructuralEditKind::Navigation,
                size_bytes,
            });
        Ok(self)
    }

    /// Stages moving one NAV point and its subtree to the end of another child list.
    ///
    /// `selector` resolves one point by [`PointSelector`] semantics. The
    /// [`InsertionTarget`] selects either a list's top level or one uniquely selected
    /// parent's children; the moved point is appended there, and an absent immediate `ol` is
    /// created. The existing `li` subtree is moved intact. Only EPUB NAV changes; no NCX is
    /// updated.
    ///
    /// # Errors
    ///
    /// Returns [`crate::edit::EditError`] if there is no EPUB NAV document, either selector is
    /// missing or ambiguous, the destination is the point itself or its descendant, the
    /// staged NAV source cannot be read, parsed, edited, or verified, or an earlier raw edit
    /// replaced that structural source.
    pub fn move_nav_point(
        mut self,
        selector: PointSelector,
        target: InsertionTarget,
    ) -> Result<Self> {
        let staged_navigation = self
            .navigation_override
            .as_ref()
            .unwrap_or(&self.epub.navigation);
        let source = resolve_nav_point(staged_navigation, &selector)?;
        let destination = resolve_nav_insertion_target(staged_navigation, &target)?;
        validate_nav_move_target(&source, &destination)?;
        let source_target = NavPointEditTarget::from(&source);
        let destination_target = NavInsertionEditTarget::from(&destination);
        let nav_path = source_target.nav_path.clone();
        let moved_point = source.point.clone();
        let verify_list_index = destination_target.list_index;
        let verify_parent_path = destination_target.parent_path.clone();
        let expected_index = nav_move_destination_index(&source, &destination);
        let size_bytes =
            self.stage_navigation_edit(
                |xot, doc| move_nav_point_in_xml(xot, doc, &source_target, &destination_target),
                |navigation| {
                    let document = navigation.epub_nav().ok_or_else(|| {
                        EditError::UnsupportedSemanticEdit {
                            message: "NAV semantic edits require an EPUB navigation document"
                                .to_string(),
                        }
                    })?;
                    let list = document.lists().get(verify_list_index).ok_or_else(|| {
                        EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: format!("navigation list {verify_list_index} was not present"),
                        }
                    })?;
                    let points = nav_points_at_parent(list, verify_parent_path.as_deref())
                        .ok_or_else(|| EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: "navigation move destination was not present".to_string(),
                        })?;
                    if points.get(expected_index) != Some(&moved_point) {
                        return Err(EditError::StructuralXml {
                            path: nav_path.clone(),
                            message: "navigation point was not moved".to_string(),
                        });
                    }
                    Ok(())
                },
            )?;
        self.edit_changes
            .push(EditChange::RewriteStructuralResource {
                path: nav_path,
                kind: StructuralEditKind::Navigation,
                size_bytes,
            });
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::EpubZip;
    use std::io::{Cursor, Write};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn nav_edit_epub(nav: &str) -> Epub<EpubZip<Cursor<Vec<u8>>>> {
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
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#,
                options,
            );
            write_zip_entry(&mut zip, "EPUB/nav.xhtml", nav, options);
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

    fn nav_and_malformed_ncx_edit_epub() -> Epub<EpubZip<Cursor<Vec<u8>>>> {
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
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav" />
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/nav.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/toc.ncx",
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint class="one" class="two"><navLabel><text>Chapter</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap></ncx>"#,
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

    fn write_zip_entry(
        zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
        path: &str,
        contents: &str,
        options: SimpleFileOptions,
    ) {
        zip.start_file(path, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn edit_sets_nav_point_label_and_preserves_anchor_attributes() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a class="keep" data-x="1" href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        );

        let preview = epub
            .edit()
            .set_nav_point_label(
                PointSelector::toc(PointMatch::path(vec![0])),
                EpubString::try_new("New Chapter").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap();
        assert!(preview.changes().iter().any(|change| matches!(
            change,
            EditChange::RewriteStructuralResource { path, .. } if path.as_str() == "EPUB/nav.xhtml"
        )));
        preview.commit();

        let toc = epub.navigation().epub_nav().unwrap().toc().unwrap();
        assert_eq!(toc.points()[0].label().unwrap().as_str(), "New Chapter");
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(nav_text.contains("New Chapter"));
        assert!(nav_text.contains("class=\"keep\""));
        assert!(nav_text.contains("data-x=\"1\""));
    }

    #[test]
    fn edit_sets_nav_point_label_and_preserves_inline_markup() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xmlns:pub="urn:publisher"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml"><span class="keep"><b pub:role="chapter">Chap</b>ter</span></a></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .set_nav_point_label(
                PointSelector::toc(PointMatch::path(vec![0])),
                EpubString::try_new("New Chapter").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        assert_eq!(
            epub.navigation()
                .epub_nav()
                .unwrap()
                .toc()
                .unwrap()
                .points()[0]
                .label()
                .unwrap()
                .as_str(),
            "New Chapter"
        );
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(nav_text.contains("<span class=\"keep\">"));
        assert!(nav_text.contains("<b pub:role=\"chapter\">New Chapter</b>"));
        assert!(nav_text.contains("xmlns:pub=\"urn:publisher\""));
    }

    #[test]
    fn edit_sets_nested_nav_point_href_by_label() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a><ol><li><a href="text/chapter.xhtml#old">Section</a></li></ol></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .set_nav_point_href(
                PointSelector::toc(PointMatch::label(EpubString::try_new("Section").unwrap())),
                EpubHref::try_new("text/chapter.xhtml#new").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let section = &epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[0]
            .children()[0];
        assert_eq!(section.href().unwrap().as_str(), "text/chapter.xhtml#new");
    }

    #[test]
    fn edit_nav_point_label_selector_must_be_unique() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml#one">Same</a></li><li><a href="text/chapter.xhtml#two">Same</a></li></ol></nav></body></html>"#,
        );

        let error = epub
            .edit()
            .set_nav_point_href(
                PointSelector::toc(PointMatch::label(EpubString::try_new("Same").unwrap())),
                EpubHref::try_new("text/chapter.xhtml#new").unwrap(),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            EditError::Selection {
                target: "navigation point",
                failure: SelectionFailure::Ambiguous,
                ..
            }
        ));
    }

    #[test]
    fn edit_nav_point_requires_epub_nav_document() {
        let mut epub = ncx_only_edit_epub();

        let error = epub
            .edit()
            .set_nav_point_label(
                PointSelector::toc(PointMatch::path(vec![0])),
                EpubString::try_new("New").unwrap(),
            )
            .unwrap_err();

        assert!(matches!(error, EditError::UnsupportedSemanticEdit { .. }));
    }
    #[test]
    fn edit_nav_point_keeps_epub_nav_selected_over_unselected_ncx() {
        let mut epub = nav_and_malformed_ncx_edit_epub();
        assert!(epub.navigation().epub_nav().is_some());
        assert!(epub.navigation().ncx().is_none());

        let preview = epub
            .edit()
            .set_nav_point_label(
                PointSelector::toc(PointMatch::path(vec![0])),
                EpubString::try_new("New Chapter").unwrap(),
            )
            .unwrap()
            .preview()
            .unwrap();

        assert_eq!(preview.changes().len(), 1);
        preview.commit();
        assert_eq!(
            epub.navigation()
                .epub_nav()
                .unwrap()
                .toc()
                .unwrap()
                .points()[0]
                .label()
                .map(EpubString::as_str),
            Some("New Chapter")
        );
        assert!(epub.navigation().ncx().is_none());
    }

    #[test]
    fn edit_add_nav_point_appends_to_toc_root() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("Appendix").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml#appendix").unwrap())
            .build()
            .unwrap();

        epub.edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Toc), point)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].label().unwrap().as_str(), "Appendix");
        assert_eq!(
            points[1].href().unwrap().as_str(),
            "text/chapter.xhtml#appendix"
        );
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(!nav_text.contains("xmlns=\"\""));
    }

    #[test]
    fn edit_add_nav_point_appends_child_and_creates_ol() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("Section").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml#section").unwrap())
            .build()
            .unwrap();

        epub.edit()
            .add_nav_point(
                InsertionTarget::child_of(PointSelector::toc(PointMatch::path(vec![0]))),
                point,
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let child = &epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[0]
            .children()[0];
        assert_eq!(child.label().unwrap().as_str(), "Section");
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(nav_text.contains("<ol"));
        assert!(nav_text.contains("Section"));
        assert!(!nav_text.contains("xmlns=\"\""));
    }

    #[test]
    fn edit_add_nav_point_creates_missing_root_ol() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("Chapter").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml").unwrap())
            .build()
            .unwrap();

        epub.edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Toc), point)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].label().unwrap().as_str(), "Chapter");
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(!nav_text.contains("xmlns=\"\""));
    }

    #[test]
    fn edit_add_nav_point_preserves_parsed_epub_and_role_tokens() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml" epub:type="unknown cover chapter" role="doc-cover custom-role">Chapter</a></li></ol></nav></body></html>"#,
        );
        let point = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[0]
            .clone();

        epub.edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Toc), point)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let inserted = &epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[1];
        assert_eq!(
            inserted
                .authored_semantic_tokens()
                .iter()
                .map(|token| token.raw())
                .collect::<Vec<_>>(),
            vec!["unknown", "cover", "chapter", "doc-cover", "custom-role"]
        );
    }

    #[test]
    fn edit_add_nav_point_rejects_ncx_class_semantics() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol/></nav></body></html>"#,
        );
        let ncx = parse::ncx(
                EpubPath::new("EPUB/toc.ncx").unwrap(),
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint class="chapter"><navLabel><text>Chapter</text></navLabel><content src="chapter.xhtml"/></navPoint></navMap></ncx>"#,
            )
            .unwrap();
        let point = ncx.toc().unwrap().points()[0].clone();

        let error = epub
            .edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Toc), point)
            .unwrap_err();

        assert!(matches!(
            error,
            EditError::StructuralXml { message, .. }
                if message.contains("NCX class semantics")
        ));
    }

    #[test]
    fn edit_add_nav_point_rejects_missing_list() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("Page 1").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml#p1").unwrap())
            .build()
            .unwrap();

        let error = epub
            .edit()
            .add_nav_point(InsertionTarget::List(ListSelector::PageList), point)
            .unwrap_err();

        assert!(matches!(
            error,
            EditError::Selection {
                target: "navigation list",
                failure: SelectionFailure::NotFound,
                ..
            }
        ));
    }

    #[test]
    fn edit_add_nav_point_appends_to_landmarks_root() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="landmarks"><ol><li><a epub:type="bodymatter" href="text/chapter.xhtml">Start</a></li></ol></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("Table of Contents").unwrap())
            .href(EpubHref::try_new("nav.xhtml").unwrap())
            .semantic(crate::semantics::EpubStructuralSemantic::Toc)
            .build()
            .unwrap();

        epub.edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Landmarks), point)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .landmarks()
            .unwrap()
            .points();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].label().unwrap().as_str(), "Table of Contents");
        assert_eq!(
            points[1].semantic(),
            Some(crate::semantics::EpubStructuralSemantic::Toc)
        );
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(nav_text.contains("epub:type=\"toc\""));
        assert!(!nav_text.contains("xmlns=\"\""));
    }

    #[test]
    fn edit_add_nav_point_appends_to_list_root_by_index() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav><nav epub:type="page-list"><ol><li><a href="text/chapter.xhtml#p1">1</a></li></ol></nav></body></html>"#,
        );
        let point = NavigationPoint::builder()
            .label(EpubString::try_new("2").unwrap())
            .href(EpubHref::try_new("text/chapter.xhtml#p2").unwrap())
            .build()
            .unwrap();

        epub.edit()
            .add_nav_point(InsertionTarget::List(ListSelector::Index(1)), point)
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let page_list = epub.navigation().epub_nav().unwrap().page_list().unwrap();
        assert_eq!(page_list.points().len(), 2);
        assert_eq!(page_list.points()[1].label().unwrap().as_str(), "2");
    }

    #[test]
    fn edit_remove_nav_point_removes_selected_point() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml#one">One</a></li><li><a href="text/chapter.xhtml#two">Two</a></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .remove_nav_point(PointSelector::toc(PointMatch::label(
                EpubString::try_new("One").unwrap(),
            )))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].label().unwrap().as_str(), "Two");
    }

    #[test]
    fn edit_remove_nav_point_removes_nested_point() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a><ol><li><a href="text/chapter.xhtml#one">One</a></li><li><a href="text/chapter.xhtml#two">Two</a></li></ol></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .remove_nav_point(PointSelector::toc(PointMatch::path(vec![0, 0])))
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let children = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[0]
            .children();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].label().unwrap().as_str(), "Two");
    }

    #[test]
    fn edit_move_nav_point_appends_to_target() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a><ol><li><a class="keep" href="text/chapter.xhtml#section">Section</a></li></ol></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .move_nav_point(
                PointSelector::toc(PointMatch::label(EpubString::try_new("Section").unwrap())),
                InsertionTarget::List(ListSelector::Toc),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].label().unwrap().as_str(), "Section");
        assert!(points[0].children().is_empty());
        let nav_text = epub
            .resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap();
        assert!(nav_text.contains("class=\"keep\""));
    }

    #[test]
    fn edit_move_nav_point_reorders_within_same_parent() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml#one">One</a></li><li><a href="text/chapter.xhtml#two">Two</a></li></ol></nav></body></html>"#,
        );

        epub.edit()
            .move_nav_point(
                PointSelector::toc(PointMatch::label(EpubString::try_new("One").unwrap())),
                InsertionTarget::List(ListSelector::Toc),
            )
            .unwrap()
            .preview()
            .unwrap()
            .commit();

        let points = epub
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points();
        assert_eq!(points[0].label().unwrap().as_str(), "Two");
        assert_eq!(points[1].label().unwrap().as_str(), "One");
    }

    #[test]
    fn edit_move_nav_point_rejects_descendant_target() {
        let mut epub = nav_edit_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a><ol><li><a href="text/chapter.xhtml#section">Section</a></li></ol></li></ol></nav></body></html>"#,
        );

        let error = epub
            .edit()
            .move_nav_point(
                PointSelector::toc(PointMatch::path(vec![0])),
                InsertionTarget::child_of(PointSelector::toc(PointMatch::path(vec![0, 0]))),
            )
            .unwrap_err();

        assert!(matches!(error, EditError::UnsupportedSemanticEdit { .. }));
    }
}
