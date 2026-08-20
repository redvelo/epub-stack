use epub_stack::content::text::TextChunkKind;
use epub_stack::{
    AnalysisLimits, Epub, ForegroundPreparationEligibility, MemoryResourceProvider,
    ResourceSelector,
    accessibility::{AccessibilityFact, AccessibilityObservationRef},
    analysis::{
        AnalysisIssue,
        coverage::{CoverageState, RelationshipSource, ResourceCoverage},
        dependency::{Root, RootError},
        fingerprint::{Blake3Hash, DuplicateGroup},
        reference::{HrefRole, HrefTarget, ManifestTarget, ReferenceContext},
    },
    content::{ContentFacts, FormFact, MediaFact, MediaSourceContext, ScriptFact},
    media_overlay::SmilNodeFact,
    semantics::TextDirection,
};
use std::collections::HashSet;

fn publication(
    package: &[u8],
    resources: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
) -> Epub<MemoryResourceProvider> {
    let provider = MemoryResourceProvider::from_entries(
        std::iter::once(("EPUB/package.opf", package.to_vec())).chain(resources),
    )
    .unwrap();
    Epub::from_provider(provider, "EPUB/package.opf").unwrap()
}

fn foreground_preparation_state(
    media_type: &str,
    document: &str,
    limits: AnalysisLimits,
) -> ForegroundPreparationEligibility {
    let package = format!(
        r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="{media_type}"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
    );
    let book = publication(
        package.as_bytes(),
        [("EPUB/chapter.xhtml", document.as_bytes().to_vec())],
    );
    let analysis = book.analyze_with_limits(limits);
    let ordinal = analysis
        .resources()
        .resources()
        .find(|resource| {
            resource
                .local_path()
                .is_some_and(|path| path.as_str() == "EPUB/chapter.xhtml")
        })
        .unwrap()
        .ordinal();
    analysis
        .facts_for(ordinal)
        .unwrap()
        .foreground_preparation_eligibility()
}

#[test]
fn foreground_preparation_requires_complete_static_xhtml() {
    let unlimited = || AnalysisLimits::new(None, None, None, None);
    let static_document = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="book.css"/></head><body><img src="cover.jpg"/><p>Static</p></body></html>"#;
    assert_eq!(
        foreground_preparation_state("application/xhtml+xml", static_document, unlimited()),
        ForegroundPreparationEligibility::Eligible
    );
    assert_eq!(
        foreground_preparation_state(
            "application/xhtml+xml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><script type="application/ld+json">{}</script></body></html>"#,
            unlimited(),
        ),
        ForegroundPreparationEligibility::Ineligible
    );
    assert_eq!(
        foreground_preparation_state(
            "application/xhtml+xml",
            "<html><body><p>broken</body></html>",
            unlimited(),
        ),
        ForegroundPreparationEligibility::Unknown
    );
    assert_eq!(
        foreground_preparation_state(
            "application/xhtml+xml",
            static_document,
            AnalysisLimits::new(None, Some(8), None, None),
        ),
        ForegroundPreparationEligibility::Unknown
    );
    assert_eq!(
        foreground_preparation_state(
            "image/svg+xml",
            r#"<svg xmlns="http://www.w3.org/2000/svg"><text>Static</text></svg>"#,
            unlimited(),
        ),
        ForegroundPreparationEligibility::Ineligible
    );
}

#[test]
fn foreground_preparation_excludes_active_and_early_lifecycle_constructs() {
    let constructs = [
        "<audio/>",
        "<video/>",
        "<source/>",
        "<track/>",
        "<iframe/>",
        "<object/>",
        "<embed/>",
        "<input autofocus=\"autofocus\"/>",
        "<meta http-equiv=\"refresh\" content=\"0\"/>",
        "<link rel=\"preload\" href=\"image.png\"/>",
        "<link rel=\"prefetch\" href=\"next.xhtml\"/>",
        "<link rel=\"modulepreload\" href=\"module.js\"/>",
        "<link rel=\"preconnect\" href=\"https://example.com\"/>",
        "<link rel=\"dns-prefetch\" href=\"//example.com\"/>",
        "<link rel=\"prerender\" href=\"next.xhtml\"/>",
        "<script>run()</script>",
        "<script type=\"speculationrules\">{}</script>",
        "<p onclick=\"run()\">Event</p>",
    ];
    for construct in constructs {
        let document = format!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head>{construct}</head><body><p>Text</p></body></html>"#
        );
        assert_eq!(
            foreground_preparation_state(
                "application/xhtml+xml",
                &document,
                AnalysisLimits::new(None, None, None, None),
            ),
            ForegroundPreparationEligibility::Ineligible,
            "construct should be excluded: {construct}"
        );
    }
}

#[test]
fn foreground_preparation_requires_supported_authored_element_namespaces() {
    let state = |document: &str| {
        foreground_preparation_state(
            "application/xhtml+xml",
            document,
            AnalysisLimits::new(None, None, None, None),
        )
    };
    assert_eq!(
        state(r#"<html xmlns="urn:not-xhtml"><body><p>Text</p></body></html>"#),
        ForegroundPreparationEligibility::Unknown
    );
    assert_eq!(
        state(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><custom xmlns="urn:unsupported">Text</custom></body></html>"#
        ),
        ForegroundPreparationEligibility::Unknown
    );
    assert_eq!(
        state(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><svg xmlns="http://www.w3.org/2000/svg"><circle cx="1" cy="1" r="1"/></svg><math xmlns="http://www.w3.org/1998/Math/MathML"><mn>1</mn></math></body></html>"#
        ),
        ForegroundPreparationEligibility::Eligible
    );
    for construct in ["<h:script>run()</h:script>", "<h:iframe/>"] {
        let document = format!(
            r#"<h:html xmlns:h="http://www.w3.org/1999/xhtml"><h:body>{construct}</h:body></h:html>"#
        );
        assert_eq!(
            state(&document),
            ForegroundPreparationEligibility::Ineligible,
            "prefixed XHTML construct should be excluded: {construct}"
        );
    }
}

#[test]
fn foreground_preparation_excludes_active_inline_svg_constructs() {
    for construct in [
        "animate",
        "animateColor",
        "animateMotion",
        "animateTransform",
        "discard",
        "script",
        "set",
    ] {
        let document = format!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><svg xmlns="http://www.w3.org/2000/svg"><{construct}/></svg></body></html>"#
        );
        assert_eq!(
            foreground_preparation_state(
                "application/xhtml+xml",
                &document,
                AnalysisLimits::new(None, None, None, None),
            ),
            ForegroundPreparationEligibility::Ineligible,
            "active SVG construct should be excluded: {construct}"
        );
    }

    assert_eq!(
        foreground_preparation_state(
            "application/xhtml+xml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:svg="http://www.w3.org/2000/svg"><body><svg:svg><svg:animate/></svg:svg></body></html>"#,
            AnalysisLimits::new(None, None, None, None),
        ),
        ForegroundPreparationEligibility::Ineligible
    );
}

#[test]
fn media_overlay_associations_join_reading_order_targets_and_smil_playback_facts() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
        <item id="audio" href="audio.mp3" media-type="audio/mpeg"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body><p id='line'>Line</p></body></html>".to_vec()),
            (
                "EPUB/overlay.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq><par><text src="chapter.xhtml#line"/><audio src="audio.mp3" clipBegin="1s" clipEnd="2s"/></par></seq></body></smil>"#.to_vec(),
            ),
            ("EPUB/audio.mp3", Vec::new()),
        ],
    )
    .analyze();

    let associations = analysis.media_overlay_associations().collect::<Vec<_>>();
    assert_eq!(associations.len(), 1);
    let association = associations[0];
    assert_eq!(
        association
            .content_declaration()
            .media_overlay()
            .unwrap()
            .as_str(),
        "overlay"
    );
    assert_eq!(
        association
            .overlay_declaration()
            .unwrap()
            .href()
            .unwrap()
            .as_str(),
        "overlay.smil"
    );
    assert!(association.overlay_resource_facts().is_some());
    assert!(association.smil_facts().is_some());

    let root = association.roots().next().unwrap();
    assert!(matches!(root.fact(), SmilNodeFact::Sequence { .. }));
    let parallel = root.children().next().unwrap();
    let children = parallel.children().collect::<Vec<_>>();
    assert!(children[0].text_reference().is_some());
    assert!(children[1].audio_reference().is_some());
}

#[test]
fn media_overlay_associations_preserve_missing_ambiguous_and_unavailable_targets() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="missing"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec())],
    )
    .analyze();

    let association = analysis.media_overlay_associations().next().unwrap();
    assert!(matches!(
        association.reference().target(),
        ManifestTarget::Missing
    ));
    assert!(association.overlay_declaration().is_none());
    assert!(association.smil_facts().is_none());
    assert_eq!(association.roots().count(), 0);

    const AMBIGUOUS_PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="one.smil" media-type="application/smil+xml"/>
        <item id="overlay" href="two.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let ambiguous = publication(
        AMBIGUOUS_PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            ("EPUB/one.smil", Vec::new()),
            ("EPUB/two.smil", Vec::new()),
        ],
    )
    .analyze();
    let association = ambiguous.media_overlay_associations().next().unwrap();
    assert!(matches!(
        association.reference().target(),
        ManifestTarget::Ambiguous { candidates } if candidates.len() == 2
    ));
    assert!(association.overlay_resource().is_none());

    const UNAVAILABLE_PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="missing.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let unavailable = publication(
        UNAVAILABLE_PACKAGE,
        [("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec())],
    )
    .analyze();
    let association = unavailable.media_overlay_associations().next().unwrap();
    assert!(association.overlay_resource().is_some());
    assert_eq!(
        association
            .overlay_resource_facts()
            .unwrap()
            .content()
            .issue(),
        Some(AnalysisIssue::Missing)
    );
    assert!(association.smil_facts().is_none());
}

#[test]
fn media_overlay_associations_preserve_reading_order_occurrences() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            (
                "EPUB/overlay.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body/></smil>"#.to_vec(),
            ),
        ],
    )
    .analyze();

    let associations = analysis.media_overlay_associations().collect::<Vec<_>>();
    assert_eq!(associations.len(), 2);
    assert_ne!(
        associations[0].reading_order().ordinal(),
        associations[1].reading_order().ordinal()
    );
    assert_eq!(
        associations[0].content_declaration().ordinal(),
        associations[1].content_declaration().ordinal()
    );
}

#[test]
fn accessibility_observations_are_resolved_borrowed_views() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            (
                "EPUB/chapter.xhtml",
                b"<html><body><h1>Heading</h1><img src='cover.jpg' alt='Cover'/></body></html>".to_vec(),
            ),
            (
                "EPUB/overlay.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><text src="chapter.xhtml"/></par></body></smil>"#.to_vec(),
            ),
        ],
    )
    .analyze();

    let observations = analysis.accessibility_observations().collect::<Vec<_>>();
    assert_eq!(observations.len(), 5);
    assert!(matches!(
        observations[0],
        AccessibilityObservationRef::Content {
            fact: AccessibilityFact::HeadingLevel(_),
            ..
        }
    ));
    assert!(matches!(
        observations[1],
        AccessibilityObservationRef::Content {
            fact: AccessibilityFact::ImageAlt(_),
            ..
        }
    ));
    assert!(matches!(
        observations[2],
        AccessibilityObservationRef::Structure { .. }
    ));
    assert!(observations.iter().any(|observation| matches!(
        observation,
        AccessibilityObservationRef::Content {
            fact: AccessibilityFact::ImageAlt(_),
            ..
        }
    )));
    assert!(
        observations.iter().any(|observation| matches!(
            observation,
            AccessibilityObservationRef::Structure { .. }
        ))
    );
    assert!(
        observations
            .iter()
            .any(|observation| matches!(observation, AccessibilityObservationRef::Smil { .. }))
    );
    assert!(
        observations
            .iter()
            .any(|observation| matches!(observation, AccessibilityObservationRef::MediaOverlay(_)))
    );
    assert!(analysis.coverage().content().is_complete());
}

#[test]
fn accessibility_observations_resolve_navigation_and_inspection_facts() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
        <item id="audio" href="audio.mp3" media-type="audio/mpeg"/>
        <item id="captions" href="captions.vtt" media-type="text/vtt"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            (
                "EPUB/nav.xhtml",
                br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#.to_vec(),
            ),
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            (
                "EPUB/audio.mp3",
                include_bytes!("../src/tests/fixtures/media-cbr.mp3").to_vec(),
            ),
            (
                "EPUB/captions.vtt",
                b"WEBVTT\n\n00:00.000 --> 00:01.000\nCaption\n".to_vec(),
            ),
        ],
    )
    .analyze();

    let observations = analysis.accessibility_observations().collect::<Vec<_>>();
    assert!(observations.iter().any(|observation| matches!(
        observation,
        AccessibilityObservationRef::Navigation {
            resource: Some(_),
            ..
        }
    )));
    assert!(
        observations.iter().any(|observation| matches!(
            observation,
            AccessibilityObservationRef::MediaTrack { .. }
        ))
    );
    assert!(
        observations
            .iter()
            .any(|observation| matches!(observation, AccessibilityObservationRef::WebVtt { .. }))
    );
}

fn assert_partition(coverage: &ResourceCoverage) {
    let states = coverage
        .completed()
        .iter()
        .copied()
        .chain(coverage.partial().iter().map(|work| work.resource()))
        .chain(coverage.unavailable().iter().map(|work| work.resource()))
        .collect::<Vec<_>>();
    let expected = coverage.expected().iter().copied().collect::<HashSet<_>>();
    let actual = states.iter().copied().collect::<HashSet<_>>();

    assert_eq!(states.len(), coverage.expected().len());
    assert_eq!(
        actual.len(),
        states.len(),
        "coverage states must be disjoint"
    );
    assert_eq!(actual, expected, "coverage states must partition expected");
}

fn assert_all_partitions(analysis: &epub_stack::PublicationAnalysis) {
    let coverage = analysis.coverage();
    for set in [
        coverage.classification(),
        coverage.fragments(),
        coverage.content(),
        coverage.inspection(),
        coverage.fingerprints(),
    ] {
        assert_partition(set);
    }

    let indexed = analysis
        .resources()
        .resources()
        .map(|resource| resource.ordinal())
        .collect::<Vec<_>>();
    let facts = analysis
        .resource_facts()
        .map(|facts| facts.resource())
        .collect::<Vec<_>>();
    assert_eq!(facts, indexed, "there is one facts record in index order");
}

fn assert_coverage(
    coverage: &ResourceCoverage,
    expected: usize,
    completed: usize,
    partial: &[AnalysisIssue],
    unavailable: &[AnalysisIssue],
) {
    assert_eq!(coverage.expected().len(), expected);
    assert_eq!(coverage.completed().len(), completed);
    assert_eq!(
        coverage
            .partial()
            .iter()
            .map(|work| work.issue())
            .collect::<Vec<_>>(),
        partial
    );
    assert_eq!(
        coverage
            .unavailable()
            .iter()
            .map(|work| work.issue())
            .collect::<Vec<_>>(),
        unavailable
    );
}

#[test]
fn selected_ncx_contributes_one_complete_relationship_source() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
        <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
      </manifest><spine toc="ncx"><itemref idref="chapter"/></spine>
    </package>"#;
    let book = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body><p>Chapter</p></body></html>".to_vec()),
            (
                "EPUB/toc.ncx",
                br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="chapter.xhtml"/></navPoint></navMap></ncx>"#.to_vec(),
            ),
        ],
    );
    let analysis = book.analyze();
    let ncx = analysis.resources().ncx().unwrap();
    let coverage = analysis
        .coverage()
        .relationships()
        .iter()
        .filter(|coverage| coverage.source() == &RelationshipSource::Ncx(ncx.ordinal()))
        .collect::<Vec<_>>();

    assert_eq!(coverage.len(), 1);
    assert_eq!(coverage[0].state(), &CoverageState::Complete);
}

#[test]
fn declaration_and_reading_order_roots_preserve_exact_declaration_identity() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="primary" href="chapter.xhtml" media-type="application/xhtml+xml" fallback="a"/>
        <item id="alias" href="chapter.xhtml" media-type="application/xhtml+xml" fallback="b"/>
        <item id="a" href="a.bin" media-type="application/octet-stream"/>
        <item id="b" href="b.bin" media-type="application/octet-stream"/>
        <item id="duplicate" href="one.bin" media-type="application/octet-stream"/>
        <item id="duplicate" href="two.bin" media-type="application/octet-stream"/>
      </manifest><spine>
        <itemref idref="primary"/><itemref idref="alias"/><itemref/><itemref idref="missing"/><itemref idref="duplicate"/>
      </spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            ("EPUB/a.bin", Vec::new()),
            ("EPUB/b.bin", Vec::new()),
            ("EPUB/one.bin", Vec::new()),
            ("EPUB/two.bin", Vec::new()),
        ],
    )
    .analyze();
    let chapter = analysis
        .resources()
        .find_unique_resource_by_id("primary")
        .unwrap()
        .ordinal();
    let a = analysis
        .resources()
        .find_unique_resource_by_id("a")
        .unwrap()
        .ordinal();
    let b = analysis
        .resources()
        .find_unique_resource_by_id("b")
        .unwrap()
        .ordinal();
    let primary = analysis.resources().find_unique_by_id("primary").unwrap();
    let alias = analysis.resources().find_unique_by_id("alias").unwrap();
    let reading_order = analysis.resources().reading_order().collect::<Vec<_>>();

    let primary_closure = analysis
        .dependency_closure(Root::Declaration(primary.ordinal()))
        .unwrap();
    let alias_closure = analysis
        .dependency_closure(Root::Declaration(alias.ordinal()))
        .unwrap();
    assert_eq!(primary_closure.resources(), &[chapter, a]);
    assert_eq!(alias_closure.resources(), &[chapter, b]);
    assert_eq!(primary_closure.unresolved(), []);
    assert_eq!(alias_closure.unresolved(), []);
    assert!(primary_closure.is_complete());
    assert!(alias_closure.is_complete());

    assert_eq!(
        analysis
            .dependency_closure(Root::ReadingOrderOccurrence(reading_order[0].ordinal(),))
            .unwrap(),
        primary_closure
    );
    assert_eq!(
        analysis
            .dependency_closure(Root::ReadingOrderOccurrence(reading_order[1].ordinal(),))
            .unwrap(),
        alias_closure
    );
    assert_eq!(
        analysis.dependency_closure(Root::ReadingOrderOccurrence(reading_order[2].ordinal())),
        Err(RootError::MissingReadingOrderIdref(
            reading_order[2].ordinal()
        ))
    );
    assert!(matches!(
        analysis.dependency_closure(Root::ReadingOrderOccurrence(reading_order[3].ordinal())),
        Err(RootError::MissingManifestId { root, idref })
            if root == reading_order[3].ordinal() && idref.as_str() == "missing"
    ));
    let duplicate_candidates = analysis
        .resources()
        .declarations_with_id("duplicate")
        .unwrap()
        .map(|declaration| declaration.ordinal())
        .collect::<Vec<_>>();
    assert_eq!(
        analysis.dependency_closure(Root::ReadingOrderOccurrence(reading_order[4].ordinal())),
        Err(RootError::AmbiguousManifestId {
            root: reading_order[4].ordinal(),
            candidates: duplicate_candidates,
        })
    );
}

#[test]
fn coverage_partitions_have_stable_expected_universes_for_every_budget() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
        <item id="remote" href="https://example.com/book.css" media-type="text/css"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let chapter =
        br##"<html><body><p id="target"><a href="#target">Budget facts</a></p></body></html>"##;
    let blob = b"undeclared provider resource";
    let book = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", chapter.to_vec()),
            ("EPUB/data.bin", blob.to_vec()),
        ],
    );
    let baseline = book.analyze_with_limits(AnalysisLimits::new(None, None, None, None));
    let local = baseline
        .resources()
        .resources()
        .filter(|resource| resource.local_path().is_some())
        .map(|resource| resource.ordinal())
        .collect::<Vec<_>>();
    let total_bytes = PACKAGE.len() as u64 + chapter.len() as u64 + blob.len() as u64;
    let largest = [PACKAGE.len(), chapter.len(), blob.len()]
        .into_iter()
        .max()
        .unwrap() as u64;
    let remote = baseline
        .resources()
        .find_unique_resource_by_id("remote")
        .unwrap()
        .ordinal();

    assert_all_partitions(&baseline);
    assert_coverage(baseline.coverage().classification(), 3, 3, &[], &[]);
    assert_coverage(baseline.coverage().fragments(), 1, 1, &[], &[]);
    assert_coverage(baseline.coverage().content(), 1, 1, &[], &[]);
    assert_coverage(baseline.coverage().inspection(), 3, 3, &[], &[]);
    assert_coverage(baseline.coverage().fingerprints(), 3, 3, &[], &[]);
    assert_eq!(baseline.coverage().classification().expected(), local);
    assert_eq!(baseline.coverage().inspection().expected(), local);
    assert_eq!(baseline.coverage().fingerprints().expected(), local);
    assert_eq!(baseline.coverage().content().expected().len(), 1);
    let chapter_key = baseline
        .resources()
        .find_unique_resource_by_id("chapter")
        .unwrap()
        .ordinal();
    assert_eq!(baseline.coverage().fragments().expected(), &[chapter_key]);
    assert_eq!(
        baseline
            .coverage()
            .relationships()
            .iter()
            .map(|coverage| (*coverage.source(), coverage.state().clone()))
            .collect::<Vec<_>>(),
        [
            (RelationshipSource::Package, CoverageState::Complete),
            (
                RelationshipSource::Xhtml(chapter_key),
                CoverageState::Complete,
            ),
        ]
    );
    let remote_facts = baseline.facts_for(remote).unwrap();
    assert!(remote_facts.classification().is_not_applicable());
    assert!(remote_facts.content().is_not_applicable());
    assert!(remote_facts.inspection().is_not_applicable());
    assert!(remote_facts.fingerprint().is_not_applicable());

    let no_resources = book.analyze_with_limits(AnalysisLimits::new(Some(0), None, None, None));
    assert_all_partitions(&no_resources);
    assert_coverage(
        no_resources.coverage().classification(),
        3,
        1,
        &[],
        &[AnalysisIssue::ResourceLimit, AnalysisIssue::ResourceLimit],
    );
    assert_coverage(no_resources.coverage().fragments(), 0, 0, &[], &[]);
    assert_coverage(
        no_resources.coverage().content(),
        1,
        0,
        &[],
        &[AnalysisIssue::ResourceLimit],
    );
    assert_coverage(
        no_resources.coverage().inspection(),
        3,
        0,
        &[],
        &[
            AnalysisIssue::ResourceLimit,
            AnalysisIssue::ResourceLimit,
            AnalysisIssue::ResourceLimit,
        ],
    );
    assert_coverage(
        no_resources.coverage().fingerprints(),
        3,
        0,
        &[],
        &[
            AnalysisIssue::ResourceLimit,
            AnalysisIssue::ResourceLimit,
            AnalysisIssue::ResourceLimit,
        ],
    );
    assert_eq!(no_resources.coverage().content().expected(), &[chapter_key]);
    assert_eq!(
        no_resources
            .coverage()
            .relationships()
            .iter()
            .map(|coverage| (*coverage.source(), coverage.state().clone()))
            .collect::<Vec<_>>(),
        [
            (RelationshipSource::Package, CoverageState::Complete),
            (
                RelationshipSource::Xhtml(chapter_key),
                CoverageState::Unavailable(AnalysisIssue::ResourceLimit),
            ),
        ]
    );

    let cases = [
        (
            AnalysisLimits::new(Some(local.len() - 1), None, None, None),
            (2, &[][..], &[AnalysisIssue::ResourceLimit][..]),
            (2, &[][..], &[AnalysisIssue::ResourceLimit][..]),
            (2, &[][..], &[AnalysisIssue::ResourceLimit][..]),
        ),
        (
            AnalysisLimits::new(None, Some(largest - 1), None, None),
            (2, &[][..], &[AnalysisIssue::PerResourceAnalysisLimit][..]),
            (2, &[AnalysisIssue::PerResourceAnalysisLimit][..], &[][..]),
            (3, &[][..], &[][..]),
        ),
        (
            AnalysisLimits::new(None, None, Some(total_bytes - 1), None),
            (2, &[][..], &[AnalysisIssue::TotalAnalysisLimit][..]),
            (2, &[AnalysisIssue::TotalAnalysisLimit][..], &[][..]),
            (3, &[][..], &[][..]),
        ),
        (
            AnalysisLimits::new(None, None, None, Some(total_bytes - 1)),
            (3, &[][..], &[][..]),
            (3, &[][..], &[][..]),
            (2, &[][..], &[AnalysisIssue::TotalFingerprintLimit][..]),
        ),
    ];
    for (limits, classification, inspection, fingerprints) in cases {
        let constrained = book.analyze_with_limits(limits);
        assert_all_partitions(&constrained);
        assert_coverage(
            constrained.coverage().classification(),
            3,
            classification.0,
            classification.1,
            classification.2,
        );
        assert_coverage(constrained.coverage().fragments(), 1, 1, &[], &[]);
        assert_coverage(constrained.coverage().content(), 1, 1, &[], &[]);
        assert_coverage(
            constrained.coverage().inspection(),
            3,
            inspection.0,
            inspection.1,
            inspection.2,
        );
        assert_coverage(
            constrained.coverage().fingerprints(),
            3,
            fingerprints.0,
            fingerprints.1,
            fingerprints.2,
        );
        assert_eq!(constrained.coverage().classification().expected(), local);
        assert_eq!(
            constrained.coverage().fragments().expected(),
            &[chapter_key]
        );
        assert_eq!(constrained.coverage().content().expected(), &[chapter_key]);
        assert_eq!(constrained.coverage().inspection().expected(), local);
        assert_eq!(constrained.coverage().fingerprints().expected(), local);
        assert_eq!(
            constrained
                .coverage()
                .relationships()
                .iter()
                .map(|coverage| (*coverage.source(), coverage.state().clone()))
                .collect::<Vec<_>>(),
            [
                (RelationshipSource::Package, CoverageState::Complete),
                (
                    RelationshipSource::Xhtml(chapter_key),
                    CoverageState::Complete,
                ),
            ]
        );
    }

    // Each exact boundary completes; each corresponding boundary-minus-one case above
    // exposes one precisely classified incomplete producer.
    for limits in [
        AnalysisLimits::new(Some(local.len()), None, None, None),
        AnalysisLimits::new(None, Some(largest), None, None),
        AnalysisLimits::new(None, None, Some(total_bytes), None),
        AnalysisLimits::new(None, None, None, Some(total_bytes)),
    ] {
        let boundary = book.analyze_with_limits(limits);
        assert_all_partitions(&boundary);
        assert_coverage(boundary.coverage().classification(), 3, 3, &[], &[]);
        assert_coverage(boundary.coverage().fragments(), 1, 1, &[], &[]);
        assert_coverage(boundary.coverage().content(), 1, 1, &[], &[]);
        assert_coverage(boundary.coverage().inspection(), 3, 3, &[], &[]);
        assert_coverage(boundary.coverage().fingerprints(), 3, 3, &[], &[]);
    }
}

#[test]
fn search_entries_borrow_facts_and_keep_a_detached_text_snapshot() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="first" href="chapter.xhtml" media-type="application/xhtml+xml"/>
        <item id="alias" href="chapter.xhtml" media-type="application/xhtml+xml"/>
      </manifest><spine><itemref idref="first"/><itemref idref="alias"/></spine>
    </package>"#;
    let mut book = publication(
        PACKAGE,
        [(
            "EPUB/chapter.xhtml",
            br#"<html xml:lang="fr" dir="rtl"><body><h2 id="opening">Bonjour monde</h2></body></html>"#.to_vec(),
        )],
    );
    let analysis = book.analyze();
    let entries = analysis.search_entries().collect::<Vec<_>>();
    let entry = entries
        .iter()
        .find(|entry| entry.chunk().heading_level().is_some())
        .unwrap();

    assert_eq!(entry.text().unwrap(), "Bonjour monde");
    assert_eq!(entry.facts().resource(), entry.resource().ordinal());
    assert!(matches!(
        entry.chunk().kind(),
        TextChunkKind::Heading { .. }
    ));
    assert_eq!(entry.chunk().heading_level().unwrap().get(), 2);
    assert_eq!(entry.chunk().fragment(), Some("opening"));
    assert_eq!(entry.chunk().lang(), Some("fr"));
    assert_eq!(entry.chunk().dir(), Some(TextDirection::Rtl));
    assert!(entry.chunk().stream_range().is_some());
    assert_eq!(entry.declarations().count(), 2);
    assert_eq!(entry.reading_order_entries().count(), 2);

    book.edit()
        .replace_resource(
            ResourceSelector::path("EPUB/chapter.xhtml").unwrap(),
            b"<html><body><p>Changed</p></body></html>".to_vec(),
        )
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    assert_eq!(entry.text().unwrap(), "Bonjour monde");
    let changed = book.analyze();
    assert_eq!(
        changed.search_entries().next().unwrap().text().unwrap(),
        "Changed"
    );
}

#[test]
fn fingerprint_queries_find_resources_and_duplicate_groups() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
      <spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"same bytes".to_vec()),
            ("EPUB/copy.bin", b"same bytes".to_vec()),
        ],
    )
    .analyze();
    let hash = Blake3Hash::hash(b"same bytes");
    let resources = analysis.resources_by_blake3(hash).collect::<Vec<_>>();
    let groups = analysis
        .duplicate_fingerprint_groups()
        .collect::<Vec<DuplicateGroup<'_>>>();

    assert_eq!(resources.len(), 2);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].hash(), hash);
    assert_eq!(groups[0].resources().count(), 2);
}

#[test]
fn standalone_svg_foreign_object_exposes_nested_xhtml_and_resolved_links()
-> Result<(), Box<dyn std::error::Error>> {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata></metadata>
      <manifest><item id="page" href="page.svg" media-type="image/svg+xml"/><item id="unknown" href="unknown.svg" media-type="image/svg+xml"/></manifest>
      <spine><itemref idref="page"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            (
                "EPUB/page.svg",
                br#"<svg xmlns="http://www.w3.org/2000/svg" xml:base="assets/">
                  <foreignObject id="panel"><div xmlns="http://www.w3.org/1999/xhtml">
                    <p id="caption">Foreign text</p><img src="image.png"/><script src="app.js"></script>
                  </div></foreignObject>
                </svg>"#
                    .to_vec(),
            ),
            (
                "EPUB/unknown.svg",
                br#"<svg xmlns="http://www.w3.org/2000/svg"><x:widget xmlns:x="urn:example" href="missed.png"/></svg>"#.to_vec(),
            ),
            ("EPUB/assets/image.png", vec![0]),
            ("EPUB/assets/app.js", vec![0]),
        ],
    )
    .analyze();
    let page = analysis.resources().find_unique_resource_by_id("page")?;
    let facts = analysis
        .content_for(page.ordinal())?
        .value()
        .and_then(ContentFacts::as_svg)
        .ok_or("missing SVG facts")?;
    let foreign = &facts.foreign_objects()[0];

    assert_eq!(foreign.fragment(), Some("panel"));
    assert_eq!(foreign.xhtml().text_stream().text(), "Foreign text");
    assert_eq!(foreign.xhtml().fragments()[0].id(), "caption");
    assert_eq!(foreign.xhtml().media().len(), 1);
    assert_eq!(foreign.xhtml().scripts().len(), 1);

    let references = analysis
        .references_from_resource(page.ordinal())?
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    for (reference, (declared, role, element, attribute, target)) in references.into_iter().zip([
        (
            "image.png",
            HrefRole::Image,
            "img",
            "src",
            "EPUB/assets/image.png",
        ),
        (
            "app.js",
            HrefRole::Script,
            "script",
            "src",
            "EPUB/assets/app.js",
        ),
    ]) {
        assert_eq!(reference.declared().as_str(), declared);
        assert_eq!(reference.role(), role);
        let ReferenceContext::Svg(context) = reference.context() else {
            panic!("foreign-object reference should retain SVG context");
        };
        assert_eq!(
            (context.element(), context.attribute()),
            (element, attribute)
        );
        let HrefTarget::Resource { resource, .. } = reference.target() else {
            panic!("foreign-object reference should resolve to a resource");
        };
        assert_eq!(
            analysis
                .resources()
                .resource(*resource)?
                .address()
                .display_value(),
            target
        );
    }
    assert!(analysis.coverage().relationships().iter().any(|coverage| {
        matches!(coverage.source(), RelationshipSource::Svg(key) if *key == page.ordinal())
            && coverage.state() == &CoverageState::Complete
    }));
    let unknown = analysis.resources().find_unique_resource_by_id("unknown")?;
    assert!(analysis.coverage().relationships().iter().any(|coverage| {
        matches!(coverage.source(), RelationshipSource::Svg(key) if *key == unknown.ordinal())
            && coverage.state() == &CoverageState::Partial(AnalysisIssue::Unsupported)
    }));

    Ok(())
}

#[test]
fn curated_content_facts_expose_semantic_variants_and_joined_optional_references() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine/></package>"#;
    let book = publication(
        PACKAGE,
        [(
            "EPUB/chapter.xhtml",
            br#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body onload="start()">
              <section id="chapter" epub:type="chapter"><h2 id="heading" xml:id="heading-xml">Heading</h2></section>
              <span id="page" epub:type="pagebreak" aria-label="Page 1">1</span><figure/><table/>
              <aside epub:type="footnote"/><nav/>
              <img src="image.png" alt="Cover"/><audio src="audio.mp3"></audio><video src="video.mp4" poster="poster.png"></video>
              <track kind="captions" srclang="en" label="English"/>
              <picture><source srcset="image.webp 1x"/></picture><source src="candidate.bin"/>
               <form id="search" action="submit" method="post"><input id="query" type="search" name="query" value="term"/><button id="alternate" formaction="alternate" name="mode" value="alt">Alt</button></form>
              <script id="external" type="text/javascript" src="app.js"></script><script id="inline">inline()</script>
              <script id="data" type="application/ld+json">{}</script>
            </body></html>"#
                .to_vec(),
        )],
    );
    let analysis = book.analyze();
    let chapter = analysis
        .resources()
        .find_unique_resource_by_id("chapter")
        .unwrap()
        .ordinal();
    let media = analysis
        .xhtml_media(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        media
            .iter()
            .map(|media| (
                match media.fact() {
                    MediaFact::Image { .. } => "image",
                    MediaFact::Audio => "audio",
                    MediaFact::Video => "video",
                    MediaFact::Source {
                        context: MediaSourceContext::Picture,
                        ..
                    } => "picture-source",
                    MediaFact::Source {
                        context: MediaSourceContext::Other,
                        ..
                    } => "other-source",
                    MediaFact::Source { .. } => "media-source",
                    MediaFact::Track { .. } => "track",
                    MediaFact::Poster => "poster",
                },
                media
                    .references()
                    .next()
                    .map(|reference| reference.declared().as_str()),
            ))
            .collect::<Vec<_>>(),
        [
            ("image", Some("image.png")),
            ("audio", Some("audio.mp3")),
            ("video", Some("video.mp4")),
            ("poster", Some("poster.png")),
            ("track", None),
            ("picture-source", Some("image.webp")),
            ("other-source", None),
        ]
    );
    let forms = analysis
        .xhtml_forms(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        forms
            .iter()
            .map(|occurrence| (
                match occurrence.fact() {
                    FormFact::Form { .. } => "form",
                    FormFact::Control { .. } => "control",
                },
                occurrence
                    .reference()
                    .map(|reference| reference.declared().as_str()),
            ))
            .collect::<Vec<_>>(),
        [
            ("form", Some("submit")),
            ("control", None),
            ("control", Some("alternate")),
        ]
    );
    let scripts = analysis
        .xhtml_scripts(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        scripts
            .iter()
            .map(|occurrence| (
                match occurrence.fact() {
                    ScriptFact::External { .. } => "external",
                    ScriptFact::Inline { .. } => "inline",
                    ScriptFact::DataBlock { .. } => "data",
                    ScriptFact::EventHandler { .. } => "handler",
                },
                occurrence
                    .reference()
                    .map(|reference| reference.declared().as_str()),
            ))
            .collect::<Vec<_>>(),
        [
            ("handler", None),
            ("external", Some("app.js")),
            ("inline", None),
            ("data", None),
        ]
    );
}

#[test]
fn responsive_media_and_submit_controls_keep_exact_public_reference_joins() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine/></package>"#;
    let book = publication(
        PACKAGE,
        [(
            "EPUB/chapter.xhtml",
            br#"<html><body>
                <picture><source src="ignored.webp" srcset="one.webp 1x, data:image/webp;base64,AAAA 2x, two.webp 3x"></picture>
                <img src="shared.png" srcset="small.png 1x, data:image/png;base64,AAAA 2x" alt="first">
                <img alt="source-less"><img src="shared.png" alt="second">
                <input id="image" type="image" src="input.png" formaction="image-submit" name="send" value="go" alt="submit">
                <form id="first-form" action="submit-a" method="post"><input id="value" name="value"></form>
                <form action="submit-b"></form>
                <script id="first-script" src="script-a.js" type="module"></script>
                <script id="inline-script">inline()</script>
                <script src="script-b.js"></script>
            </body></html>"#
                .to_vec(),
        )],
    );
    let analysis = book.analyze();
    let chapter = analysis
        .resources()
        .find_unique_resource_by_id("chapter")
        .unwrap()
        .ordinal();
    let media = analysis
        .xhtml_media(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();

    assert_eq!(
        media
            .iter()
            .map(|occurrence| {
                occurrence
                    .references()
                    .map(|reference| reference.declared().as_str())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [
            vec!["one.webp", "data:image/webp;base64,AAAA", "two.webp"],
            vec!["shared.png", "small.png", "data:image/png;base64,AAAA"],
            vec![],
            vec!["shared.png"],
            vec!["input.png"],
        ]
    );

    let forms = analysis
        .xhtml_forms(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        forms
            .iter()
            .map(|occurrence| {
                occurrence
                    .reference()
                    .map(|reference| reference.declared().as_str())
            })
            .collect::<Vec<_>>(),
        [
            Some("image-submit"),
            Some("submit-a"),
            None,
            Some("submit-b"),
        ]
    );

    let scripts = analysis
        .xhtml_scripts(chapter)
        .unwrap()
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(
        scripts
            .iter()
            .map(|occurrence| {
                occurrence
                    .reference()
                    .map(|reference| reference.declared().as_str())
            })
            .collect::<Vec<_>>(),
        [Some("script-a.js"), None, Some("script-b.js"),]
    );
}
