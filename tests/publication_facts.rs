#![cfg(all(feature = "serde", feature = "specta"))]

use epub_stack::{
    Epub, EpubPath,
    analysis::reference::{ManifestRole, ManifestTarget},
    navigation::facts::{NavigationLoadingOutcome, NavigationTarget},
    resource::{
        DeclarationTarget, IdrefTarget, ManifestOrdinal, ProviderPresence, ResourceSelection,
        SelectionSource, provider::MemoryResourceProvider,
    },
};
use serde_json::Value;

fn open(
    package: &str,
    entries: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
) -> Epub<MemoryResourceProvider> {
    let provider = MemoryResourceProvider::from_entries(
        std::iter::once(("EPUB/package.opf", package.as_bytes().to_vec())).chain(entries),
    )
    .unwrap();
    Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap()
}

/// The owned wire view a host composes from the borrowed models, with no clone of its own.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Facts<'a> {
    package: &'a epub_stack::package::Package,
    navigation: Option<&'a epub_stack::navigation::NavigationDocument>,
    resources: &'a epub_stack::resource::ResourceIndex,
    navigation_loading: epub_stack::navigation::facts::NavigationLoadingFacts,
    navigation_targets: Vec<epub_stack::navigation::facts::NavigationTargetFacts>,
}

fn facts_value(book: &Epub<MemoryResourceProvider>) -> Value {
    serde_json::to_value(facts_of(book)).unwrap()
}

fn facts_of(book: &Epub<MemoryResourceProvider>) -> Facts<'_> {
    Facts {
        package: book.package(),
        navigation: book.navigation(),
        resources: book.resources(),
        navigation_loading: book.navigation_loading(),
        navigation_targets: book.navigation_targets().unwrap(),
    }
}

#[test]
fn ordinal_edits_address_missing_identity_and_follow_staged_positions() {
    let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item media-type="application/octet-stream"/><item media-type="text/plain"/><item id="keep" href="keep.txt" media-type="text/plain"/></manifest><spine/></package>"#;
    let provider =
        MemoryResourceProvider::from_entries([("EPUB/package.opf", package.to_vec())]).unwrap();
    let mut book =
        Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
    let first = book.resources().declarations().next().unwrap().ordinal();
    let preview = book
        .edit()
        .remove_manifest_item(epub_stack::edit::select::ManifestItemSelector::Ordinal(
            first,
        ))
        .unwrap()
        .remove_manifest_item(epub_stack::edit::select::ManifestItemSelector::Ordinal(
            first,
        ))
        .unwrap()
        .preview()
        .unwrap();
    let staged = preview.package().clone();
    assert_eq!(staged.manifest().items().len(), 1);
    assert_eq!(staged.manifest().items()[0].id(), Some("keep"));
    preview.commit();
    assert_eq!(book.package(), &staged);
}

#[test]
fn navigation_base_agrees_between_snapshot_analysis_and_edit_preview() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
    let nav = br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><base href="text/"/></head><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml#one">Chapter</a></li></ol></nav></body></html>"#;
    let provider = MemoryResourceProvider::from_entries([
        ("EPUB/package.opf", package.as_bytes().to_vec()),
        ("EPUB/nav.xhtml", nav.to_vec()),
        ("EPUB/text/chapter.xhtml", br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p id="one">One</p></body></html>"#.to_vec()),
    ]).unwrap();
    let mut book =
        Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
    let targets = book.navigation_targets().unwrap();
    let chapter = book
        .resources()
        .declaration_by_id("chapter")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    assert!(
        matches!(&targets[0].target, NavigationTarget::Resource { resource, fragment: Some(fragment) } if *resource == chapter && fragment == "one")
    );
    let analysis = book.analyze();
    assert!(analysis.references().any(|reference| match reference {
        epub_stack::analysis::reference::AuthoredReference::Href(reference) => matches!(reference.context(), epub_stack::analysis::reference::ReferenceContext::Element(_)) && matches!(reference.target(), epub_stack::analysis::reference::HrefTarget::Fragment { resource, exists: Some(true), .. } if *resource == chapter),
        _ => false,
    }));
    let preview = book.edit().preview().unwrap();
    assert_eq!(preview.navigation_targets().unwrap(), targets);
    preview.commit();
    assert_eq!(book.navigation_targets().unwrap(), targets);
}

#[test]
fn navigation_image_labels_reach_facts_without_rewriting_source() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine></package>"#;
    let nav = br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><nav epub:type="toc" hidden=""><h1><img title="Contents"/></h1><ol><li><a href="chapter.xhtml#iv" epub:type="chapter" role="doc-chapter" hidden=""><span>Part <span><img alt="IV" title="Ignored"/></span> tail</span></a></li><li><a href=""><img alt="" title="Ignored"/></a></li></ol></nav></html>"#;
    let provider = MemoryResourceProvider::from_entries([
        ("EPUB/package.opf", package.as_bytes().to_vec()),
        ("EPUB/nav.xhtml", nav.to_vec()),
        ("EPUB/chapter.xhtml", b"chapter".to_vec()),
    ])
    .unwrap();
    let book = Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
    let facts = &book;
    assert_eq!(
        facts.navigation_loading().epub_nav,
        NavigationLoadingOutcome::Loaded
    );
    let list = facts
        .navigation()
        .as_ref()
        .filter(|document| document.is_epub_nav())
        .unwrap()
        .toc()
        .unwrap();
    assert_eq!(list.heading().unwrap().text(), "Contents");
    assert!(list.hidden());
    let point = &list.points()[0];
    assert_eq!(point.label().unwrap().as_str(), "Part IV tail");
    assert_eq!(point.authored_href().unwrap().as_str(), "chapter.xhtml#iv");
    assert!(point.hidden());
    assert_eq!(
        point
            .authored_semantic_tokens()
            .iter()
            .map(|token| token.as_str())
            .collect::<Vec<_>>(),
        vec!["chapter", "doc-chapter"]
    );
    assert_eq!(list.points()[1].label(), None);
    assert_eq!(list.points()[1].authored_href().unwrap().as_str(), "");
    assert!(
        matches!(facts.navigation_targets().unwrap()[0].target, NavigationTarget::Resource {
        fragment: Some(ref fragment), ..
    } if fragment == "iv")
    );
    assert_eq!(
        book.bytes(book.resources().epub_nav().unwrap().local_path().unwrap())
            .unwrap()
            .as_slice(),
        nav
    );
}

#[test]
fn navigation_loading_observes_nav_failure_and_ncx_fallback() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
        <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
      </manifest><spine toc="ncx"/></package>"#;
    let facts = open(
        package,
        [
            ("EPUB/nav.xhtml", b"not xml".to_vec()),
            (
                "EPUB/toc.ncx",
                br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap/></ncx>"#.to_vec(),
            ),
        ],
    );

    assert_eq!(
        facts.navigation_loading().epub_nav,
        NavigationLoadingOutcome::Malformed
    );
    assert_eq!(
        facts.navigation_loading().ncx,
        NavigationLoadingOutcome::Loaded
    );
    assert!(
        facts
            .navigation()
            .as_ref()
            .is_some_and(|document| document.is_ncx())
    );
}

#[test]
fn navigation_loading_distinguishes_missing_and_unattempted_candidates() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="nav" href="missing.xhtml" media-type="application/xhtml+xml" properties="nav"/>
      </manifest><spine/></package>"#;
    let facts = open(package, []);

    assert_eq!(
        facts.navigation_loading().epub_nav,
        NavigationLoadingOutcome::MissingResource
    );
    assert_eq!(
        facts.navigation_loading().ncx,
        NavigationLoadingOutcome::NotAttempted
    );
}

#[test]
fn facts_resolve_navigation_targets_in_depth_first_document_order() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/>
      <manifest>
        <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
      </manifest>
      <spine><itemref idref="chapter"/></spine>
    </package>"#;
    let nav = br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
      <head><title>Contents</title></head><body><nav epub:type="toc"><ol>
        <li><a href="chapter.xhtml#part">Chapter</a><ol><li><a href="missing.xhtml">Missing</a></li></ol></li>
        <li><a href="https://example.test/remote">Remote</a></li>
      </ol></nav></body></html>"#;
    let facts = open(
        package,
        [
            ("EPUB/nav.xhtml", nav.to_vec()),
            ("EPUB/chapter.xhtml", b"chapter".to_vec()),
        ],
    );

    assert_eq!(
        facts
            .navigation_targets()
            .unwrap()
            .iter()
            .map(|target| target.point_path.as_slice())
            .collect::<Vec<_>>(),
        vec![&[0][..], &[0, 0][..], &[1][..]]
    );
    assert!(matches!(
        facts.navigation_targets().unwrap()[0].target,
        NavigationTarget::Resource {
            fragment: Some(ref fragment),
            ..
        } if fragment == "part"
    ));
    assert!(matches!(
        facts.navigation_targets().unwrap()[1].target,
        NavigationTarget::MissingResource { .. }
    ));
    assert!(matches!(
        facts.navigation_targets().unwrap()[2].target,
        NavigationTarget::UnindexedAddress { .. }
    ));
}

#[test]
fn facts_preserve_alignment_duplicates_states_and_canonical_json() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
  <metadata><dc:title>T</dc:title><meta name="cover" content="absent-cover"/></metadata>
  <manifest>
    <item id="same" href="shared.xhtml" media-type="Application/XHTML+XML; Profile=Authored" properties="nav custom:unknown"/>
    <item id="same" href="shared.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="missing" href="missing.xhtml" media-type="text/plain"/>
    <item id="remote" href="https://example.test/book.css" media-type="text/css"/>
    <item id="broken" href="bad%href" media-type="not a mime"/>
    <item id="nohref" media-type="text/plain"/>
  </manifest>
  <spine>
    <itemref idref="same"/><itemref idref="same"/>
    <itemref idref="missing"/><itemref idref="unknown"/><itemref/>
  </spine>
</package>"#;
    let facts = open(
        package,
        [
            ("EPUB/shared.xhtml", b"shared".to_vec()),
            ("EPUB/provider-only.bin", b"provider".to_vec()),
        ],
    );

    let index = &facts.resources();
    assert_eq!(
        facts.package().manifest().items().len(),
        index.declarations().len()
    );
    assert_eq!(
        facts.package().spine().itemrefs().len(),
        index.reading_order().len()
    );
    assert_eq!(index.resources().nth(1).unwrap().declarations().len(), 2);
    let order = index.reading_order().collect::<Vec<_>>();
    assert_eq!(order.len(), 5);
    let ambiguous = [ManifestOrdinal::new(0), ManifestOrdinal::new(1)];
    assert_eq!(
        order[0].target(),
        Some(&IdrefTarget::Ambiguous(ambiguous.to_vec()))
    );
    assert_eq!(
        order[1].target(),
        Some(&IdrefTarget::Ambiguous(ambiguous.to_vec()))
    );
    assert!(order[2].resource().is_some());
    assert_eq!(order[3].target(), Some(&IdrefTarget::Missing));
    assert_eq!(order[4].target(), None);
    let declarations = index.declarations().collect::<Vec<_>>();
    assert_eq!(declarations[4].target(), DeclarationTarget::InvalidHref);
    assert_eq!(declarations[5].target(), DeclarationTarget::MissingHref);
    assert!(index.resources().any(|resource| {
        resource.address().display_value() == "EPUB/missing.xhtml"
            && resource.presence() == ProviderPresence::Missing
    }));
    assert!(index.resources().any(|resource| {
        resource.address().display_value() == "https://example.test/book.css"
            && resource.presence() == ProviderPresence::NotApplicable
    }));
    assert!(matches!(
        index.selections().epub_nav,
        ResourceSelection::Ambiguous { ref candidates, .. } if candidates.len() == 2
    ));
    assert!(matches!(
        index.selections().cover,
        ResourceSelection::UnresolvedAuthoredId {
            source: SelectionSource::Opf2CoverMetadata,
            ..
        }
    ));

    let value = facts_value(&facts);
    let encoded = serde_json::to_string(&facts_of(&facts)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    assert_eq!(
        value["resources"]["resources"][0]["address"]["value"],
        "EPUB/package.opf"
    );
    assert_eq!(
        value["resources"]["selections"]["cover"]["state"],
        "unresolved-authored-id"
    );
    assert_eq!(
        value["resources"]["selections"]["cover"]["authoredId"],
        "absent-cover"
    );
    assert_eq!(
        value["resources"]["resources"][2]["presence"]["state"],
        "missing"
    );
    assert_eq!(
        value["package"]["manifest"]["items"][0]["mediaType"],
        "Application/XHTML+XML; Profile=Authored"
    );
    assert_eq!(
        value["package"]["manifest"]["items"][0]["properties"][1]["raw"],
        "custom:unknown"
    );
    assert!(
        value["resources"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|resource| resource["presence"]["sizeBytes"].as_str())
            .all(|size| size.parse::<u64>().is_ok())
    );
}

#[test]
fn manifest_fallback_facts_are_aligned_and_preserve_declaration_resolution() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata/><manifest>
    <item id="start" href="shared.xhtml" media-type="application/example" fallback=" middle "/>
    <item id="middle" href="shared.xhtml" media-type="application/other" fallback="end"/>
    <item id="end" href="end.xhtml" media-type="application/final"/>
    <item id="invalid-edge" href="invalid.bin" media-type="application/octet-stream" fallback="1bad"/>
    <item id="missing-edge" href="missing.bin" media-type="application/octet-stream" fallback="unknown"/>
    <item id="ambiguous-edge" href="ambiguous.bin" media-type="application/octet-stream" fallback="dup"/>
    <item id="dup" href="duplicate.xhtml" media-type="application/one"/>
    <item id="dup" href="./duplicate.xhtml" media-type="application/two"/>
    <item id="self" href="self.bin" media-type="application/octet-stream" fallback="self"/>
    <item id="cycle-a" href="a.bin" media-type="application/octet-stream" fallback="cycle-b"/>
    <item id="cycle-b" href="b.bin" media-type="application/octet-stream" fallback="cycle-a"/>
    <item id="prefix" href="prefix.bin" media-type="application/octet-stream" fallback="cycle-a"/>
  </manifest><spine/></package>"#;
    let facts = open(package, []);
    let index = &facts.resources();
    let fallbacks = index
        .declarations()
        .map(|declaration| declaration.fallback_target().cloned())
        .collect::<Vec<_>>();
    let declaration = |position| index.declarations().nth(position).unwrap().ordinal();

    assert_eq!(
        fallbacks,
        [
            Some(IdrefTarget::Declaration(declaration(1))),
            Some(IdrefTarget::Declaration(declaration(2))),
            None,
            Some(IdrefTarget::Invalid),
            Some(IdrefTarget::Missing),
            Some(IdrefTarget::Ambiguous(vec![declaration(6), declaration(7)])),
            None,
            None,
            Some(IdrefTarget::Declaration(declaration(8))),
            Some(IdrefTarget::Declaration(declaration(10))),
            Some(IdrefTarget::Declaration(declaration(9))),
            Some(IdrefTarget::Declaration(declaration(9))),
        ]
    );
    assert_eq!(
        index
            .resources()
            .filter(|resource| resource.declarations().len() == 2)
            .count(),
        2
    );
    assert_eq!(
        facts_value(&facts)["resources"]["declarations"][5]["fallbackTarget"],
        serde_json::json!({ "state": "ambiguous" })
    );
}

#[test]
fn manifest_idref_resolution_agrees_between_fallback_facts_and_analysis() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata/><manifest>
    <item id="empty" href="empty.bin" media-type="application/octet-stream" fallback=""/>
    <item id="xml-space-only" href="space.bin" media-type="application/octet-stream" fallback=" &#x9;&#xA;&#xD; "/>
    <item id="non-xml-space" href="nbsp.bin" media-type="application/octet-stream" fallback="&#xA0;target&#xA0;"/>
    <item id="unicode-edge" href="unicode.bin" media-type="application/octet-stream" fallback="Δelta"/>
    <item id="malformed" href="malformed.bin" media-type="application/octet-stream" fallback="1bad"/>
    <item id="missing" href="missing.bin" media-type="application/octet-stream" fallback="unknown"/>
    <item id="ambiguous" href="ambiguous.bin" media-type="application/octet-stream" fallback="dup"/>
    <item id="missing-href-edge" href="edge.bin" media-type="application/octet-stream" fallback="no-href"/>
    <item id="invalid-href-edge" href="edge2.bin" media-type="application/octet-stream" fallback="invalid-href"/>
    <item id="xml-space" href="trim.bin" media-type="application/octet-stream" fallback=" &#x9;target&#xA; "/>
    <item id="target" href="target.bin" media-type="application/octet-stream"/>
    <item id="Δelta" href="unicode-target.bin" media-type="application/octet-stream"/>
    <item id="no-href" media-type="application/octet-stream"/>
    <item id="invalid-href" href="bad%href" media-type="application/octet-stream"/>
    <item id="dup" href="one.bin" media-type="application/octet-stream"/>
    <item id="dup" href="two.bin" media-type="application/octet-stream"/>
  </manifest><spine/></package>"#;
    let provider =
        MemoryResourceProvider::from_entries([("EPUB/package.opf", package.as_bytes().to_vec())])
            .unwrap();
    let epub = Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
    let facts = &epub;
    let analysis = epub.analyze();
    let declarations = facts.resources().declarations().collect::<Vec<_>>();
    let declaration = |position: usize| declarations[position].ordinal();
    let expected = [
        IdrefTarget::Invalid,
        IdrefTarget::Invalid,
        IdrefTarget::Invalid,
        IdrefTarget::Declaration(declaration(11)),
        IdrefTarget::Invalid,
        IdrefTarget::Missing,
        IdrefTarget::Ambiguous(vec![declaration(14), declaration(15)]),
        IdrefTarget::Declaration(declaration(12)),
        IdrefTarget::Declaration(declaration(13)),
        IdrefTarget::Declaration(declaration(10)),
    ];

    for (position, expected_fact) in expected.iter().enumerate() {
        let ordinal = declaration(position);
        assert_eq!(
            declarations[position].fallback_target(),
            Some(expected_fact)
        );
        let reference = analysis
            .declaration(ordinal)
            .unwrap()
            .references()
            .find(|reference| reference.role() == ManifestRole::Fallback)
            .unwrap();
        match (expected_fact, reference.target()) {
            (IdrefTarget::Invalid, ManifestTarget::InvalidManifestIdref)
            | (IdrefTarget::Missing, ManifestTarget::Missing)
            | (IdrefTarget::Ambiguous(_), ManifestTarget::Ambiguous { .. }) => {}
            (
                IdrefTarget::Declaration(declaration),
                ManifestTarget::Declaration {
                    declaration: analyzed,
                    resource,
                },
            ) => {
                assert_eq!(analyzed, declaration);
                if matches!(declaration.index(), 12 | 13) {
                    assert_eq!(*resource, None);
                }
            }
            pair => panic!("fallback facts and analysis disagree: {pair:?}"),
        }
    }
}

#[test]
fn manifest_fallback_serialization_is_linear_for_duplicate_ids() {
    fn encoded_len(count: usize) -> usize {
        let mut package = String::from(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest>"#,
        );
        for index in 0..count {
            package.push_str(&format!(
                r#"<item id="source{index}" href="source{index}.bin" media-type="application/octet-stream" fallback="dup"/>"#
            ));
        }
        for index in 0..count {
            package.push_str(&format!(
                r#"<item id="dup" href="duplicate{index}.bin" media-type="application/octet-stream"/>"#
            ));
        }
        package.push_str("</manifest><spine/></package>");
        let facts = open(&package, []);
        let encoded = serde_json::to_string(&facts.resources()).unwrap();
        assert!(!encoded.contains("candidates"));
        encoded.len()
    }

    let small = encoded_len(64);
    let large = encoded_len(256);
    assert!(
        large <= small * 5,
        "fallback facts grew non-linearly: {small} -> {large}"
    );
}

#[test]
fn facts_report_selected_and_absent_structural_resources() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata><meta name="cover" content="cover"/></metadata>
  <manifest>
    <item id="cover" href="cover.jpg" media-type="image/jpeg"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
  </manifest>
  <spine toc="ncx"/>
</package>"#;
    let facts = open(package, []);

    assert!(matches!(
        facts.resources().selections().package,
        ResourceSelection::Selected {
            source: SelectionSource::PackagePath,
            declaration: None,
            resource: Some(_)
        }
    ));
    assert!(matches!(
        facts.resources().selections().cover,
        ResourceSelection::Selected {
            source: SelectionSource::Opf2CoverMetadata,
            declaration: Some(_),
            resource: Some(_)
        }
    ));
    assert!(matches!(
        facts.resources().selections().ncx,
        ResourceSelection::Selected {
            source: SelectionSource::SpineToc,
            declaration: Some(_),
            resource: Some(_)
        }
    ));
    assert_eq!(
        facts.resources().selections().epub_nav,
        ResourceSelection::Absent
    );
}

#[test]
fn publication_facts_exports_without_unsafe_u64_numbers() {
    let mut types = specta::Types::default();
    types = types.register::<epub_stack::resource::ResourceIndex>();
    types = types.register::<epub_stack::package::Package>();
    types = types.register::<epub_stack::navigation::NavigationDocument>();
    types = types.register::<epub_stack::navigation::facts::NavigationTargetFacts>();
    types = types.register::<epub_stack::navigation::facts::NavigationLoadingFacts>();
    let output = specta_typescript::Typescript::default()
        .export(&types, specta_serde::Format)
        .unwrap();

    assert!(output.contains("export type IdrefTarget"));
    assert!(output.contains("export type ResourceIndex"));
    assert!(output.contains("sizeBytes: string | null"));
    assert!(output.contains("export type MediaType = EpubString"));
    assert!(!output.contains("sizeBytes: number"));
    assert!(!output.contains("bigint"));
}

#[test]
fn serialized_resources_carry_declaration_predicates_for_facts_consumers() {
    let facts = open(
        r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata/>
            <manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml" properties="scripted"/>
                <item id="style" href="style.css" media-type="text/css"/>
                <item id="art" href="art.svg" media-type="image/svg+xml"/>
            </manifest>
            <spine><itemref idref="page"/></spine>
        </package>"#,
        [
            ("EPUB/page.xhtml", b"<html><body/></html>".to_vec()),
            ("EPUB/style.css", b"p{}".to_vec()),
            ("EPUB/art.svg", b"<svg/>".to_vec()),
        ],
    );

    let value = facts_value(&facts);
    let resources = value["resources"]["resources"].as_array().unwrap();
    let find = |suffix: &str| {
        resources
            .iter()
            .find(|resource| {
                resource["address"]["value"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(suffix))
            })
            .unwrap()
    };

    let page = find("page.xhtml");
    assert_eq!(page["hasXhtmlDeclaration"], Value::Bool(true));
    assert_eq!(page["hasStylesheetDeclaration"], Value::Bool(false));
    assert_eq!(page["hasSvgDeclaration"], Value::Bool(false));
    assert_eq!(page["scripted"], Value::Bool(true));

    assert_eq!(
        find("style.css")["hasStylesheetDeclaration"],
        Value::Bool(true)
    );
    assert_eq!(find("art.svg")["hasSvgDeclaration"], Value::Bool(true));
    assert_eq!(find("art.svg")["scripted"], Value::Bool(false));
}

#[test]
fn manifest_id_normalization_is_available_to_facts_consumers() {
    use epub_stack::package::normalize_manifest_id;

    assert_eq!(normalize_manifest_id("  chapter \n"), Ok("chapter"));
    assert_eq!(normalize_manifest_id("chapter"), Ok("chapter"));
    assert!(normalize_manifest_id("1chapter").is_err());
    assert!(normalize_manifest_id("has space").is_err());
    assert_eq!(
        normalize_manifest_id("has space").unwrap_err().value(),
        "has space"
    );
}

#[test]
fn serialized_facts_carry_the_joins_and_authored_fields_a_consumer_cannot_recompute() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
  <metadata><dc:title>T</dc:title></metadata>
  <manifest>
    <item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="broken" href="bad%href" media-type="text/plain" fallback="chapter"/>
    <item media-type="text/plain"/>
  </manifest>
  <spine><itemref idref="chapter"/><itemref idref="absent"/></spine>
</package>"#;
    let facts = open(package, [("EPUB/text/chapter.xhtml", b"chapter".to_vec())]);
    let encoded = serde_json::to_value(facts.resources()).unwrap();

    let occurrence = &encoded["readingOrder"][0];
    let chapter = facts
        .resources()
        .declaration_by_id("chapter")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    assert_eq!(occurrence["resource"], Value::from(chapter.as_u32()));
    assert_eq!(occurrence["idref"], Value::from("chapter"));
    assert_eq!(occurrence["linear"], Value::from("yes"));
    assert_eq!(encoded["readingOrder"][1]["resource"], Value::Null);

    let declarations = &encoded["declarations"];
    assert_eq!(
        declarations[0]["id"],
        serde_json::json!({"state": "valid", "value": "chapter"})
    );
    assert_eq!(declarations[0]["href"], Value::from("text/chapter.xhtml"));
    assert_eq!(
        declarations[0]["mediaType"],
        Value::from("application/xhtml+xml")
    );
    assert_eq!(declarations[1]["fallback"], Value::from("chapter"));
    assert_eq!(
        declarations[2]["id"],
        serde_json::json!({"state": "missing"})
    );

    assert_eq!(
        declarations[1]["target"],
        serde_json::json!({"state": "invalid-href"})
    );
    assert_eq!(declarations[1]["href"], Value::from("bad%href"));

    assert_eq!(
        encoded["selections"]["package"]["resource"],
        Value::from(facts.resources().package().ordinal().as_u32())
    );
    assert_eq!(
        facts.resources().selections().package.resource(),
        Some(facts.resources().package().ordinal())
    );
}

#[test]
fn serialized_index_carries_the_package_path_without_walking_the_selection() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
  <metadata><dc:title>T</dc:title></metadata>
  <manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#;
    let facts = open(package, [("EPUB/text/chapter.xhtml", b"chapter".to_vec())]);
    let encoded = serde_json::to_value(facts.resources()).unwrap();

    assert_eq!(encoded["packagePath"], Value::from("EPUB/package.opf"));
    assert_eq!(
        encoded["packagePath"],
        encoded["resources"][facts.resources().package().ordinal().index()]["address"]["value"]
    );
    assert_eq!(
        facts.resources().package_path().as_str(),
        encoded["packagePath"].as_str().unwrap()
    );
}

#[test]
fn ambiguous_idref_candidates_are_live_only_and_recomputable_from_serialized_ids() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
  <metadata><dc:title>T</dc:title></metadata>
  <manifest>
    <item id="dup" href="one.xhtml" media-type="application/xhtml+xml"/>
    <item id="dup" href="two.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="dup"/></spine>
</package>"#;
    let facts = open(
        package,
        [
            ("EPUB/one.xhtml", b"one".to_vec()),
            ("EPUB/two.xhtml", b"two".to_vec()),
        ],
    );

    let occurrence = facts.resources().reading_order().next().unwrap();
    assert_eq!(
        occurrence.target().map(IdrefTarget::candidates),
        Some([ManifestOrdinal::new(0), ManifestOrdinal::new(1)].as_slice())
    );

    let encoded = serde_json::to_value(facts.resources()).unwrap();
    let serialized = &encoded["readingOrder"][0];
    assert_eq!(
        serialized["target"],
        serde_json::json!({"state": "ambiguous"})
    );
    let idref = serialized["idref"].as_str().unwrap();
    let recomputed = encoded["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, declaration)| declaration["id"]["value"] == *idref)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    assert_eq!(recomputed, [0, 1]);
}

#[test]
fn serialized_index_carries_the_resolved_cover_path() {
    let facts = open(
        r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
            <metadata><dc:title>T</dc:title></metadata>
            <manifest>
                <item id="cover" href="images/cover.png" media-type="image/png" properties="cover-image"/>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
            </manifest>
            <spine><itemref idref="chapter"/></spine>
        </package>"#,
        [
            ("EPUB/images/cover.png", b"png".to_vec()),
            ("EPUB/chapter.xhtml", b"chapter".to_vec()),
        ],
    );
    let encoded = facts_value(&facts);

    assert_eq!(encoded["resources"]["coverPath"], "EPUB/images/cover.png");
    assert_eq!(
        facts.resources().cover_path(),
        facts
            .resources()
            .cover_image()
            .and_then(|resource| resource.local_path())
    );

    let without = open(
        r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
            <metadata><dc:title>T</dc:title></metadata>
            <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="chapter"/></spine>
        </package>"#,
        [("EPUB/chapter.xhtml", b"chapter".to_vec())],
    );
    assert_eq!(without.resources().cover_path(), None);
    assert_eq!(facts_value(&without)["resources"]["coverPath"], Value::Null);

    // A declared cover without provider bytes is not a readable path.
    let missing = open(
        r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
            <metadata><dc:title>T</dc:title></metadata>
            <manifest>
                <item id="cover" href="images/cover.png" media-type="image/png" properties="cover-image"/>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
            </manifest>
            <spine><itemref idref="chapter"/></spine>
        </package>"#,
        [("EPUB/chapter.xhtml", b"chapter".to_vec())],
    );
    assert!(missing.resources().cover_image().is_some());
    assert_eq!(missing.resources().cover_path(), None);
}

#[test]
fn serialized_facts_round_trip_and_rebuild_derived_lookups() {
    let facts = open(
        r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0">
            <metadata><dc:title>T</dc:title></metadata>
            <manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="chapter"/></spine>
        </package>"#,
        [("EPUB/text/chapter.xhtml", b"chapter".to_vec())],
    );
    let encoded = serde_json::to_value(facts.resources()).unwrap();
    let decoded: epub_stack::resource::ResourceIndex = serde_json::from_value(encoded).unwrap();

    assert_eq!(&decoded, facts.resources());

    // An ordinal that points outside the inventory would panic the borrowed views.
    let mut malformed = serde_json::to_value(facts.resources()).unwrap();
    malformed["declarations"][0]["target"] =
        serde_json::json!({"state": "resource", "resource": 99});
    assert!(serde_json::from_value::<epub_stack::resource::ResourceIndex>(malformed).is_err());

    // An ambiguous outcome encodes without its candidates, so it must decode without them too.
    let ambiguous: epub_stack::resource::IdrefTarget =
        serde_json::from_str(r#"{"state":"ambiguous"}"#).unwrap();
    assert!(matches!(
        ambiguous,
        epub_stack::resource::IdrefTarget::Ambiguous(ref candidates) if candidates.is_empty()
    ));
    assert_eq!(decoded.package_path(), facts.resources().package_path());
    // The lookup tables are not encoded, so a decoded index must rebuild them to answer queries.
    let chapter = decoded.declaration_by_id("chapter").unwrap();
    assert_eq!(chapter.href().unwrap().as_str(), "text/chapter.xhtml");
    assert_eq!(
        decoded
            .resource_by_path(&EpubPath::new("EPUB/text/chapter.xhtml").unwrap())
            .map(|resource| resource.ordinal()),
        chapter.resource().map(|resource| resource.ordinal())
    );
}

#[test]
fn deserialization_rejects_values_the_constructors_would_reject() {
    use epub_stack::package::metadata::MetaPropertyToken;

    assert!(serde_json::from_str::<EpubPath>("\"../escape\"").is_err());
    assert!(serde_json::from_str::<EpubPath>("\"/absolute\"").is_err());
    assert!(serde_json::from_str::<epub_stack::EpubString>("\"   \"").is_err());
    assert!(serde_json::from_str::<epub_stack::resource::MediaType>("\"\"").is_err());

    // Derived predicates are recomputed from the declarations, never taken from the payload.
    let index: epub_stack::resource::ResourceIndex = serde_json::from_str(
        r#"{
            "packagePath": "EPUB/package.opf",
            "resources": [{
                "address": {"kind": "local", "value": "EPUB/page.xhtml"},
                "presence": {"state": "present", "sizeBytes": "1"},
                "declarations": [0],
                "hasXhtmlDeclaration": false,
                "hasStylesheetDeclaration": true,
                "hasSvgDeclaration": true,
                "scripted": true
            }],
            "declarations": [{
                "id": {"state": "valid", "value": "page"},
                "href": "page.xhtml",
                "target": {"state": "resource", "resource": 0},
                "mediaType": "application/xhtml+xml",
                "fallback": null,
                "fallbackTarget": null
            }],
            "readingOrder": [],
            "selections": {"package": {"state": "absent"}, "cover": {"state": "absent"}, "epubNav": {"state": "absent"}, "ncx": {"state": "absent"}}
        }"#,
    )
    .unwrap();
    let page = index.resources().next().unwrap();
    assert!(page.has_xhtml_declaration());
    assert!(!page.is_scripted());

    // A token's known value is derived from its spelling, never taken from the encoded pair.
    let token: MetaPropertyToken =
        serde_json::from_str(r#"{"raw":"not-a-known-property","known":"rendition:layout"}"#)
            .unwrap();
    assert_eq!(token.as_str(), "not-a-known-property");
    assert_eq!(token.known_value(), None);
}
