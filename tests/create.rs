use epub_stack::edit::navigation::{InsertionTarget, ListSelector};
use epub_stack::navigation::NavigationPoint;
use epub_stack::package::{
    PackageError,
    manifest::{KnownManifestProperty, ManifestItem},
    spine::ItemRef,
};
use epub_stack::resource::{EpubHref, MediaType};
use epub_stack::semantics::{EpubString, HeadingLevel};
use epub_stack::{Epub, EpubCreateError, EpubZip, ResourceSelector};
use std::io::Cursor;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[test]
fn create_builds_a_minimal_conventional_publication() {
    let modified = OffsetDateTime::parse("2026-07-20T14:15:16+02:00", &Rfc3339).unwrap();
    let book = Epub::create("urn:uuid:new-book", "New Book", "en", modified).unwrap();

    assert_eq!(book.package_path().as_str(), "EPUB/package.opf");
    assert!(book.package().spine().itemrefs().is_empty());
    assert_eq!(book.resources().reading_order().count(), 0);
    assert_eq!(
        book.package().unique_identifier().unwrap().as_str(),
        "urn:uuid:new-book"
    );
    assert_eq!(
        book.package().metadata().title()[0]
            .content()
            .unwrap()
            .as_str(),
        "New Book"
    );
    assert_eq!(
        book.package().metadata().language()[0]
            .content()
            .unwrap()
            .as_str(),
        "en"
    );
    assert!(book.package().metadata().meta().iter().any(|meta| {
        meta.property().map(|property| property.as_str()) == Some("dcterms:modified")
            && meta.content().map(EpubString::as_str) == Some("2026-07-20T12:15:16Z")
    }));
    let [nav_item] = book.package().manifest().items() else {
        panic!("created package must declare exactly one NAV item");
    };
    assert_eq!(nav_item.id().unwrap(), "nav");
    assert_eq!(nav_item.href().unwrap().as_str(), "nav.xhtml");
    assert_eq!(
        nav_item.media_type().unwrap().as_str(),
        "application/xhtml+xml"
    );
    assert!(
        nav_item
            .properties()
            .iter()
            .any(|property| property.known_value() == Some(KnownManifestProperty::Nav))
    );
    let toc = book.navigation().epub_nav().unwrap().toc().unwrap();
    assert!(toc.points().is_empty());
    let heading = toc.heading().unwrap();
    assert_eq!(heading.level(), HeadingLevel::new(1).unwrap());
    assert_eq!(heading.text().as_str(), "Contents");

    let provider = book.into_base_provider();
    let paths = provider
        .entries()
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "EPUB/nav.xhtml",
            "EPUB/package.opf",
            "META-INF/container.xml",
            "mimetype"
        ]
    );
}

#[test]
fn create_reports_empty_required_metadata() {
    let modified = OffsetDateTime::UNIX_EPOCH;
    for (identifier, title, language, field) in [
        ("", "Title", "en", "identifier"),
        ("urn:test", "", "en", "title"),
        ("urn:test", "Title", "", "language"),
    ] {
        assert!(matches!(
            Epub::create(identifier, title, language, modified),
            Err(EpubCreateError::Package {
                source: PackageError::EmptyField { field: actual }
            }) if actual == field
        ));
    }
}

#[test]
fn create_rejects_a_package_over_the_default_open_limit_without_panicking() {
    let title = "T".repeat(16 * 1024 * 1024 + 1);
    assert!(matches!(
        Epub::create("urn:test", &title, "en", OffsetDateTime::UNIX_EPOCH),
        Err(EpubCreateError::PackageByteLimit { size, limit }) if size > limit
    ));
}

#[test]
fn created_publication_supports_edit_export_and_reopen() {
    let modified = OffsetDateTime::parse("2026-07-20T14:15:16+02:00", &Rfc3339).unwrap();
    let mut book = Epub::create("urn:uuid:new-book", "New Book", "en", modified).unwrap();
    assert!(
        book.resource(ResourceSelector::EpubNav)
            .unwrap()
            .utf8_text()
            .unwrap()
            .contains("<title>New Book</title>")
    );

    let chapter = ManifestItem::builder()
        .id(EpubString::new("chapter").unwrap())
        .href(EpubHref::try_new("text/chapter.xhtml").unwrap())
        .media_type(MediaType::try_from("application/xhtml+xml").unwrap())
        .build()
        .unwrap();
    let nav_point = NavigationPoint::builder()
        .label(EpubString::new("Chapter").unwrap())
        .href(EpubHref::try_new("text/chapter.xhtml").unwrap())
        .build()
        .unwrap();
    book.edit()
        .add_manifest_item(chapter)
        .unwrap()
        .upsert_resource(
            "EPUB/text/chapter.xhtml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Chapter</body></html>".to_vec(),
        )
        .unwrap()
        .add_spine_itemref(ItemRef::new("chapter").unwrap())
        .unwrap()
        .add_nav_point(InsertionTarget::List(ListSelector::Toc), nav_point)
        .unwrap()
        .preview()
        .unwrap()
        .commit();

    let archive = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    let reopened = EpubZip::from_reader(Cursor::new(archive))
        .unwrap()
        .default_rendition()
        .unwrap();

    assert_eq!(reopened.package().spine().itemrefs().len(), 1);
    assert_eq!(
        reopened
            .navigation()
            .epub_nav()
            .unwrap()
            .toc()
            .unwrap()
            .points()[0]
            .label()
            .unwrap()
            .as_str(),
        "Chapter"
    );
    assert_eq!(
        reopened
            .resource(ResourceSelector::path("EPUB/text/chapter.xhtml").unwrap())
            .unwrap()
            .utf8_text()
            .unwrap(),
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Chapter</body></html>"
    );
}
