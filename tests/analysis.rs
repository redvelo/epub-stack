use epub_stack::content::text::TextRole;
use epub_stack::{
    Epub, EpubPath, MemoryResourceProvider,
    accessibility::{AccessibilityObservation, AccessibilityObservationRef},
    analysis::{
        AnalysisIssue, AnalysisLimit, AnalysisLimits,
        coverage::{Completeness, RelationshipSource, ResourceCompleteness, ResourceCoverage},
        fingerprint::{Blake3Hash, DuplicateGroup},
        reference::{HrefRole, HrefTarget, ManifestTarget, ReferenceContext},
    },
    content::{ContentFacts, FormFact, FragmentFact, MediaFact, MediaSourceContext, ScriptFact},
    media_overlay::SmilNodeFact,
    semantics::TextDirection,
};
use std::collections::HashSet;

fn limits(configure: impl FnOnce(&mut AnalysisLimits)) -> AnalysisLimits {
    let mut limits = AnalysisLimits::default();
    limits.max_analyzed_resources = None;
    limits.max_resource_analysis_bytes = None;
    limits.max_total_analysis_bytes = None;
    limits.max_total_fingerprint_bytes = None;
    limits.max_smil_nodes = None;
    limits.max_smil_nesting = None;
    configure(&mut limits);
    limits
}

fn unlimited_limits() -> AnalysisLimits {
    limits(|_| {})
}

fn publication(
    package: &[u8],
    resources: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
) -> Epub<MemoryResourceProvider> {
    let provider = MemoryResourceProvider::from_entries(
        std::iter::once(("EPUB/package.opf", package.to_vec())).chain(resources),
    )
    .unwrap();
    Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap()
}

#[test]
fn xhtml_activity_reports_authored_constructs() {
    use epub_stack::content::DocumentActivity;

    for (construct, expected) in [
        ("<audio/>", DocumentActivity::Media),
        ("<iframe/>", DocumentActivity::EmbeddedContent),
        ("<input autofocus=\"\"/>", DocumentActivity::Autofocus),
        (
            "<p onclick=\"run()\">Text</p>",
            DocumentActivity::EventAttribute,
        ),
        (
            "<script type=\"application/ld+json\">{}</script>",
            DocumentActivity::ScriptElement,
        ),
        (
            "<meta http-equiv=\"refresh\" content=\"0\"/>",
            DocumentActivity::Refresh,
        ),
        (
            "<link rel=\"preconnect\" href=\"https://example.com\"/>",
            DocumentActivity::ResourceHint,
        ),
        ("<svg><animate/></svg>", DocumentActivity::Animation),
    ] {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/></manifest><spine/></package>"#;
        let document = format!("<html><head>{construct}</head><body/></html>");
        let analysis = publication(package, [("EPUB/page.xhtml", document.into_bytes())]).analyze();
        let ordinal = analysis
            .resources()
            .declaration_by_id("page")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();
        let resource = analysis.resource(ordinal).unwrap();
        assert!(resource.content().is_complete());
        let facts = resource
            .content()
            .value()
            .and_then(ContentFacts::as_xhtml)
            .unwrap();
        assert_eq!(
            facts.activity().iter().collect::<Vec<_>>(),
            [expected],
            "{construct}"
        );
    }
}

#[test]
fn standalone_svg_executable_projection_includes_foreign_object_xhtml() {
    let executable = |document: &str| {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.svg" media-type="image/svg+xml"/></manifest><spine><itemref idref="page"/></spine></package>"#;
        let analysis =
            publication(package, [("EPUB/page.svg", document.as_bytes().to_vec())]).analyze();
        let page = analysis
            .resources()
            .declaration_by_id("page")
            .unwrap()
            .resource()
            .unwrap();
        analysis
            .resource(page.ordinal())
            .unwrap()
            .content()
            .value()
            .and_then(ContentFacts::as_svg)
            .map(epub_stack::content::SvgFacts::has_executable_content)
    };

    assert_eq!(
        executable(r#"<svg xmlns="http://www.w3.org/2000/svg"><text>Static</text></svg>"#),
        Some(false)
    );
    assert_eq!(
        executable(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><script type="application/ld+json">{}</script></svg>"#
        ),
        Some(false)
    );
    let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.svg" media-type="image/svg+xml"/></manifest><spine><itemref idref="page"/></spine></package>"#;
    let analysis = publication(
        package,
        [("EPUB/page.svg", br#"<svg xmlns="http://www.w3.org/2000/svg"><script type="application/ld+json">{}</script></svg>"#.to_vec())],
    )
    .analyze();
    let page = analysis
        .resources()
        .declaration_by_id("page")
        .unwrap()
        .resource()
        .unwrap();
    let svg = analysis
        .resource(page.ordinal())
        .unwrap()
        .content()
        .value()
        .and_then(ContentFacts::as_svg)
        .unwrap();
    assert!(matches!(svg.scripts(), [ScriptFact::DataBlock { .. }]));
    assert_eq!(
        executable(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><script xmlns="http://www.w3.org/1999/xhtml">run()</script></foreignObject></svg>"#
        ),
        Some(true)
    );
}

#[test]
fn inline_scripts_without_text_are_not_executable_content() {
    let executable = |head: &str| {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="page"/></spine></package>"#;
        let document = format!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><head>{head}</head><body><p>Text</p></body></html>"#
        );
        let analysis = publication(package, [("EPUB/page.xhtml", document.into_bytes())]).analyze();
        let page = analysis
            .resources()
            .declaration_by_id("page")
            .unwrap()
            .resource()
            .unwrap();
        analysis
            .resource(page.ordinal())
            .unwrap()
            .content()
            .value()
            .and_then(ContentFacts::as_xhtml)
            .map(epub_stack::content::XhtmlFacts::has_executable_content)
    };

    assert_eq!(
        executable(r#"<script type="text/javascript"></script>"#),
        Some(false)
    );
    assert_eq!(executable("<script>\n  </script>"), Some(false));
    assert_eq!(executable("<script>run()</script>"), Some(true));
    assert_eq!(executable(r#"<script src=""></script>"#), Some(true));
    assert_eq!(
        executable("<script></script><script>run()</script>"),
        Some(true)
    );
}

#[test]
fn media_overlay_durations_split_publication_totals_from_item_refinements() {
    const PACKAGE: &[u8] = br##"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata>
        <meta property="media:duration">1:30:00</meta>
        <meta property="media:duration" refines="#overlay">0:30:00</meta>
      </metadata><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"##;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            (
                "EPUB/overlay.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq/></body></smil>"#.to_vec(),
            ),
        ],
    )
    .analyze();

    let metadata = analysis.media_overlays().metadata();
    assert_eq!(metadata.durations().len(), 2);
    let totals = metadata.total_durations().collect::<Vec<_>>();
    assert_eq!(totals.len(), 1);
    assert_eq!(totals[0].refines(), None);
    let items = metadata.item_durations().collect::<Vec<_>>();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].refines(), Some("#overlay"));
}

#[test]
fn executable_content_detection_spans_xhtml_and_svg_without_matching_on_format() {
    let detected = |path: &'static str, media_type: &str, bytes: &'static [u8]| {
        let package = format!(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="{}" media-type="{media_type}"/></manifest><spine><itemref idref="page"/></spine></package>"#,
            path.trim_start_matches("EPUB/")
        );
        let analysis = publication(package.as_bytes(), [(path, bytes.to_vec())]).analyze();
        let page = analysis
            .resources()
            .declaration_by_id("page")
            .unwrap()
            .resource()
            .unwrap();
        analysis
            .resource(page.ordinal())
            .unwrap()
            .content()
            .value()
            .and_then(ContentFacts::executable_content_detected)
    };

    assert_eq!(
        detected(
            "EPUB/page.xhtml",
            "application/xhtml+xml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><script>run()</script></head><body/></html>",
        ),
        Some(true)
    );
    assert_eq!(
        detected(
            "EPUB/page.xhtml",
            "application/xhtml+xml",
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><script></script></head><body/></html>",
        ),
        Some(false)
    );
    assert_eq!(
        detected(
            "EPUB/page.svg",
            "image/svg+xml",
            br#"<svg xmlns="http://www.w3.org/2000/svg"><script>run()</script></svg>"#,
        ),
        Some(true)
    );
    assert_eq!(
        detected(
            "EPUB/overlay.smil",
            "application/smil+xml",
            br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq/></body></smil>"#,
        ),
        None
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
    assert!(association.overlay_resource().is_some());
    assert!(association.smil_facts().is_some());

    let root = association.roots().next().unwrap();
    assert!(matches!(root.fact(), SmilNodeFact::Sequence { .. }));
    let parallel = root.children().next().unwrap();
    let children = parallel.children().collect::<Vec<_>>();
    assert!(children[0].text_reference().is_some());
    assert!(children[1].audio_reference().is_some());
}

#[test]
fn publication_analysis_applies_and_retains_smil_structural_limits() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
        <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analyze = |limits| {
        publication(
            PACKAGE,
            [
                ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
                (
                    "EPUB/overlay.smil",
                    br#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq><par/></seq></body></smil>"#.to_vec(),
                ),
            ],
        )
        .analyze_with_limits(limits)
    };

    for (limits, expected_issue) in [
        (
            limits(|limits| {
                limits.max_smil_nodes = Some(3);
                limits.max_smil_nesting = Some(64);
            }),
            AnalysisIssue::Limit(AnalysisLimit::SmilNodes),
        ),
        (
            limits(|limits| {
                limits.max_smil_nodes = Some(64);
                limits.max_smil_nesting = Some(2);
            }),
            AnalysisIssue::Limit(AnalysisLimit::SmilNesting),
        ),
    ] {
        let expected_nodes = limits.max_smil_nodes;
        let expected_nesting = limits.max_smil_nesting;
        let analysis = analyze(limits);
        assert_eq!(analysis.limits().max_smil_nodes, expected_nodes);
        assert_eq!(analysis.limits().max_smil_nesting, expected_nesting);
        let association = analysis.media_overlay_associations().next().unwrap();
        assert_eq!(
            association.overlay_resource().unwrap().content().issue(),
            Some(expected_issue)
        );
        assert!(association.smil_facts().is_none());
        assert_eq!(association.roots().count(), 0);
    }
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
        association.overlay_resource().unwrap().content().issue(),
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
        AccessibilityObservationRef::Content { fact, .. }
            if matches!(fact.observation(), AccessibilityObservation::HeadingLevel(_))
    ));
    assert!(matches!(
        observations[1],
        AccessibilityObservationRef::Content {
            fact,
            ..
        } if matches!(fact.observation(), AccessibilityObservation::ImageAlt(_))
    ));
    assert!(matches!(
        observations[2],
        AccessibilityObservationRef::Structure { .. }
    ));
    assert!(observations.iter().any(|observation| matches!(
        observation,
        AccessibilityObservationRef::Content {
            fact,
            ..
        } if matches!(fact.observation(), AccessibilityObservation::ImageAlt(_))
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

fn assert_all_partitions(analysis: &epub_stack::PublicationAnalysis) {
    let coverage = analysis.coverage();
    for set in [
        coverage.fragments(),
        coverage.content(),
        coverage.inspection(),
        coverage.fingerprints(),
    ] {
        let resources = set.iter().map(|entry| entry.resource).collect::<Vec<_>>();
        assert_eq!(
            resources.iter().collect::<HashSet<_>>().len(),
            resources.len(),
            "coverage entries must be disjoint"
        );
    }

    let indexed = analysis
        .resources()
        .resources()
        .map(|resource| resource.ordinal())
        .collect::<Vec<_>>();
    let facts = analysis
        .analyzed_resources()
        .map(|resource| resource.resource().ordinal())
        .collect::<Vec<_>>();
    assert_eq!(facts, indexed, "there is one facts record in index order");
}

fn assert_coverage(
    coverage: ResourceCoverage<'_>,
    expected: usize,
    completed: usize,
    partial: &[AnalysisIssue],
    unavailable: &[AnalysisIssue],
) {
    let entries = coverage.iter().collect::<Vec<ResourceCompleteness>>();
    assert_eq!(entries.len(), expected);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.completeness.is_complete())
            .count(),
        completed
    );
    assert_eq!(
        entries
            .iter()
            .filter_map(|entry| match entry.completeness {
                Completeness::Partial(issue) => Some(issue),
                _ => None,
            })
            .collect::<Vec<_>>(),
        partial
    );
    assert_eq!(
        entries
            .iter()
            .filter_map(|entry| match entry.completeness {
                Completeness::Unavailable(issue) => Some(issue),
                _ => None,
            })
            .collect::<Vec<_>>(),
        unavailable
    );
}

fn xhtml_text(
    resource: epub_stack::analysis::ResourceAnalysisRef<'_>,
) -> Option<&epub_stack::content::text::TextStream> {
    resource
        .content()
        .value()
        .and_then(ContentFacts::as_xhtml)
        .map(epub_stack::content::XhtmlFacts::text_stream)
}

fn expected(coverage: ResourceCoverage<'_>) -> Vec<epub_stack::resource::ResourceOrdinal> {
    coverage.iter().map(|entry| entry.resource).collect()
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
        .filter(|coverage| coverage.source == RelationshipSource::Ncx(ncx.ordinal()))
        .collect::<Vec<_>>();

    assert_eq!(coverage.len(), 1);
    assert_eq!(coverage[0].completeness, Completeness::Complete);
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
        .declaration_by_id("primary")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let a = analysis
        .resources()
        .declaration_by_id("a")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let b = analysis
        .resources()
        .declaration_by_id("b")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let primary = analysis.resources().declaration_by_id("primary").unwrap();
    let alias = analysis.resources().declaration_by_id("alias").unwrap();
    let reading_order = analysis.resources().reading_order().collect::<Vec<_>>();

    let primary_closure = analysis
        .declaration(primary.ordinal())
        .unwrap()
        .dependency_closure();
    let alias_closure = analysis
        .declaration(alias.ordinal())
        .unwrap()
        .dependency_closure();
    assert_eq!(primary_closure.resources(), &[chapter, a]);
    assert_eq!(alias_closure.resources(), &[chapter, b]);
    assert_eq!(primary_closure.unresolved(), []);
    assert_eq!(alias_closure.unresolved(), []);
    assert!(primary_closure.is_complete());
    assert!(alias_closure.is_complete());

    let occurrence_closure = |index: usize| {
        analysis
            .declaration(reading_order[index].declaration().unwrap().ordinal())
            .unwrap()
            .dependency_closure()
    };
    assert_eq!(occurrence_closure(0), primary_closure);
    assert_eq!(occurrence_closure(1), alias_closure);
    assert_eq!(reading_order[2].target(), None);
    assert_eq!(
        reading_order[3].target(),
        Some(&epub_stack::resource::IdrefTarget::Missing)
    );
    assert_eq!(reading_order[3].idref().unwrap().as_str(), "missing");
    assert_eq!(
        reading_order[4]
            .target()
            .map(epub_stack::resource::IdrefTarget::candidates)
            .map(<[_]>::len),
        Some(2)
    );
    assert_eq!(
        analysis
            .resources()
            .declarations_with_id("duplicate")
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn coverage_is_complete_only_when_no_part_stopped_early() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let chapter =
        br##"<html><body><p id="target"><a href="#target">Complete</a></p></body></html>"##;
    let book = publication(PACKAGE, [("EPUB/chapter.xhtml", chapter.to_vec())]);

    assert!(
        book.analyze_with_limits(unlimited_limits())
            .coverage()
            .is_complete()
    );

    let budgeted = book.analyze_with_limits(limits(|limits| {
        limits.max_analyzed_resources = Some(0);
    }));
    assert!(!budgeted.coverage().is_complete());
    assert!(!budgeted.coverage().content().is_complete());
    // A part with nothing to cover stays complete; the overall answer is false regardless.
    assert!(budgeted.coverage().fragments().is_complete());
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
    let baseline = book.analyze_with_limits(unlimited_limits());
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
        .declaration_by_id("remote")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();

    assert_all_partitions(&baseline);
    assert_coverage(baseline.coverage().fragments(), 1, 1, &[], &[]);
    assert_coverage(baseline.coverage().content(), 1, 1, &[], &[]);
    assert_coverage(baseline.coverage().inspection(), 3, 3, &[], &[]);
    assert_coverage(baseline.coverage().fingerprints(), 3, 3, &[], &[]);
    assert_eq!(expected(baseline.coverage().inspection()), local);
    assert_eq!(expected(baseline.coverage().fingerprints()), local);
    assert_eq!(expected(baseline.coverage().content()).len(), 1);
    let chapter_key = baseline
        .resources()
        .declaration_by_id("chapter")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    assert_eq!(expected(baseline.coverage().fragments()), [chapter_key]);
    assert_eq!(
        baseline
            .coverage()
            .relationships()
            .iter()
            .map(|coverage| (coverage.source, coverage.completeness))
            .collect::<Vec<_>>(),
        [
            (
                RelationshipSource::Css(remote),
                Completeness::Unavailable(AnalysisIssue::Unsupported),
            ),
            (
                RelationshipSource::Xhtml(chapter_key),
                Completeness::Complete
            ),
        ]
    );
    let remote_facts = baseline.resource(remote).unwrap();
    assert!(remote_facts.content().is_not_applicable());
    assert!(remote_facts.inspection().is_not_applicable());
    assert!(remote_facts.fingerprint().is_not_applicable());

    let no_resources = book.analyze_with_limits(limits(|limits| {
        limits.max_analyzed_resources = Some(0);
    }));
    assert_all_partitions(&no_resources);
    assert_coverage(no_resources.coverage().fragments(), 0, 0, &[], &[]);
    assert_coverage(
        no_resources.coverage().content(),
        1,
        0,
        &[],
        &[AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources)],
    );
    assert_coverage(
        no_resources.coverage().inspection(),
        3,
        0,
        &[],
        &[
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
        ],
    );
    assert_coverage(
        no_resources.coverage().fingerprints(),
        3,
        0,
        &[],
        &[
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
            AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources),
        ],
    );
    assert_eq!(expected(no_resources.coverage().content()), [chapter_key]);
    assert_eq!(
        no_resources
            .coverage()
            .relationships()
            .iter()
            .map(|coverage| (coverage.source, coverage.completeness))
            .collect::<Vec<_>>(),
        [
            (
                RelationshipSource::Css(remote),
                Completeness::Unavailable(AnalysisIssue::Unsupported),
            ),
            (
                RelationshipSource::Xhtml(chapter_key),
                Completeness::Unavailable(AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources)),
            ),
        ]
    );

    let cases = [
        (
            limits(|limits| {
                limits.max_analyzed_resources = Some(local.len() - 1);
            }),
            (1, &[][..], &[][..]),
            (
                2,
                &[][..],
                &[AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources)][..],
            ),
            (
                2,
                &[][..],
                &[AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources)][..],
            ),
        ),
        (
            limits(|limits| {
                limits.max_resource_analysis_bytes = Some(largest - 1);
            }),
            (
                2,
                &[][..],
                &[AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)][..],
            ),
            (
                2,
                &[AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)][..],
                &[][..],
            ),
            (3, &[][..], &[][..]),
        ),
        (
            limits(|limits| {
                limits.max_total_analysis_bytes = Some(total_bytes - 1);
            }),
            (
                2,
                &[][..],
                &[AnalysisIssue::Limit(AnalysisLimit::TotalAnalysisBytes)][..],
            ),
            (
                2,
                &[AnalysisIssue::Limit(AnalysisLimit::TotalAnalysisBytes)][..],
                &[][..],
            ),
            (3, &[][..], &[][..]),
        ),
        (
            limits(|limits| {
                limits.max_total_fingerprint_bytes = Some(total_bytes - 1);
            }),
            (1, &[][..], &[][..]),
            (3, &[][..], &[][..]),
            (
                2,
                &[][..],
                &[AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes)][..],
            ),
        ),
    ];
    for (limits, content, inspection, fingerprints) in cases {
        let constrained = book.analyze_with_limits(limits);
        assert_all_partitions(&constrained);
        assert_coverage(constrained.coverage().fragments(), 1, 1, &[], &[]);
        assert_coverage(
            constrained.coverage().content(),
            content.0,
            1,
            content.1,
            content.2,
        );
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
        assert_eq!(expected(constrained.coverage().fragments()), [chapter_key]);
        assert!(expected(constrained.coverage().content()).contains(&chapter_key));
        assert_eq!(expected(constrained.coverage().inspection()), local);
        assert_eq!(expected(constrained.coverage().fingerprints()), local);
        assert_eq!(
            constrained
                .coverage()
                .relationships()
                .iter()
                .map(|coverage| (coverage.source, coverage.completeness))
                .collect::<Vec<_>>(),
            [
                (
                    RelationshipSource::Css(remote),
                    Completeness::Unavailable(AnalysisIssue::Unsupported),
                ),
                (
                    RelationshipSource::Xhtml(chapter_key),
                    Completeness::Complete
                ),
            ]
        );
    }

    // Each exact boundary completes; each corresponding boundary-minus-one case above
    // exposes one precisely classified incomplete producer.
    for limits in [
        limits(|limits| {
            limits.max_analyzed_resources = Some(local.len());
        }),
        limits(|limits| {
            limits.max_resource_analysis_bytes = Some(largest);
        }),
        limits(|limits| {
            limits.max_total_analysis_bytes = Some(total_bytes);
        }),
        limits(|limits| {
            limits.max_total_fingerprint_bytes = Some(total_bytes);
        }),
    ] {
        let boundary = book.analyze_with_limits(limits);
        assert_all_partitions(&boundary);
        assert_coverage(boundary.coverage().fragments(), 1, 1, &[], &[]);
        assert_coverage(boundary.coverage().content(), 1, 1, &[], &[]);
        assert_coverage(boundary.coverage().inspection(), 3, 3, &[], &[]);
        assert_coverage(boundary.coverage().fingerprints(), 3, 3, &[], &[]);
    }
}

#[test]
fn text_spans_borrow_their_stream_and_keep_a_detached_snapshot() {
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
    let resource = analysis
        .analyzed_resources()
        .find(|resource| xhtml_text(*resource).is_some())
        .unwrap();
    let entry = xhtml_text(resource)
        .unwrap()
        .spans()
        .find(|span| matches!(span.role(), TextRole::Heading { .. }))
        .unwrap();

    assert_eq!(entry.text(), "Bonjour monde");
    assert!(matches!(
        entry.role(),
        TextRole::Heading { level } if level.get() == 2
    ));
    assert_eq!(entry.origin().fragment().unwrap().id(), "opening");
    assert_eq!(entry.origin().lang(), Some("fr"));
    assert_eq!(entry.origin().dir(), Some(TextDirection::Rtl));
    assert_eq!(resource.resource().declarations().count(), 2);

    book.edit()
        .upsert_resource(
            EpubPath::new("EPUB/chapter.xhtml").unwrap(),
            b"<html><body><p>Changed</p></body></html>".to_vec(),
        )
        .unwrap()
        .preview()
        .unwrap()
        .commit();
    assert_eq!(entry.text(), "Bonjour monde");
    let changed = book.analyze();
    assert_eq!(
        xhtml_text(changed.resource(resource.resource().ordinal()).unwrap())
            .unwrap()
            .text(),
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
    let resources = analysis
        .resources_with_fingerprint(hash)
        .collect::<Vec<_>>();
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
    let page = analysis
        .resources()
        .declaration_by_id("page")
        .ok_or("missing page declaration")?
        .resource()
        .ok_or("declaration has no resource")?;
    let facts = analysis
        .resource(page.ordinal())
        .unwrap()
        .content()
        .value()
        .and_then(ContentFacts::as_svg)
        .ok_or("missing SVG facts")?;
    let foreign = &facts.foreign_objects()[0];

    assert_eq!(foreign.fragment().map(FragmentFact::id), Some("panel"));
    assert_eq!(foreign.xhtml().text_stream().text(), "Foreign text");
    assert_eq!(foreign.xhtml().fragments()[0].id(), "caption");
    assert_eq!(foreign.xhtml().media().len(), 1);
    assert_eq!(foreign.xhtml().scripts().len(), 1);

    let references = analysis
        .resource(page.ordinal())
        .unwrap()
        .references()
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
        let ReferenceContext::Element(context) = reference.context() else {
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
                .resource(*resource)
                .unwrap()
                .address()
                .display_value(),
            target
        );
    }
    assert!(analysis.coverage().relationships().iter().any(|coverage| {
        matches!(coverage.source, RelationshipSource::Svg(key) if key == page.ordinal())
            && coverage.completeness == Completeness::Complete
    }));
    let unknown = analysis
        .resources()
        .declaration_by_id("unknown")
        .ok_or("missing unknown declaration")?
        .resource()
        .ok_or("declaration has no resource")?;
    assert!(analysis.coverage().relationships().iter().any(|coverage| {
        matches!(coverage.source, RelationshipSource::Svg(key) if key == unknown.ordinal())
            && coverage.completeness == Completeness::Partial(AnalysisIssue::Unsupported)
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
        .declaration_by_id("chapter")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let media = analysis
        .resource(chapter)
        .unwrap()
        .xhtml_media()
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
        .resource(chapter)
        .unwrap()
        .xhtml_forms()
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
        .resource(chapter)
        .unwrap()
        .xhtml_scripts()
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
        .declaration_by_id("chapter")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let media = analysis
        .resource(chapter)
        .unwrap()
        .xhtml_media()
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
        .resource(chapter)
        .unwrap()
        .xhtml_forms()
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
        .resource(chapter)
        .unwrap()
        .xhtml_scripts()
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

#[test]
fn non_executable_script_sources_stay_in_the_reference_graph() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
        <item id="figure" href="figure.svg" media-type="image/svg+xml"/>
        <item id="data" href="data.json" media-type="application/json"/>
      </manifest><spine><itemref idref="page"/></spine>
    </package>"#;
    let page = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
        <script type="application/json" src="data.json"></script>
    </body></html>"#;
    let figure =
        br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
        <script type="application/json" xlink:href="data.json"></script>
    </svg>"#;
    let analysis = publication(
        PACKAGE,
        [
            ("EPUB/page.xhtml", page.to_vec()),
            ("EPUB/figure.svg", figure.to_vec()),
            ("EPUB/data.json", b"{}".to_vec()),
        ],
    )
    .analyze();
    let resource = |id: &str| {
        analysis
            .resources()
            .declaration_by_id(id)
            .unwrap()
            .resource()
            .unwrap()
            .ordinal()
    };
    let data = resource("data");

    for source in [resource("page"), resource("figure")] {
        let facts = analysis.resource(source).unwrap();
        assert!(
            facts.references().any(|reference| {
                reference.declared().as_str() == "data.json"
                    && reference.role() == HrefRole::Script
                    && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == data)
            }),
            "a non-executable script source must remain an authored reference"
        );
        assert!(facts.dependency_closure().resources().contains(&data));
    }
    assert!(
        analysis
            .resource(resource("page"))
            .unwrap()
            .content()
            .value()
            .and_then(ContentFacts::as_xhtml)
            .is_some_and(|facts| !facts.has_executable_content()),
        "a data block must not count as executable content"
    );
}

#[test]
fn resources_skipped_by_the_analysis_budget_keep_not_applicable_content() {
    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
      <metadata/><manifest>
        <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
        <item id="cover" href="cover.png" media-type="image/png"/>
      </manifest><spine><itemref idref="chapter"/></spine>
    </package>"#;
    let analysis = publication(
        PACKAGE,
        [
            (
                "EPUB/chapter.xhtml",
                b"<html><body><p>Text</p></body></html>".to_vec(),
            ),
            ("EPUB/cover.png", b"not a real png".to_vec()),
        ],
    )
    .analyze_with_limits(limits(|limits| {
        limits.max_analyzed_resources = Some(1);
    }));
    let cover = analysis
        .resources()
        .declaration_by_id("cover")
        .unwrap()
        .resource()
        .unwrap()
        .ordinal();
    let facts = analysis.resource(cover).unwrap();

    assert!(facts.content().is_not_applicable());
    assert_eq!(
        facts.inspection().issue(),
        Some(AnalysisIssue::Limit(AnalysisLimit::AnalyzedResources))
    );
    assert!(
        !analysis
            .coverage()
            .content()
            .iter()
            .any(|entry| entry.resource == cover)
    );
}
