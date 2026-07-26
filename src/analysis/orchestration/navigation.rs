use super::resolution::*;
use super::*;

pub(super) fn collect_secondary_ncx_references(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    mut results: HashMap<ResourceKey, std::result::Result<NavigationDocument, AnalysisIssue>>,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) {
    let mut seen = HashSet::new();
    for declaration in resources.declarations().iter().filter(|declaration| {
        declaration
            .media_type()
            .is_some_and(crate::resource::MediaType::is_ncx)
    }) {
        let DeclarationTarget::Resource(key) = declaration.target() else {
            continue;
        };
        if !seen.insert(*key) {
            continue;
        }
        let Some(result) = results.remove(key) else {
            continue;
        };
        match result {
            Ok(document) => {
                collect_navigation_document_references(
                    &document, *key, resources, facts, references, None,
                );
                coverage.push(RelationshipCoverage::new(
                    RelationshipSource::Ncx(*key),
                    CoverageState::Complete,
                ));
            }
            Err(issue) => coverage.push(RelationshipCoverage::new(
                RelationshipSource::Ncx(*key),
                CoverageState::Unavailable(issue),
            )),
        }
    }
}

pub(super) fn collect_navigation_references(
    navigation: &Navigation,
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    xhtml_pending: &HashMap<
        ResourceKey,
        (Vec<LinkFact>, Option<AuthoredHref>, XhtmlLinkAssociations),
    >,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) {
    let Some(document) = navigation.document() else {
        return;
    };
    let Some(record) = resources
        .resources_at(&ResourceAddress::Local(document.path().clone()))
        .next()
    else {
        return;
    };
    let (source, state, authored_base) = match document.source() {
        NavigationSource::Ncx => (
            RelationshipSource::Ncx(record.key()),
            CoverageState::Complete,
            None,
        ),
        NavigationSource::EpubNav => {
            let outcome = facts
                .iter()
                .find(|facts| facts.resource() == record.key())
                .map(ResourceFacts::content);
            let authored_base = xhtml_pending
                .get(&record.key())
                .and_then(|(_, authored_base, _)| authored_base.as_ref());
            match outcome {
                Some(AnalysisOutcome::Complete(content)) => (
                    RelationshipSource::Navigation(record.key()),
                    CoverageState::Complete,
                    content.as_xhtml().and(authored_base),
                ),
                Some(AnalysisOutcome::Partial {
                    value: content,
                    issue,
                }) => (
                    RelationshipSource::Navigation(record.key()),
                    CoverageState::Partial(*issue),
                    content.as_xhtml().and(authored_base),
                ),
                Some(AnalysisOutcome::Unavailable(issue)) => {
                    coverage.push(RelationshipCoverage::new(
                        RelationshipSource::Navigation(record.key()),
                        CoverageState::Unavailable(*issue),
                    ));
                    return;
                }
                Some(AnalysisOutcome::NotApplicable) | None => {
                    coverage.push(RelationshipCoverage::new(
                        RelationshipSource::Navigation(record.key()),
                        CoverageState::Unavailable(AnalysisIssue::Unsupported),
                    ));
                    return;
                }
            }
        }
    };
    collect_navigation_document_references(
        document,
        record.key(),
        resources,
        facts,
        references,
        authored_base,
    );
    coverage.push(RelationshipCoverage::new(source, state));
}

fn collect_navigation_document_references(
    document: &NavigationDocument,
    source: ResourceKey,
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
    authored_base: Option<&AuthoredHref>,
) {
    for list in document.lists() {
        let kind = match document.source() {
            NavigationSource::Ncx => HrefRole::Ncx,
            NavigationSource::EpubNav => match list.semantic() {
                Some(crate::semantics::EpubStructuralSemantic::Toc) => HrefRole::Toc,
                Some(crate::semantics::EpubStructuralSemantic::PageList) => HrefRole::PageList,
                Some(crate::semantics::EpubStructuralSemantic::Landmarks) => HrefRole::Landmark,
                _ => continue,
            },
        };
        let mut points = list.points().iter().collect::<Vec<_>>();
        while let Some(point) = points.pop() {
            if let Some(href) = point.authored_href() {
                let context = ReferenceContext::Navigation(ElementAttribute::new(
                    if document.source() == NavigationSource::Ncx {
                        "content"
                    } else {
                        "a"
                    },
                    if document.source() == NavigationSource::Ncx {
                        "src"
                    } else {
                        "href"
                    },
                ));
                if document.source() == NavigationSource::EpubNav {
                    push_xhtml_href_reference(
                        resources,
                        facts,
                        references,
                        source,
                        href,
                        kind,
                        document.path(),
                        authored_base,
                        context,
                    );
                } else {
                    push_href_reference(
                        resources,
                        facts,
                        references,
                        source,
                        href,
                        kind,
                        document.path(),
                        context,
                    );
                }
            }
            points.extend(point.children());
        }
    }
}

pub(super) fn navigation_reference_indices(
    links: &[LinkFact],
    references: &[AuthoredReference],
    source: ResourceKey,
) -> Vec<Option<usize>> {
    let navigation = references
        .iter()
        .enumerate()
        .filter_map(|reference| {
            let (index, reference) = reference;
            let AuthoredReference::Href(reference) = reference else {
                return None;
            };
            (reference.source() == source
                && matches!(
                    reference.role(),
                    HrefRole::Toc | HrefRole::PageList | HrefRole::Landmark
                ))
            .then_some((index, reference))
        })
        .collect::<Vec<_>>();
    let mut indices = vec![None; links.len()];
    let mut claimed = HashSet::new();

    for (index, link) in links.iter().enumerate() {
        let Some(kind) = link.navigation().map(navigation_reference_kind) else {
            continue;
        };
        if let Some((reference_index, _)) = navigation.iter().find(|(index, reference)| {
            reference.role() == kind
                && reference.declared() == link.declared()
                && !claimed.contains(index)
        }) {
            indices[index] = Some(*reference_index);
            claimed.insert(*reference_index);
        }
    }

    for (reference_index, reference) in navigation {
        if claimed.contains(&reference_index) {
            continue;
        }
        if let Some((index, _)) = links.iter().enumerate().find(|(index, link)| {
            indices[*index].is_none()
                && matches!(link, LinkFact::Hyperlink(_))
                && link.declared() == reference.declared()
        }) {
            indices[index] = Some(reference_index);
            claimed.insert(reference_index);
        }
    }
    indices
}

fn navigation_reference_kind(kind: NavigationLinkKind) -> HrefRole {
    match kind {
        NavigationLinkKind::Toc => HrefRole::Toc,
        NavigationLinkKind::PageList => HrefRole::PageList,
        NavigationLinkKind::Landmark => HrefRole::Landmark,
    }
}
