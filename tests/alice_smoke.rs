use epub_stack::EpubString;
use epub_stack::{
    Cfi, EpubPath, EpubZip,
    analysis::{inspection::InspectionData, reference::HrefTarget},
    cfi::ResolvedCfi,
    content::{ContentFacts, MediaFact},
    package::metadata::DcElement,
};
use std::path::Path;

#[test]
#[ignore = "requires the repository-only Alice EPUB fixture"]
fn alice_real_publication_runs_the_public_open_read_and_analysis_workflow() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/real/alice-in-wonderland.epub");
    assert!(
        fixture.is_file(),
        "required Alice smoke fixture is missing: {}",
        fixture.display()
    );

    let zip = EpubZip::open(&fixture).expect("Alice must be a readable ZIP publication");
    let book = zip
        .default_rendition()
        .expect("Alice must open as a semantic publication");

    assert_eq!(book.resources().package_path().as_str(), "epub/content.opf");
    assert!(
        book.package().metadata().elements(DcElement::Title)[0]
            .content()
            .map(EpubString::as_str)
            .is_some_and(|title| title.starts_with("Alice"))
    );
    assert_eq!(book.resources().reading_order().count(), 20);
    assert!(book.resources().resources().len() > 60);
    assert_eq!(
        book.navigation()
            .filter(|document| document.is_epub_nav())
            .expect("Alice declares EPUB NAV")
            .path()
            .as_str(),
        "epub/toc.xhtml"
    );

    let titlepage = book
        .resources()
        .declaration_by_id("titlepage.xhtml")
        .expect("Alice titlepage declaration")
        .resource()
        .expect("Alice titlepage declaration");
    let titlepage_key = titlepage.ordinal();
    let text = book
        .utf8_text(&EpubPath::new("epub/text/titlepage.xhtml").unwrap())
        .unwrap();
    assert!(text.contains("John Tenniel"));

    let foliate_cfi: Cfi = "epubcfi(/6/22!/4/2[chapter-6]/128,/1:0,/1:83)"
        .parse()
        .expect("Foliate chapter 6 CFI must parse");
    let ResolvedCfi::Range(foliate_range) = book
        .resolve_cfi(&foliate_cfi)
        .expect("Foliate chapter 6 CFI must resolve")
    else {
        panic!("Foliate CFI must be a range");
    };
    assert_eq!(
        foliate_range.text(),
        "“Oh, you can’t help that,” said the Cat: “we’re all mad here. I’m mad. You’re mad.”"
    );

    let analysis = book.analyze();
    assert!(
        analysis
            .resource(titlepage_key)
            .unwrap()
            .content()
            .is_complete()
    );

    let illustration = analysis
        .resources()
        .resource_by_path(
            &EpubPath::new("epub/images/illustration-2.svg")
                .expect("Alice illustration path must be canonical"),
        )
        .expect("Alice must contain illustration 2");
    let illustration_key = illustration.ordinal();
    let illustration_content = analysis.resource(illustration_key).unwrap().content();
    assert!(illustration_content.is_complete());
    assert!(
        illustration_content
            .value()
            .and_then(ContentFacts::as_svg)
            .is_some()
    );

    let illustration_inspection = analysis.resource(illustration_key).unwrap().inspection();
    assert!(illustration_inspection.is_complete());
    let InspectionData::Svg(svg) = illustration_inspection.value().unwrap().data() else {
        panic!("Alice illustration 2 must inspect as SVG");
    };
    assert_eq!(svg.view_box(), Some("0 0 489 764"));

    let chapter = analysis
        .resources()
        .resource_by_path(
            &EpubPath::new("epub/text/chapter-1.xhtml")
                .expect("Alice chapter path must be canonical"),
        )
        .expect("Alice must contain chapter 1");
    let authored_use = analysis
        .resource(chapter.ordinal())
        .unwrap()
        .xhtml_media()
        .find(|occurrence| {
            occurrence.references().any(|reference| {
                matches!(
                    reference.target(),
                    HrefTarget::Resource { resource, .. } if *resource == illustration_key
                )
            })
        })
        .expect("Alice chapter 1 must reference illustration 2");
    assert!(matches!(
        authored_use.fact(),
        MediaFact::Image { alt: Some(alt), .. }
            if alt == "A white rabbit wearing a waistcoat stands upright consulting a pocket-watch."
    ));

    assert!(analysis.coverage().content().is_complete());
    assert!(analysis.coverage().inspection().is_complete());
    assert!(analysis.coverage().fingerprints().is_complete());
    assert!(analysis.accessibility().claims().next().is_some());
    assert!(!analysis.coverage().relationships().is_empty());
}
