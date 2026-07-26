use epub_stack::container::{ExportError, Rootfile};
use epub_stack::edit::{
    EditChange, StructuralEditKind,
    navigation::{PointMatch, PointSelector},
    package::SpineItemRefSelector,
};
use epub_stack::package::{
    EpubVersion, RenditionLayout,
    manifest::ManifestItem,
    spine::{ItemRef, KnownSpineProperty},
};
use epub_stack::resource::provider::{
    ProviderReadError, ReadResult, ResourceProvider, ResourceProviderIndex,
    ResourceProviderIndexError, ResourceProviderIndexLimits,
};
use epub_stack::resource::{
    EpubHref, MediaType, ProviderPresence, ReadingOrderTarget, ResourceLookupError,
};
use epub_stack::semantics::EpubString;
use epub_stack::{
    Epub, EpubOpenFailure, EpubOpenLimits, EpubPath, EpubZip, MemoryResourceProvider,
    ResourceSelector,
};
use std::cell::Cell;
use std::io::{Cursor, Read, Seek, Write};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const CONTAINER: &str = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
const PACKAGE: &str = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>Lifecycle</dc:title><dc:identifier id="uid">urn:test</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="style" href="style.css" media-type="text/css"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
const NAV: &str = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
const CHAPTER: &str = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="style.css"/></head><body><p id="p1">Chapter</p></body></html>"#;
const STYLE: &str = "body { color: black; }";
const PRESENTATION_PACKAGE: &str = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata><meta property="rendition:layout">reflowable</meta></metadata>
<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="style" href="style.css" media-type="text/css"/></manifest>
<spine><itemref idref="chapter"/></spine></package>"#;

const OPF2_CONTAINER: &str = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
const OPF2_PACKAGE: &str = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
<metadata><dc:title>Migration</dc:title><dc:identifier id="uid">urn:migration</dc:identifier><dc:language>en</dc:language><meta name="cover" content="cover"/></metadata>
<manifest>
<item id="ncx" href="navigation/toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/>
<item id="afterword" href="text/afterword.xhtml" media-type="application/xhtml+xml"/>
<item id="cover" href="images/cover.jpg" media-type="image/jpeg"/>
</manifest>
<spine toc="ncx"><itemref idref="chapter"/><itemref idref="afterword"/></spine>
<guide><reference type="text" title="Start" href="text/chapter.xhtml#start"/><reference type="cover" title="Cover" href="images/cover.jpg"/></guide>
</package>"#;
const OPF2_NCX: &str = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="../text/chapter.xhtml?edition=deluxe&amp;view=full#section-1"/></navPoint></navMap><navList class="landmarks"><navLabel><text>Extras</text></navLabel><navTarget><navLabel><text>Glossary</text></navLabel><content src="../appendix/glossary.xhtml#terms"/></navTarget></navList></ncx>"#;

fn entries(package: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("mimetype", b"application/epub+zip".to_vec()),
        ("META-INF/container.xml", CONTAINER.as_bytes().to_vec()),
        ("EPUB/package.opf", package.to_vec()),
        ("EPUB/nav.xhtml", NAV.as_bytes().to_vec()),
        ("EPUB/chapter.xhtml", CHAPTER.as_bytes().to_vec()),
        ("EPUB/style.css", STYLE.as_bytes().to_vec()),
    ]
}

fn provider(package: &[u8]) -> MemoryResourceProvider {
    MemoryResourceProvider::from_entries(entries(package)).unwrap()
}

fn archive(package: &[u8]) -> Cursor<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut output);
        for (path, bytes) in entries(package) {
            let options = if path == "mimetype" {
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
            } else {
                SimpleFileOptions::default()
            };
            zip.start_file(path, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    output.set_position(0);
    output
}

fn opf2_migration_provider() -> MemoryResourceProvider {
    MemoryResourceProvider::from_entries([
        ("mimetype", b"application/epub+zip".to_vec()),
        ("META-INF/container.xml", OPF2_CONTAINER.as_bytes().to_vec()),
        ("OPS/package.opf", OPF2_PACKAGE.as_bytes().to_vec()),
        ("OPS/navigation/toc.ncx", OPF2_NCX.as_bytes().to_vec()),
        (
            "OPS/text/chapter.xhtml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><p id=\"start\">Chapter</p><p id=\"section-1\">Section</p></body></html>".to_vec(),
        ),
        (
            "OPS/text/afterword.xhtml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Afterword</body></html>".to_vec(),
        ),
        ("OPS/images/cover.jpg", b"cover".to_vec()),
    ])
    .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct PublicationSnapshot {
    package_path: String,
    version: Option<EpubVersion>,
    resources: Vec<(String, ProviderPresence)>,
    reading_order: Vec<String>,
    has_epub_nav: bool,
}

fn snapshot<R: ResourceProvider>(book: &Epub<R>) -> PublicationSnapshot {
    let resources = book
        .resources()
        .resources()
        .iter()
        .map(|resource| {
            (
                resource.address().display_value().to_string(),
                resource.presence(),
            )
        })
        .collect();
    let reading_order = book
        .reading_order()
        .map(|entry| match entry.target() {
            ReadingOrderTarget::Declaration {
                resource: Some(key),
                ..
            } => book
                .resources()
                .resource(*key)
                .unwrap()
                .address()
                .display_value()
                .to_string(),
            target => format!("{target:?}"),
        })
        .collect();
    PublicationSnapshot {
        package_path: book.package_path().as_str().to_string(),
        version: book.package().version(),
        resources,
        reading_order,
        has_epub_nav: book.navigation().epub_nav().is_some(),
    }
}

fn assert_opf2_migration_state<R: ResourceProvider>(book: &Epub<R>) {
    assert_eq!(book.package_path().as_str(), "OPS/package.opf");
    assert_eq!(book.package().version(), Some(EpubVersion::Three));
    assert!(book.package().guide().is_none());
    assert!(book.package().spine().toc().is_none());
    assert!(book.package().manifest_item_by_id("ncx").is_none());

    let nav_item = book.package().nav_item().unwrap();
    assert_eq!(nav_item.id().unwrap().as_str(), "nav");
    assert_eq!(nav_item.authored_href().unwrap().as_str(), "nav.xhtml");
    assert!(book.package().ncx_item().is_none());

    let nav = book.navigation().epub_nav().unwrap();
    assert_eq!(nav.path().as_str(), "OPS/nav.xhtml");
    assert!(book.navigation().ncx().is_none());
    let toc = nav.toc().unwrap();
    assert_eq!(toc.points().len(), 1);
    assert_eq!(toc.points()[0].label().unwrap().as_str(), "Chapter");
    assert_eq!(
        toc.points()[0].authored_href().unwrap().as_str(),
        "text/chapter.xhtml?edition=deluxe&view=full#section-1"
    );

    let landmarks = nav.landmarks().unwrap();
    assert_eq!(
        landmarks
            .points()
            .iter()
            .map(|point| (
                point.label().unwrap().as_str(),
                point.authored_href().unwrap().as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("Start", "text/chapter.xhtml#start"),
            ("Cover", "images/cover.jpg"),
        ]
    );

    let auxiliary = nav.auxiliary_lists().collect::<Vec<_>>();
    assert_eq!(auxiliary.len(), 1);
    assert!(auxiliary[0].semantic().is_none());
    assert_eq!(auxiliary[0].points().len(), 1);
    assert_eq!(
        auxiliary[0].points()[0].label().unwrap().as_str(),
        "Glossary"
    );
    assert_eq!(
        auxiliary[0].points()[0].authored_href().unwrap().as_str(),
        "appendix/glossary.xhtml#terms"
    );

    assert_eq!(
        book.reading_order()
            .map(|entry| match entry.target() {
                ReadingOrderTarget::Declaration {
                    resource: Some(key),
                    ..
                } => book
                    .resources()
                    .resource(*key)
                    .unwrap()
                    .address()
                    .display_value(),
                target => panic!("unexpected reading-order target: {target:?}"),
            })
            .collect::<Vec<_>>(),
        ["OPS/text/chapter.xhtml", "OPS/text/afterword.xhtml"]
    );
    let mut manifest_resources = book
        .resources()
        .manifest_resources()
        .map(|resource| resource.address().display_value())
        .collect::<Vec<_>>();
    manifest_resources.sort_unstable();
    assert_eq!(
        manifest_resources,
        [
            "OPS/images/cover.jpg",
            "OPS/nav.xhtml",
            "OPS/text/afterword.xhtml",
            "OPS/text/chapter.xhtml",
        ]
    );
    assert!(book.resource(ResourceSelector::EpubNav).is_ok());
    assert!(matches!(
        book.resource(ResourceSelector::path("OPS/navigation/toc.ncx").unwrap()),
        Err(ResourceLookupError::NotFound(_))
    ));
}

fn assert_provider_conformance<R: ResourceProvider>(provider: &R) {
    let limits = ResourceProviderIndexLimits::default();
    let index = provider.index(&limits).unwrap();
    let paths = index
        .entries()
        .iter()
        .map(|entry| entry.path().as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "EPUB/chapter.xhtml",
            "EPUB/nav.xhtml",
            "EPUB/package.opf",
            "EPUB/style.css",
            "META-INF/container.xml",
            "mimetype",
        ]
    );

    let chapter = EpubPath::new("EPUB/chapter.xhtml").unwrap();
    assert_eq!(provider.read(&chapter).unwrap(), CHAPTER.as_bytes());
    assert_eq!(
        index.get(&chapter).unwrap().size_bytes(),
        Some(CHAPTER.len() as u64)
    );

    let calls = Cell::new(0);
    let result = provider
        .read_with(&chapter, |_| {
            calls.set(calls.get() + 1);
            Err::<(), _>("domain failure")
        })
        .unwrap();
    assert_eq!(result, Err("domain failure"));

    let missing = EpubPath::new("EPUB/missing.xhtml").unwrap();
    assert!(matches!(
        provider.read_with(&missing, |_| calls.set(calls.get() + 1)),
        Err(ProviderReadError::MissingResource { path }) if path == missing
    ));
    assert_eq!(calls.get(), 1);

    let total_path_bytes = index
        .entries()
        .iter()
        .map(|entry| entry.path().as_str().len())
        .sum();
    let exact = ResourceProviderIndexLimits::new(index.entries().len(), total_path_bytes).unwrap();
    assert!(provider.index(&exact).is_ok());
    let count_limit =
        ResourceProviderIndexLimits::new(index.entries().len() - 1, total_path_bytes).unwrap();
    assert!(matches!(
        provider.index(&count_limit),
        Err(ResourceProviderIndexError::EntryCountLimitExceeded { .. })
    ));
}

#[test]
fn memory_resource_provider_conforms_to_resource_provider_contract() {
    assert_provider_conformance(&provider(PACKAGE.as_bytes()));
}

#[test]
fn epub_zip_conforms_to_resource_provider_contract() {
    assert_provider_conformance(&EpubZip::from_reader(archive(PACKAGE.as_bytes())).unwrap());
}

#[test]
fn failed_provider_open_can_be_repaired_and_retried() {
    let error = Epub::from_provider(provider(b"<broken"), "EPUB/package.opf").unwrap_err();
    let (failure, mut provider) = error.into_parts();
    assert!(
        matches!(failure, EpubOpenFailure::PackageParse { ref path, .. } if path.as_str() == "EPUB/package.opf")
    );
    provider
        .insert(
            EpubPath::new("EPUB/package.opf").unwrap(),
            PACKAGE.as_bytes().to_vec(),
        )
        .unwrap();

    let repaired = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
    assert_eq!(
        repaired
            .resource(ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
            .unwrap()
            .utf8_text()
            .unwrap(),
        CHAPTER
    );
}

#[test]
fn no_op_export_reopen_preserves_normalized_publication_snapshot() {
    let book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let before = snapshot(&book);
    let chapter_before = book
        .resource(ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
        .unwrap()
        .bytes()
        .unwrap();
    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();

    let mut zip = ZipArchive::new(Cursor::new(output.as_slice())).unwrap();
    let mut mimetype = zip.by_index(0).unwrap();
    assert_eq!(mimetype.name(), "mimetype");
    assert_eq!(mimetype.compression(), CompressionMethod::Stored);
    let mut mimetype_bytes = Vec::new();
    mimetype.read_to_end(&mut mimetype_bytes).unwrap();
    assert_eq!(mimetype_bytes, b"application/epub+zip");
    drop(mimetype);
    drop(zip);

    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert_eq!(snapshot(&reopened), before);
    assert_eq!(
        reopened
            .resource(ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
            .unwrap()
            .bytes()
            .unwrap(),
        chapter_before
    );
}

#[test]
fn edit_preview_commit_export_reopen_preserves_effective_state() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let selector = ResourceSelector::path("EPUB/chapter.xhtml").unwrap();
    let replacement = b"<html><body>Edited chapter</body></html>".to_vec();

    let preview = book
        .edit()
        .replace_resource(selector.clone(), replacement.clone())
        .unwrap()
        .preview()
        .unwrap();
    assert_eq!(preview.changes().len(), 1);
    let preview_package = preview.package().clone();
    let preview_navigation = preview.navigation().clone();
    let preview_resources = preview.resources().clone();
    let preview_changes = preview.changes().to_vec();
    let report = preview.commit();
    assert_eq!(report.changes(), preview_changes);
    assert_eq!(book.package(), &preview_package);
    assert_eq!(book.navigation(), &preview_navigation);
    assert_eq!(book.resources(), &preview_resources);
    assert_eq!(
        book.resource(selector.clone()).unwrap().bytes().unwrap(),
        replacement
    );

    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert_eq!(
        reopened.resource(selector).unwrap().bytes().unwrap(),
        replacement
    );
    assert!(reopened.navigation().epub_nav().is_some());
}

#[test]
fn reading_order_presentation_tracks_preview_commit_and_detached_analysis() {
    let mut book = Epub::from_provider(
        provider(PRESENTATION_PACKAGE.as_bytes()),
        "EPUB/package.opf",
    )
    .unwrap();
    let analysis_before = book.analyze();
    assert_eq!(
        analysis_before
            .resources()
            .reading_order()
            .next()
            .unwrap()
            .presentation()
            .layout()
            .value(),
        Some(&RenditionLayout::Reflowable)
    );

    let replacement = ItemRef::new("chapter")
        .unwrap()
        .with_property(KnownSpineProperty::RenditionLayoutPrePaginated);
    let preview = book
        .edit()
        .replace_spine_itemref(SpineItemRefSelector::index(0), replacement)
        .unwrap()
        .preview()
        .unwrap();
    assert_eq!(
        preview
            .resources()
            .reading_order()
            .next()
            .unwrap()
            .presentation()
            .layout()
            .value(),
        Some(&RenditionLayout::PrePaginated)
    );

    preview.commit();
    assert_eq!(
        book.reading_order()
            .next()
            .unwrap()
            .presentation()
            .layout()
            .value(),
        Some(&RenditionLayout::PrePaginated)
    );
    assert_eq!(
        analysis_before
            .resources()
            .reading_order()
            .next()
            .unwrap()
            .presentation()
            .layout()
            .value(),
        Some(&RenditionLayout::Reflowable)
    );
}

#[test]
fn edit_changes_are_coalesced_by_path_in_canonical_order() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();

    let preview = book
        .edit()
        .upsert_resource("EPUB/z.txt", b"old".to_vec())
        .unwrap()
        .upsert_resource("EPUB/a.txt", b"a".to_vec())
        .unwrap()
        .upsert_resource("EPUB/z.txt", b"final".to_vec())
        .unwrap()
        .preview()
        .unwrap();

    assert_eq!(
        preview.changes(),
        [
            EditChange::UpsertResource {
                path: EpubPath::new("EPUB/a.txt").unwrap(),
                size_bytes: 1,
            },
            EditChange::UpsertResource {
                path: EpubPath::new("EPUB/z.txt").unwrap(),
                size_bytes: 5,
            },
        ]
    );
    let expected = preview.changes().to_vec();
    assert_eq!(preview.commit().changes(), expected);
}

#[test]
fn repeated_package_edits_report_one_final_structural_rewrite() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let first = ManifestItem::builder()
        .id(EpubString::try_new("first").unwrap())
        .href(EpubHref::try_new("first.bin").unwrap())
        .media_type(MediaType::try_from("application/octet-stream").unwrap())
        .build();
    let second = ManifestItem::builder()
        .id(EpubString::try_new("second").unwrap())
        .href(EpubHref::try_new("second.bin").unwrap())
        .media_type(MediaType::try_from("application/octet-stream").unwrap())
        .build();

    let preview = book
        .edit()
        .add_manifest_item(first)
        .unwrap()
        .add_manifest_item(second)
        .unwrap()
        .preview()
        .unwrap();

    assert!(matches!(
        preview.changes(),
        [EditChange::RewriteStructuralResource {
            path,
            kind: StructuralEditKind::Package,
            ..
        }] if path.as_str() == "EPUB/package.opf"
    ));
}

#[test]
fn export_includes_pending_changes_accumulated_across_commits() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    book.edit()
        .upsert_resource("EPUB/added.bin", b"added".to_vec())
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    book.edit()
        .replace_resource(
            ResourceSelector::path("EPUB/chapter.xhtml").unwrap(),
            b"replaced".to_vec(),
        )
        .unwrap()
        .remove_resource(ResourceSelector::path("EPUB/style.css").unwrap())
        .unwrap()
        .preview()
        .unwrap()
        .commit();

    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    let mut zip = ZipArchive::new(Cursor::new(output.as_slice())).unwrap();
    assert!(zip.by_name("EPUB/added.bin").is_ok());
    assert!(zip.by_name("EPUB/style.css").is_err());
    drop(zip);

    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert_eq!(
        reopened
            .resource(ResourceSelector::path("EPUB/added.bin").unwrap())
            .unwrap()
            .bytes()
            .unwrap(),
        b"added"
    );
    assert_eq!(
        reopened
            .resource(ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
            .unwrap()
            .bytes()
            .unwrap(),
        b"replaced"
    );
}

#[test]
fn consecutive_semantic_commits_read_prior_structural_overlay_bytes() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let first = ManifestItem::builder()
        .id(EpubString::try_new("first").unwrap())
        .href(EpubHref::try_new("first.bin").unwrap())
        .media_type(MediaType::try_from("application/octet-stream").unwrap())
        .build();
    let second = ManifestItem::builder()
        .id(EpubString::try_new("second").unwrap())
        .href(EpubHref::try_new("second.bin").unwrap())
        .media_type(MediaType::try_from("application/octet-stream").unwrap())
        .build();

    book.edit()
        .add_manifest_item(first)
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    book.edit()
        .add_manifest_item(second)
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    book.edit()
        .set_nav_point_label(
            PointSelector::toc(PointMatch::path(vec![0])),
            EpubString::try_new("Renamed").unwrap(),
        )
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    book.edit()
        .set_nav_point_href(
            PointSelector::toc(PointMatch::path(vec![0])),
            EpubHref::try_new("chapter.xhtml#updated").unwrap(),
        )
        .unwrap()
        .preview()
        .unwrap()
        .commit();

    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert!(reopened.package().manifest_item_by_id("first").is_some());
    assert!(reopened.package().manifest_item_by_id("second").is_some());
    let point = &reopened
        .navigation()
        .epub_nav()
        .unwrap()
        .toc()
        .unwrap()
        .points()[0];
    assert_eq!(point.label().unwrap().as_str(), "Renamed");
    assert_eq!(
        point.authored_href().unwrap().as_str(),
        "chapter.xhtml#updated"
    );
}

#[test]
fn successful_preview_is_isolated_until_commit() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let package_before = book.package().clone();
    let navigation_before = book.navigation().clone();
    let resources_before = book.resources().clone();
    let replacement = b"<html><body>Preview only</body></html>".to_vec();
    let bytes_before = resources_before
        .resources()
        .iter()
        .map(|resource| {
            let path = resource.address().local_path().unwrap().clone();
            let bytes = book.resource(path.clone()).unwrap().bytes().unwrap();
            (path, bytes)
        })
        .collect::<Vec<_>>();

    {
        let preview = book
            .edit()
            .replace_resource(
                ResourceSelector::path("EPUB/chapter.xhtml").unwrap(),
                replacement.clone(),
            )
            .unwrap()
            .preview()
            .unwrap();
        assert_eq!(preview.changes().len(), 1);
        assert_eq!(preview.package(), &package_before);
        assert_eq!(preview.navigation(), &navigation_before);
        assert_eq!(
            preview
                .resources()
                .select(&ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
                .unwrap()
                .metadata()
                .size_bytes(),
            Some(replacement.len() as u64)
        );
    }

    assert_eq!(book.package(), &package_before);
    assert_eq!(book.navigation(), &navigation_before);
    assert_eq!(book.resources(), &resources_before);
    for (path, bytes) in bytes_before {
        assert_eq!(
            book.resource(ResourceSelector::path(path).unwrap())
                .unwrap()
                .bytes()
                .unwrap(),
            bytes
        );
    }
}

#[test]
fn opf2_migration_preview_commit_export_reopen_preserves_semantics() {
    let mut book = Epub::from_provider(opf2_migration_provider(), "OPS/package.opf").unwrap();

    let preview = book
        .edit()
        .migrate_opf2_to_epub3()
        .unwrap()
        .preview()
        .unwrap();
    assert_eq!(preview.package().version(), Some(EpubVersion::Three));
    assert_eq!(
        preview.navigation().epub_nav().unwrap().path().as_str(),
        "OPS/nav.xhtml"
    );
    assert!(preview.navigation().ncx().is_none());
    assert!(preview.package().manifest_item_by_id("ncx").is_none());
    assert!(
        preview
            .resources()
            .manifest_resources()
            .all(|resource| resource.address().display_value() != "OPS/navigation/toc.ncx")
    );

    let preview_package = preview.package().clone();
    let preview_navigation = preview.navigation().clone();
    let preview_resources = preview.resources().clone();
    let preview_changes = preview.changes().to_vec();
    let report = preview.commit();
    assert_eq!(report.changes(), preview_changes);
    assert_eq!(book.package(), &preview_package);
    assert_eq!(book.navigation(), &preview_navigation);
    assert_eq!(book.resources(), &preview_resources);
    assert_opf2_migration_state(&book);

    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert_opf2_migration_state(&reopened);
}

#[test]
fn failed_preview_does_not_mutate_live_publication() {
    let base = provider(PACKAGE.as_bytes());
    let limits = EpubOpenLimits::default().with_provider_index_limits(
        ResourceProviderIndexLimits::new(base.entries().count(), usize::MAX).unwrap(),
    );
    let mut book = Epub::from_provider_with_limits(base, "EPUB/package.opf", limits).unwrap();
    assert!(
        book.edit()
            .upsert_resource("EPUB/extra.xhtml", b"extra".to_vec())
            .unwrap()
            .preview()
            .is_err()
    );
    assert!(matches!(
        book.resource(ResourceSelector::path("EPUB/extra.xhtml").unwrap()),
        Err(ResourceLookupError::NotFound(_))
    ));
    assert_eq!(
        book.resource(ResourceSelector::path("EPUB/chapter.xhtml").unwrap())
            .unwrap()
            .utf8_text()
            .unwrap(),
        CHAPTER
    );
}

#[test]
fn into_base_provider_explicitly_discards_committed_overlay() {
    let mut book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    let selector = ResourceSelector::path("EPUB/chapter.xhtml").unwrap();
    book.edit()
        .replace_resource(selector.clone(), b"replacement".to_vec())
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    assert_eq!(
        book.resource(selector).unwrap().bytes().unwrap(),
        b"replacement"
    );

    let provider = book.into_base_provider();
    assert_eq!(
        provider
            .read(&EpubPath::new("EPUB/chapter.xhtml").unwrap())
            .unwrap(),
        CHAPTER.as_bytes()
    );
}

#[test]
fn failed_zip_open_can_be_repaired_exported_and_reopened() {
    let error = EpubZip::from_reader(archive(b"<broken"))
        .unwrap()
        .default_rendition()
        .unwrap_err();
    assert!(matches!(
        error.failure(),
        EpubOpenFailure::PackageParse { .. }
    ));

    let mut provider = error.into_provider();
    provider.upsert_entry(
        EpubPath::new("EPUB/package.opf").unwrap(),
        PACKAGE.as_bytes().to_vec(),
    );
    let repaired = provider.default_rendition().unwrap();
    let output = repaired
        .export(Cursor::new(Vec::new()))
        .unwrap()
        .into_inner();
    let reopened = EpubZip::from_reader(Cursor::new(output))
        .unwrap()
        .default_rendition()
        .unwrap();
    assert_eq!(snapshot(&reopened), snapshot(&repaired));
}

#[test]
fn generated_container_supersedes_removal_and_limits_use_the_logical_view() {
    let mut provider = EpubZip::from_reader(archive(PACKAGE.as_bytes())).unwrap();
    provider.remove_entry(EpubPath::new("META-INF/container.xml").unwrap());
    provider.clear_rootfiles();
    provider.add_rootfile(Rootfile::new("EPUB/package.opf").unwrap());
    provider.remove_entry(EpubPath::new("EPUB/style.css").unwrap());

    let limits = ResourceProviderIndexLimits::new(5, usize::MAX).unwrap();
    let index = provider.index(&limits).unwrap();
    assert!(
        index
            .get(&EpubPath::new("META-INF/container.xml").unwrap())
            .is_some()
    );
    assert!(
        index
            .get(&EpubPath::new("EPUB/style.css").unwrap())
            .is_none()
    );

    let book = provider.default_rendition().unwrap();
    let output = book.export(Cursor::new(Vec::new())).unwrap().into_inner();
    assert!(
        EpubZip::from_reader(Cursor::new(output))
            .unwrap()
            .default_rendition()
            .is_ok()
    );
}

struct FailingResourceReader;

impl Read for FailingResourceReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("provider stream failed"))
    }
}

struct StreamingFailureProvider(MemoryResourceProvider);

impl ResourceProvider for StreamingFailureProvider {
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> ReadResult<T> {
        if path.as_str() == "EPUB/chapter.xhtml" {
            return Ok(read(&mut FailingResourceReader));
        }
        self.0.read_with(path, read)
    }

    fn index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> Result<ResourceProviderIndex, ResourceProviderIndexError> {
        self.0.index(limits)
    }
}

struct AcquisitionFailureProvider(MemoryResourceProvider);

impl ResourceProvider for AcquisitionFailureProvider {
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> ReadResult<T> {
        if path.as_str() == "EPUB/chapter.xhtml" {
            return Err(ProviderReadError::MissingResource { path: path.clone() });
        }
        self.0.read_with(path, read)
    }

    fn index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> Result<ResourceProviderIndex, ResourceProviderIndexError> {
        self.0.index(limits)
    }
}

#[test]
fn export_distinguishes_provider_acquisition_and_stream_failures() {
    let acquisition = Epub::from_provider(
        AcquisitionFailureProvider(provider(PACKAGE.as_bytes())),
        "EPUB/package.opf",
    )
    .unwrap();
    assert!(matches!(
        acquisition.export(Cursor::new(Vec::new())),
        Err(ExportError::ProviderRead { path, .. }) if path.as_str() == "EPUB/chapter.xhtml"
    ));

    let streaming = Epub::from_provider(
        StreamingFailureProvider(provider(PACKAGE.as_bytes())),
        "EPUB/package.opf",
    )
    .unwrap();
    assert!(matches!(
        streaming.export(Cursor::new(Vec::new())),
        Err(ExportError::ProviderStream { path, .. }) if path.as_str() == "EPUB/chapter.xhtml"
    ));
}

struct FailingWriter(Cursor<Vec<u8>>);

impl Write for FailingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer == b"application/epub+zip" {
            Err(std::io::Error::other("writer failed"))
        } else {
            self.0.write(buffer)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for FailingWriter {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(position)
    }
}

#[test]
fn export_distinguishes_output_path_and_stream_failures() {
    let book = Epub::from_provider(provider(PACKAGE.as_bytes()), "EPUB/package.opf").unwrap();
    assert!(matches!(
        book.export(FailingWriter(Cursor::new(Vec::new()))),
        Err(ExportError::OutputStream { .. })
    ));

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir()
        .join(format!("epub-stack-missing-{unique}"))
        .join("book.epub");
    assert!(matches!(
        book.export_to_path(path),
        Err(ExportError::OutputPath { .. })
    ));
}
