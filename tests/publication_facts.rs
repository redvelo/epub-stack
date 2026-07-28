#![cfg(all(feature = "serde", feature = "specta"))]

use epub_stack::{
    Epub, NavigationHrefTargetFacts, NavigationLoadingOutcome, NavigationTargetOutcomeFacts,
    PublicationFacts,
    resource::{
        ProviderPresence,
        facts::{ManifestTargetFacts, ReadingOrderTargetFacts, SelectionFacts, SelectionSource},
        provider::MemoryResourceProvider,
    },
};
use serde_json::Value;

fn open(
    package: &str,
    entries: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
) -> PublicationFacts {
    let provider = MemoryResourceProvider::from_entries(
        std::iter::once(("EPUB/package.opf", package.as_bytes().to_vec())).chain(entries),
    )
    .unwrap();
    Epub::from_provider(provider, "EPUB/package.opf")
        .unwrap()
        .facts()
        .unwrap()
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
        facts.navigation_loading.epub_nav,
        NavigationLoadingOutcome::Malformed
    );
    assert_eq!(
        facts.navigation_loading.ncx,
        NavigationLoadingOutcome::Loaded
    );
    assert!(facts.navigation.ncx().is_some());
}

#[test]
fn navigation_loading_distinguishes_missing_and_unattempted_candidates() {
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="nav" href="missing.xhtml" media-type="application/xhtml+xml" properties="nav"/>
      </manifest><spine/></package>"#;
    let facts = open(package, []);

    assert_eq!(
        facts.navigation_loading.epub_nav,
        NavigationLoadingOutcome::MissingResource
    );
    assert_eq!(
        facts.navigation_loading.ncx,
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
            .navigation_targets
            .iter()
            .map(|target| target.point_path.as_slice())
            .collect::<Vec<_>>(),
        vec![&[0][..], &[0, 0][..], &[1][..]]
    );
    assert!(matches!(
        facts.navigation_targets[0].target,
        NavigationHrefTargetFacts::Address {
            resource: Some(_),
            outcome: NavigationTargetOutcomeFacts::Resolved,
            fragment: Some(ref fragment),
            ..
        } if fragment == "part"
    ));
    assert!(matches!(
        facts.navigation_targets[1].target,
        NavigationHrefTargetFacts::Address {
            resource: None,
            outcome: NavigationTargetOutcomeFacts::MissingResource,
            ..
        }
    ));
    assert!(matches!(
        facts.navigation_targets[2].target,
        NavigationHrefTargetFacts::Address {
            resource: None,
            outcome: NavigationTargetOutcomeFacts::UnindexedAddress,
            ..
        }
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
    <item id="broken" href="bad href" media-type="not a mime"/>
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

    assert_eq!(
        facts.package.manifest().items().len(),
        facts.resource_index.manifest_targets.len()
    );
    assert_eq!(
        facts.package.spine().itemrefs().len(),
        facts.resource_index.reading_order.len()
    );
    assert_eq!(facts.resource_index.resources[1].declarations.len(), 2);
    assert_eq!(facts.resource_index.reading_order.len(), 5);
    assert!(matches!(
        facts.resource_index.reading_order[0].target,
        ReadingOrderTargetFacts::AmbiguousManifestId { ref candidates } if candidates.len() == 2
    ));
    assert!(matches!(
        facts.resource_index.reading_order[1].target,
        ReadingOrderTargetFacts::AmbiguousManifestId { .. }
    ));
    assert!(matches!(
        facts.resource_index.reading_order[2].target,
        ReadingOrderTargetFacts::Declaration {
            resource: Some(_),
            ..
        }
    ));
    assert_eq!(
        facts.resource_index.reading_order[3].target,
        ReadingOrderTargetFacts::MissingManifestId
    );
    assert_eq!(
        facts.resource_index.reading_order[4].target,
        ReadingOrderTargetFacts::MissingIdref
    );
    assert_eq!(
        facts.resource_index.manifest_targets[4],
        ManifestTargetFacts::InvalidHref
    );
    assert_eq!(
        facts.resource_index.manifest_targets[5],
        ManifestTargetFacts::MissingHref
    );
    assert!(facts.resource_index.resources.iter().any(|resource| {
        resource.address.display_value() == "EPUB/missing.xhtml"
            && resource.presence == ProviderPresence::Missing
    }));
    assert!(facts.resource_index.resources.iter().any(|resource| {
        resource.address.display_value() == "https://example.test/book.css"
            && resource.presence == ProviderPresence::NotApplicable
    }));
    assert!(matches!(
        facts.resource_index.selections.epub_nav,
        SelectionFacts::AmbiguousCandidateDeclarations { ref candidates, .. }
            if candidates.len() == 2
    ));
    assert!(matches!(
        facts.resource_index.selections.cover,
        SelectionFacts::UnresolvedAuthoredId {
            source: SelectionSource::Opf2CoverMetadata,
            ..
        }
    ));

    let value = serde_json::to_value(&facts).unwrap();
    let encoded = serde_json::to_string(&facts).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    assert_eq!(value["packagePath"], "EPUB/package.opf");
    assert_eq!(
        value["resourceIndex"]["selections"]["cover"]["state"],
        "unresolved-authored-id"
    );
    assert_eq!(
        value["resourceIndex"]["selections"]["cover"]["authoredId"],
        "absent-cover"
    );
    assert_eq!(
        value["resourceIndex"]["resources"][2]["presence"],
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
        value["resourceIndex"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|resource| resource["providerMetadata"]["sizeBytes"].as_str())
            .all(|size| size.parse::<u64>().is_ok())
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
        facts.resource_index.selections.package,
        SelectionFacts::Selected {
            source: SelectionSource::PackagePath,
            declaration: None,
            resource: Some(_)
        }
    ));
    assert!(matches!(
        facts.resource_index.selections.cover,
        SelectionFacts::Selected {
            source: SelectionSource::Opf2CoverMetadata,
            declaration: Some(_),
            resource: Some(_)
        }
    ));
    assert!(matches!(
        facts.resource_index.selections.ncx,
        SelectionFacts::Selected {
            source: SelectionSource::SpineToc,
            declaration: Some(_),
            resource: Some(_)
        }
    ));
    assert_eq!(
        facts.resource_index.selections.epub_nav,
        SelectionFacts::Absent
    );
}

#[test]
fn publication_facts_exports_without_unsafe_u64_numbers() {
    let types = specta::Types::default().register::<PublicationFacts>();
    let output = specta_typescript::Typescript::default()
        .export(&types, specta_serde::Format)
        .unwrap();

    assert!(output.contains("export type PublicationFacts"));
    assert!(output.contains("sizeBytes: string | null"));
    assert!(output.contains("export type MediaType = EpubString"));
    assert!(!output.contains("sizeBytes: number"));
    assert!(!output.contains("bigint"));
}
