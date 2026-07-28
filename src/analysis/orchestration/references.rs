use super::navigation::navigation_reference_indices;
use super::resolution::*;
use super::*;

pub(super) fn collect_manifest_references(
    resources: &ResourceIndex,
    references: &mut Vec<AuthoredReference>,
) {
    for declaration in resources.declarations() {
        if let Some(idref) = declaration.fallback() {
            let target = manifest_id_target(resources, idref);
            manifest_reference(
                references,
                declaration.key(),
                idref.clone(),
                ManifestRole::Fallback,
                target,
            );
        }
        if let Some(idref) = declaration.media_overlay() {
            let target = manifest_id_target(resources, idref);
            manifest_reference(
                references,
                declaration.key(),
                idref.clone(),
                ManifestRole::MediaOverlay,
                target,
            );
        }
    }
}

fn manifest_id_target(resources: &ResourceIndex, idref: &AuthoredIdRef) -> ManifestTarget {
    let candidates = resources
        .declarations_with_id(idref.as_str())
        .ok()
        .into_iter()
        .flatten()
        .map(ManifestDeclarationRef::key)
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => ManifestTarget::Missing,
        [declaration] => {
            let resource =
                resources
                    .declaration((*declaration).into())
                    .ok()
                    .and_then(|declaration| match declaration.target_row() {
                        DeclarationTargetRow::Resource(resource) => Some(*resource),
                        DeclarationTargetRow::MissingHref
                        | DeclarationTargetRow::InvalidHref(_) => None,
                    });
            ManifestTarget::Declaration {
                declaration: (*declaration).into(),
                resource: resource.map(Into::into),
            }
        }
        _ => ManifestTarget::Ambiguous {
            candidates: candidates.into_iter().map(Into::into).collect(),
        },
    }
}

pub(super) fn collect_package_href_references(
    package: &Package,
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
) -> Vec<Option<ReferenceSlot>> {
    let source = resources.package().key();
    let link_references = package
        .metadata()
        .link()
        .iter()
        .map(|link| {
            link.authored_href().map(|href| {
                push_href_reference(
                    resources,
                    facts,
                    references,
                    source,
                    href,
                    HrefRole::Metadata,
                    resources.package().local_path().expect("package is local"),
                    ReferenceContext::Package(ElementAttribute::new("link", "href")),
                )
            })
        })
        .collect();
    if let Some(guide) = package.guide() {
        for reference in guide.references() {
            if let Some(href) = reference.authored_href() {
                push_href_reference(
                    resources,
                    facts,
                    references,
                    source,
                    href,
                    HrefRole::Guide,
                    resources.package().local_path().expect("package is local"),
                    ReferenceContext::Package(ElementAttribute::new("reference", "href")),
                );
            }
        }
    }

    let mut collections = package.collections().iter().collect::<Vec<_>>();
    while let Some(collection) = collections.pop() {
        collect_collection_links(collection, source, resources, facts, references);
        collections.extend(collection.collections());
    }
    link_references
}

fn collect_collection_links(
    collection: &Collection,
    source: ResourceRow,
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
) {
    let links = collection.link().iter().chain(
        collection
            .metadata()
            .into_iter()
            .flat_map(|metadata| metadata.link()),
    );
    for link in links {
        if let Some(href) = link.authored_href() {
            push_href_reference(
                resources,
                facts,
                references,
                source,
                href,
                HrefRole::Collection,
                resources.package().local_path().expect("package is local"),
                ReferenceContext::Package(ElementAttribute::new("link", "href")),
            );
        }
    }
}

pub(super) fn collect_xhtml_references(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    xhtml_sources: &HashSet<ResourceRow>,
    mut pending_by_resource: HashMap<
        ResourceRow,
        (Vec<LinkFact>, Option<AuthoredHref>, XhtmlLinkAssociations),
    >,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) -> HashMap<ResourceRow, XhtmlReferenceIndex> {
    let mut indexes = HashMap::new();
    for index in 0..facts.len() {
        let resource_key = facts[index].resource_row();
        if !xhtml_sources.contains(&resource_key) {
            continue;
        }
        let Some((links, authored_base, associations)) = pending_by_resource.remove(&resource_key)
        else {
            if let AnalysisOutcome::Unavailable(issue) = facts[index].content() {
                coverage.push(RelationshipCoverage::new(
                    RelationshipSource::Xhtml((resource_key).into()),
                    CoverageState::Unavailable(*issue),
                ));
            }
            continue;
        };
        let source_path = resources
            .resource(resource_key.into())
            .ok()
            .and_then(ResourceRef::local_path);
        let Some(source_path) = source_path else {
            continue;
        };
        let navigation_indices = navigation_reference_indices(&links, references, resource_key);
        let mut link_references = vec![None; links.len()];
        for (link_index, (link, navigation_index)) in
            links.iter().zip(navigation_indices).enumerate()
        {
            if let Some(reference_index) = navigation_index {
                link_references[link_index] = Some(ReferenceSlot::new(reference_index));
                continue;
            }
            let kind = match link {
                LinkFact::Hyperlink(_) => HrefRole::Hyperlink,
                LinkFact::Stylesheet(_) => HrefRole::Stylesheet,
                LinkFact::FormAction(_) => HrefRole::FormAction,
                LinkFact::Image(_) => HrefRole::Image,
                LinkFact::Script(_) => HrefRole::Script,
                LinkFact::Audio(_) => HrefRole::Audio,
                LinkFact::Video(_) => HrefRole::Video,
                LinkFact::Track(_) => HrefRole::Track,
                LinkFact::Poster(_) => HrefRole::Poster,
                LinkFact::Object(_) => HrefRole::Object,
                LinkFact::Embed(_) => HrefRole::Embed,
                LinkFact::Iframe(_) => HrefRole::Iframe,
                LinkFact::SvgReference(_) => HrefRole::Svg,
            };
            let reference = push_xhtml_href_reference(
                resources,
                facts,
                references,
                resource_key,
                link.declared(),
                kind,
                source_path,
                authored_base.as_ref(),
                ReferenceContext::Xhtml(ElementAttribute::new(
                    link.element(),
                    link.attribute().as_str(),
                )),
            );
            link_references[link_index] = Some(reference);
        }
        indexes.insert(
            resource_key,
            XhtmlReferenceIndex {
                media: joined_media_reference_slots(associations.media, &link_references),
                forms: joined_reference_slots(associations.forms, &link_references),
                scripts: joined_reference_slots(associations.scripts, &link_references),
            },
        );
        let state = match facts[index].content() {
            AnalysisOutcome::Partial { issue, .. } => CoverageState::Partial(*issue),
            AnalysisOutcome::Complete(_) => CoverageState::Complete,
            AnalysisOutcome::NotApplicable | AnalysisOutcome::Unavailable(_) => unreachable!(),
        };
        coverage.push(RelationshipCoverage::new(
            RelationshipSource::Xhtml((resource_key).into()),
            state,
        ));
    }
    indexes
}

fn joined_reference_slots(
    links: Vec<Option<usize>>,
    references: &[Option<ReferenceSlot>],
) -> Vec<Option<ReferenceSlot>> {
    links
        .into_iter()
        .map(|link| link.and_then(|index| references.get(index).copied().flatten()))
        .collect()
}

fn joined_media_reference_slots(
    links: Vec<Vec<usize>>,
    references: &[Option<ReferenceSlot>],
) -> Vec<Vec<ReferenceSlot>> {
    links
        .into_iter()
        .map(|links| {
            links
                .into_iter()
                .filter_map(|index| references.get(index).copied().flatten())
                .collect()
        })
        .collect()
}

pub(super) fn collect_css_references(
    resources: &ResourceIndex,
    facts: &mut [ResourceFacts],
    css_sources: &HashSet<ResourceRow>,
    outcomes: &HashMap<ResourceRow, AnalysisOutcome<()>>,
    mut pending_by_resource: HashMap<
        ResourceRow,
        Vec<crate::content::extraction::css::CssPendingReference>,
    >,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) {
    for index in 0..facts.len() {
        let resource = facts[index].resource_row();
        if !css_sources.contains(&resource) {
            continue;
        }
        let Some(pending) = pending_by_resource.remove(&resource) else {
            if let Some(AnalysisOutcome::Unavailable(issue)) = outcomes.get(&resource) {
                coverage.push(RelationshipCoverage::new(
                    RelationshipSource::Css((resource).into()),
                    CoverageState::Unavailable(*issue),
                ));
            }
            continue;
        };
        let Some(source_path) = resources
            .resource(resource.into())
            .ok()
            .and_then(ResourceRef::local_path)
        else {
            continue;
        };
        for pending in pending {
            push_href_reference(
                resources,
                facts,
                references,
                resource,
                &pending.declared,
                pending.kind,
                source_path,
                ReferenceContext::Css(CssContext::new(pending.at_rule, pending.property)),
            );
        }
        let state = match outcomes
            .get(&resource)
            .expect("CSS source must retain an extraction outcome")
        {
            AnalysisOutcome::Complete(_) => CoverageState::Complete,
            AnalysisOutcome::Partial { issue, .. } => CoverageState::Partial(*issue),
            AnalysisOutcome::NotApplicable | AnalysisOutcome::Unavailable(_) => unreachable!(),
        };
        coverage.push(RelationshipCoverage::new(
            RelationshipSource::Css((resource).into()),
            state,
        ));
    }
}

pub(super) fn collect_svg_references(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    svg_sources: &HashSet<ResourceRow>,
    mut pending_by_resource: HashMap<ResourceRow, Vec<SvgPendingRef>>,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) {
    for resource_facts in facts {
        let resource = resource_facts.resource_row();
        if !svg_sources.contains(&resource) {
            continue;
        }
        let Some(pending) = pending_by_resource.remove(&resource) else {
            if let AnalysisOutcome::Unavailable(issue) = resource_facts.content() {
                coverage.push(RelationshipCoverage::new(
                    RelationshipSource::Svg((resource).into()),
                    CoverageState::Unavailable(*issue),
                ));
            }
            continue;
        };
        let Some(source_path) = resources
            .resource(resource.into())
            .ok()
            .and_then(ResourceRef::local_path)
        else {
            continue;
        };
        for pending in pending {
            push_svg_href_reference(
                resources,
                facts,
                references,
                resource,
                pending.declared(),
                pending.bases(),
                pending.kind(),
                source_path,
                ReferenceContext::Svg(ElementAttribute::new(
                    pending.element(),
                    pending.attribute(),
                )),
            );
        }
        let state = match resource_facts.content() {
            AnalysisOutcome::Complete(_) => CoverageState::Complete,
            AnalysisOutcome::Partial { issue, .. } => CoverageState::Partial(*issue),
            AnalysisOutcome::NotApplicable | AnalysisOutcome::Unavailable(_) => unreachable!(),
        };
        coverage.push(RelationshipCoverage::new(
            RelationshipSource::Svg((resource).into()),
            state,
        ));
    }
}

pub(crate) fn collect_smil_references(
    resources: &ResourceIndex,
    facts: &mut [ResourceFacts],
    smil_sources: &HashSet<ResourceRow>,
    mut pending_by_resource: HashMap<ResourceRow, Vec<SmilPendingReference>>,
    references: &mut Vec<AuthoredReference>,
    coverage: &mut Vec<RelationshipCoverage>,
) {
    for index in 0..facts.len() {
        let resource = facts[index].resource_row();
        if !smil_sources.contains(&resource) {
            continue;
        }
        let Some(pending) = pending_by_resource.remove(&resource) else {
            if let AnalysisOutcome::Unavailable(issue) = facts[index].content() {
                coverage.push(RelationshipCoverage::new(
                    RelationshipSource::Smil((resource).into()),
                    CoverageState::Unavailable(*issue),
                ));
            }
            continue;
        };
        let Some(source_path) = resources
            .resource(resource.into())
            .ok()
            .and_then(ResourceRef::local_path)
        else {
            continue;
        };
        let mut resolved = Vec::with_capacity(pending.len());
        for pending in pending {
            let id = push_href_reference(
                resources,
                facts,
                references,
                resource,
                &pending.authored,
                pending.kind,
                source_path,
                ReferenceContext::Smil(ElementAttribute::new(pending.element, pending.attribute)),
            );
            resolved.push((pending.node, pending.kind, id));
        }
        if let Some(ContentFacts::Smil(content)) = facts[index].content_mut().value_mut() {
            for (node, kind, reference) in resolved {
                match kind {
                    HrefRole::SmilText => content.set_text_reference(node, reference),
                    HrefRole::SmilAudio => content.set_audio_reference(node, reference),
                    _ => unreachable!("only SMIL references link to SMIL nodes"),
                }
            }
        }
        let state = match facts[index].content() {
            AnalysisOutcome::Partial { issue, .. } => CoverageState::Partial(*issue),
            AnalysisOutcome::Complete(_) => CoverageState::Complete,
            AnalysisOutcome::NotApplicable | AnalysisOutcome::Unavailable(_) => continue,
        };
        coverage.push(RelationshipCoverage::new(
            RelationshipSource::Smil((resource).into()),
            state,
        ));
    }
}
