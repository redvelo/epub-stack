#![cfg(all(feature = "serde", feature = "specta"))]

use epub_stack::{
    Epub, NavigationHrefTargetFacts, NavigationLoadingOutcome, NavigationTargetOutcomeFacts,
    PublicationFacts,
    analysis::reference::{ManifestRole, ManifestTarget},
    resource::{
        ManifestOrdinal, ProviderPresence,
        facts::{
            ManifestFallbackDeclaration, ManifestFallbackFacts, ManifestFallbackTargetFacts,
            ManifestFallbackTopologyError, ManifestFallbackTraversal,
            ManifestFallbackTraversalOutcome, ManifestFallbackUnresolvedReason,
            ManifestFallbackValidationError, ManifestTargetFacts, ReadingOrderTargetFacts,
            SelectionFacts, SelectionSource,
        },
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
    let fallbacks = &facts.resource_index.manifest_fallbacks;
    let declarations = facts
        .package
        .manifest()
        .items()
        .iter()
        .map(|item| ManifestFallbackDeclaration::new(item.id(), item.fallback()))
        .collect::<Vec<_>>();

    assert_eq!(facts.package.manifest().items().len(), fallbacks.len());
    assert_eq!(fallbacks.validate_against(&declarations), Ok(()));
    let expected = [
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(1),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(2),
        },
        ManifestFallbackTargetFacts::Absent,
        ManifestFallbackTargetFacts::InvalidManifestIdref,
        ManifestFallbackTargetFacts::MissingManifestId,
        ManifestFallbackTargetFacts::AmbiguousManifestId,
        ManifestFallbackTargetFacts::Absent,
        ManifestFallbackTargetFacts::Absent,
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(8),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(10),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(9),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(9),
        },
    ];
    for (index, expected) in expected.iter().enumerate() {
        assert_eq!(fallbacks.get(ManifestOrdinal(index as u32)), Some(expected));
    }
    assert_eq!(
        facts
            .resource_index
            .resources
            .iter()
            .filter(|resource| resource.declarations.len() == 2)
            .count(),
        2
    );

    let chain = fallbacks.traverse(ManifestOrdinal(0)).unwrap();
    assert_eq!(
        chain.declarations,
        vec![ManifestOrdinal(0), ManifestOrdinal(1), ManifestOrdinal(2)]
    );
    assert_eq!(chain.outcome, ManifestFallbackTraversalOutcome::End);

    for (ordinal, expected) in [
        (3, ManifestFallbackUnresolvedReason::InvalidManifestIdref),
        (4, ManifestFallbackUnresolvedReason::MissingManifestId),
        (5, ManifestFallbackUnresolvedReason::AmbiguousManifestId),
    ] {
        let traversal = fallbacks.traverse(ManifestOrdinal(ordinal)).unwrap();
        assert_eq!(traversal.declarations, vec![ManifestOrdinal(ordinal)]);
        assert_eq!(
            traversal.outcome,
            ManifestFallbackTraversalOutcome::UnresolvedReference {
                declaration: ManifestOrdinal(ordinal),
                reason: expected,
            }
        );
    }

    let self_cycle = fallbacks.traverse(ManifestOrdinal(8)).unwrap();
    assert_eq!(self_cycle.declarations, vec![ManifestOrdinal(8)]);
    assert_eq!(
        self_cycle.outcome,
        ManifestFallbackTraversalOutcome::Cycle {
            repeated_declaration: ManifestOrdinal(8),
        }
    );

    let multi_node_cycle = fallbacks.traverse(ManifestOrdinal(9)).unwrap();
    assert_eq!(
        multi_node_cycle.declarations,
        vec![ManifestOrdinal(9), ManifestOrdinal(10)]
    );
    assert_eq!(
        multi_node_cycle.outcome,
        ManifestFallbackTraversalOutcome::Cycle {
            repeated_declaration: ManifestOrdinal(9),
        }
    );

    let prefixed_cycle = fallbacks.traverse(ManifestOrdinal(11)).unwrap();
    assert_eq!(
        prefixed_cycle.declarations,
        vec![ManifestOrdinal(11), ManifestOrdinal(9), ManifestOrdinal(10)]
    );
    assert_eq!(
        prefixed_cycle.outcome,
        ManifestFallbackTraversalOutcome::Cycle {
            repeated_declaration: ManifestOrdinal(9),
        }
    );
}

#[test]
fn manifest_fallback_validation_recomputes_all_authored_resolution_states() {
    let declarations = [
        ManifestFallbackDeclaration::new(Some(" start "), Some(" target ")),
        ManifestFallbackDeclaration::new(Some("target"), None),
        ManifestFallbackDeclaration::new(Some("dup"), Some("1bad")),
        ManifestFallbackDeclaration::new(Some("dup"), Some("missing")),
        ManifestFallbackDeclaration::new(Some("last"), Some("dup")),
    ];
    let valid: ManifestFallbackFacts = serde_json::from_value(serde_json::json!([
        { "state": "declaration", "declaration": 1 },
        { "state": "absent" },
        { "state": "invalid-manifest-idref" },
        { "state": "missing-manifest-id" },
        { "state": "ambiguous-manifest-id" }
    ]))
    .unwrap();
    assert_eq!(valid.validate_against(&declarations), Ok(()));

    let changed: ManifestFallbackFacts = serde_json::from_value(serde_json::json!([
        { "state": "declaration", "declaration": 0 },
        { "state": "absent" },
        { "state": "invalid-manifest-idref" },
        { "state": "missing-manifest-id" },
        { "state": "ambiguous-manifest-id" }
    ]))
    .unwrap();
    assert_eq!(
        changed.validate_against(&declarations),
        Err(ManifestFallbackValidationError::TargetMismatch {
            declaration: ManifestOrdinal(0)
        })
    );
}

#[test]
fn manifest_fallback_traversal_reports_out_of_range_topology() {
    let fallbacks: ManifestFallbackFacts = serde_json::from_value(serde_json::json!([
        { "state": "declaration", "declaration": 7 }
    ]))
    .unwrap();

    assert_eq!(
        fallbacks.traverse(ManifestOrdinal(0)),
        Err(ManifestFallbackTopologyError::EdgeOutOfRange {
            declaration: ManifestOrdinal(0),
            target: ManifestOrdinal(7),
        })
    );
    assert_eq!(
        fallbacks.traverse(ManifestOrdinal(9)),
        Err(ManifestFallbackTopologyError::StartOutOfRange {
            start: ManifestOrdinal(9),
        })
    );
}

#[test]
fn manifest_fallback_facts_round_trip_with_exact_array_shape_and_reject_unknown_fields() {
    let fallbacks: ManifestFallbackFacts = serde_json::from_value(serde_json::json!([
        { "state": "declaration", "declaration": 1 },
        { "state": "absent" }
    ]))
    .unwrap();
    let encoded = serde_json::to_value(&fallbacks).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!([
            { "state": "declaration", "declaration": 1 },
            { "state": "absent" }
        ])
    );
    let decoded: ManifestFallbackFacts = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, fallbacks);

    let traversal = decoded.traverse(ManifestOrdinal(0)).unwrap();
    assert_eq!(
        traversal,
        ManifestFallbackTraversal {
            declarations: vec![ManifestOrdinal(0), ManifestOrdinal(1)],
            outcome: ManifestFallbackTraversalOutcome::End,
        }
    );
    let traversal_json = serde_json::to_value(&traversal).unwrap();
    assert_eq!(
        traversal_json,
        serde_json::json!({
            "declarations": [0, 1],
            "outcome": { "state": "end" }
        })
    );
    assert!(
        serde_json::from_value::<ManifestFallbackFacts>(serde_json::json!([
            { "state": "absent", "unexpected": true }
        ]))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ManifestFallbackFacts>(serde_json::json!([
            { "state": "declaration", "declaration": 0, "unexpected": true }
        ]))
        .is_err()
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
    <item id="invalid-href" href="bad href" media-type="application/octet-stream"/>
    <item id="dup" href="one.bin" media-type="application/octet-stream"/>
    <item id="dup" href="two.bin" media-type="application/octet-stream"/>
  </manifest><spine/></package>"#;
    let provider =
        MemoryResourceProvider::from_entries([("EPUB/package.opf", package.as_bytes().to_vec())])
            .unwrap();
    let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
    let facts = epub.facts().unwrap();
    let analysis = epub.analyze();
    let expected = [
        ManifestFallbackTargetFacts::InvalidManifestIdref,
        ManifestFallbackTargetFacts::InvalidManifestIdref,
        ManifestFallbackTargetFacts::InvalidManifestIdref,
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(11),
        },
        ManifestFallbackTargetFacts::InvalidManifestIdref,
        ManifestFallbackTargetFacts::MissingManifestId,
        ManifestFallbackTargetFacts::AmbiguousManifestId,
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(12),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(13),
        },
        ManifestFallbackTargetFacts::Declaration {
            declaration: ManifestOrdinal(10),
        },
    ];

    for (index, expected_fact) in expected.iter().enumerate() {
        let ordinal = ManifestOrdinal(index as u32);
        assert_eq!(
            facts.resource_index.manifest_fallbacks.get(ordinal),
            Some(expected_fact)
        );
        let reference = analysis
            .references_from_declaration(ordinal)
            .unwrap()
            .find(|reference| reference.role() == ManifestRole::Fallback)
            .unwrap();
        match (expected_fact, reference.target()) {
            (
                ManifestFallbackTargetFacts::InvalidManifestIdref,
                ManifestTarget::InvalidManifestIdref,
            )
            | (ManifestFallbackTargetFacts::MissingManifestId, ManifestTarget::Missing)
            | (
                ManifestFallbackTargetFacts::AmbiguousManifestId,
                ManifestTarget::Ambiguous { .. },
            ) => {}
            (
                ManifestFallbackTargetFacts::Declaration { declaration },
                ManifestTarget::Declaration {
                    declaration: analyzed,
                    resource,
                },
            ) => {
                assert_eq!(analyzed, declaration);
                if matches!(declaration.0, 12 | 13) {
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
        let encoded = serde_json::to_string(&facts.resource_index.manifest_fallbacks).unwrap();
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
    let types = specta::Types::default()
        .register::<PublicationFacts>()
        .register::<ManifestFallbackTraversal>();
    let output = specta_typescript::Typescript::default()
        .export(&types, specta_serde::Format)
        .unwrap();

    assert!(output.contains("export type PublicationFacts"));
    assert!(output.contains("export type ManifestFallbackTargetFacts"));
    assert!(output.contains("export type ManifestFallbackTraversalOutcome"));
    assert!(output.contains("sizeBytes: string | null"));
    assert!(output.contains("export type MediaType = EpubString"));
    assert!(!output.contains("sizeBytes: number"));
    assert!(!output.contains("bigint"));
}
