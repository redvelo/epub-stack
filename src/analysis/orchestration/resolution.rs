pub(crate) fn probe_root_format(bytes: &[u8]) -> (Option<SemanticFormat>, Option<Detection>) {
    let inspection = detect_resource_format(bytes);
    if inspection
        .as_ref()
        .is_some_and(|detection| detection.format() == DetectedFormat::Svg)
    {
        return (Some(SemanticFormat::Svg), inspection);
    }
    let input = BufReader::new(XmlUtf8Reader::new(Cursor::new(bytes)));
    let mut reader = quick_xml::Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        let Ok(event) = reader.read_event_into(&mut buffer) else {
            return (None, inspection);
        };
        match event {
            quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element) => {
                let name = element.name();
                let Some(local) = name.as_ref().rsplit(|byte| *byte == b':').next() else {
                    return (None, inspection);
                };
                let format = if local.eq_ignore_ascii_case(b"html") {
                    Some(SemanticFormat::Xhtml)
                } else if local.eq_ignore_ascii_case(b"smil") {
                    Some(SemanticFormat::Smil)
                } else {
                    None
                };
                return (format, inspection);
            }
            quick_xml::events::Event::Text(text) if !text.iter().all(u8::is_ascii_whitespace) => {
                return (None, inspection);
            }
            quick_xml::events::Event::Eof => return (None, inspection),
            _ => {}
        }
        buffer.clear();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_xhtml_href_reference(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
    source: ResourceRow,
    declared: &AuthoredHref,
    kind: HrefRole,
    source_path: &EpubPath,
    authored_base: Option<&AuthoredHref>,
    context: ReferenceContext,
) -> ReferenceSlot {
    let (resolved, query) = authored_base
        .and_then(|base| resolve_href_with_bases(resources, source_path, &[base], declared))
        .unwrap_or_else(|| {
            (
                resources.resolve_href_from(declared.as_str(), source_path),
                authored_query(declared),
            )
        });
    let target = href_target(resources, facts, declared, resolved, query);
    href_reference(references, source, declared.clone(), kind, target, context)
}

fn resolve_href_with_bases(
    resources: &ResourceIndex,
    source_path: &EpubPath,
    authored_bases: &[&AuthoredHref],
    declared: &AuthoredHref,
) -> Option<(ResolvedHref, Option<String>)> {
    const LOCAL_ORIGIN: &str = "analysis.invalid";
    const LOCAL_ROOT: &str = "/__epub_root__/";
    if authored_bases
        .iter()
        .copied()
        .chain(std::iter::once(declared))
        .any(|href| matches!(parse_href(href.clone()), ParsedHref::Invalid { .. }))
    {
        return Some((
            ResolvedHref::Invalid(declared.as_str().to_string()),
            authored_query(declared),
        ));
    }
    let mut document = url::Url::parse(&format!("https://{LOCAL_ORIGIN}/")).ok()?;
    document.set_path(&format!("{LOCAL_ROOT}{}", source_path.as_str()));
    let bases_are_relative = authored_bases
        .iter()
        .all(|base| is_relative_url_reference(base));
    let target_is_relative = is_relative_url_reference(declared);
    let mut base = document;
    for authored_base in authored_bases {
        base = base.join(authored_base.as_str()).ok()?;
    }
    let target = base.join(declared.as_str()).ok()?;
    let query = target.query().map(str::to_string);
    if bases_are_relative
        && target_is_relative
        && target.scheme() == "https"
        && target.host_str() == Some(LOCAL_ORIGIN)
    {
        let Some(path) = target.path().strip_prefix(LOCAL_ROOT) else {
            return Some((ResolvedHref::Invalid(declared.as_str().to_string()), query));
        };
        let mut href = path.to_string();
        if let Some(query) = target.query() {
            href.push('?');
            href.push_str(query);
        }
        if let Some(fragment) = target.fragment() {
            href.push('#');
            href.push_str(fragment);
        }
        let root = EpubPath::new("__analysis_root__.xhtml").expect("constant path is valid");
        Some((resources.resolve_href_from(href, &root), query))
    } else {
        Some((
            resources.resolve_href_from(target.as_str(), source_path),
            query,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_svg_href_reference(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
    source: ResourceRow,
    declared: &AuthoredHref,
    authored_bases: &[AuthoredHref],
    kind: HrefRole,
    source_path: &EpubPath,
    context: ReferenceContext,
) -> ReferenceSlot {
    let bases = authored_bases.iter().collect::<Vec<_>>();
    let (resolved, query) = resolve_href_with_bases(resources, source_path, &bases, declared)
        .unwrap_or_else(|| {
            (
                resources.resolve_href_from(declared.as_str(), source_path),
                authored_query(declared),
            )
        });
    let target = href_target(resources, facts, declared, resolved, query);
    href_reference(references, source, declared.clone(), kind, target, context)
}

fn is_relative_url_reference(href: &AuthoredHref) -> bool {
    !href.as_str().starts_with("//") && url::Url::parse(href.as_str()).is_err()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_href_reference(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    references: &mut Vec<AuthoredReference>,
    source: ResourceRow,
    declared: &AuthoredHref,
    kind: HrefRole,
    source_path: &EpubPath,
    context: ReferenceContext,
) -> ReferenceSlot {
    let resolved = resources.resolve_href_from(declared.as_str(), source_path);
    let target = href_target(
        resources,
        facts,
        declared,
        resolved,
        authored_query(declared),
    );
    href_reference(references, source, declared.clone(), kind, target, context)
}

fn href_target(
    resources: &ResourceIndex,
    facts: &[ResourceFacts],
    declared: &AuthoredHref,
    resolved: ResolvedHref,
    query: Option<String>,
) -> HrefTarget {
    match resolved {
        ResolvedHref::Resource(address) => match &address {
            ResourceAddress::Local(path) => resources
                .resources_at(&address)
                .next()
                .map(|resource| HrefTarget::Resource {
                    resource: resource.ordinal(),
                    query,
                })
                .unwrap_or_else(|| HrefTarget::MissingLocal(path.clone())),
            ResourceAddress::Remote(href) => HrefTarget::Remote {
                href: href.clone(),
                declared_resource: resources
                    .resources_at(&address)
                    .next()
                    .map(ResourceRef::ordinal),
            },
            ResourceAddress::Data(value) => HrefTarget::Data(value.clone()),
            ResourceAddress::External(value) => HrefTarget::External(value.clone()),
            ResourceAddress::Invalid(_) => HrefTarget::Invalid(declared.clone()),
        },
        ResolvedHref::Fragment {
            resource: address,
            fragment,
            ..
        } if fragment.is_empty() => href_target(
            resources,
            facts,
            declared,
            ResolvedHref::Resource(address),
            query,
        ),
        ResolvedHref::Fragment {
            resource: address,
            fragment,
            ..
        } => match &address {
            ResourceAddress::Local(path) => resources
                .resources_at(&address)
                .next()
                .map(|resource| HrefTarget::Fragment {
                    resource: resource.ordinal(),
                    query,
                    exists: fragment_exists(facts, resource.key(), &fragment),
                    fragment,
                })
                .unwrap_or_else(|| HrefTarget::MissingLocal(path.clone())),
            ResourceAddress::Remote(href) => HrefTarget::Remote {
                href: format!("{href}#{fragment}"),
                declared_resource: resources
                    .resources_at(&address)
                    .next()
                    .map(ResourceRef::ordinal),
            },
            ResourceAddress::Data(value) => HrefTarget::Data(value.clone()),
            ResourceAddress::External(value) => HrefTarget::External(value.clone()),
            ResourceAddress::Invalid(_) => HrefTarget::Invalid(declared.clone()),
        },
        ResolvedHref::RemoteUrl(href) => HrefTarget::Remote {
            declared_resource: resources
                .resources_at(&ResourceAddress::Remote(href.clone()))
                .next()
                .map(ResourceRef::ordinal),
            href,
        },
        ResolvedHref::Data(value) => HrefTarget::Data(value),
        ResolvedHref::External(value) => HrefTarget::External(value),
        ResolvedHref::MissingPath(path) => HrefTarget::MissingLocal(path),
        ResolvedHref::MissingManifestId(_)
        | ResolvedHref::AmbiguousAddress { .. }
        | ResolvedHref::Invalid(_) => HrefTarget::Invalid(declared.clone()),
    }
}

fn authored_query(href: &AuthoredHref) -> Option<String> {
    href.as_str()
        .split_once('#')
        .map_or(href.as_str(), |(target, _)| target)
        .split_once('?')
        .map(|(_, query)| query.to_string())
}

pub(super) fn facts_for_row(
    facts: &[ResourceFacts],
    resource: ResourceRow,
) -> Option<&ResourceFacts> {
    facts
        .get(resource.0)
        .filter(|facts| facts.resource_row() == resource)
}

fn fragment_exists(facts: &[ResourceFacts], resource: ResourceRow, fragment: &str) -> Option<bool> {
    let outcome = facts_for_row(facts, resource)?.content();
    let content = outcome.value()?;
    let fragments = content
        .as_xhtml()
        .map(|facts| facts.fragments())
        .or_else(|| content.as_svg().map(|facts| facts.fragments()))?;
    let found = fragments.iter().any(|fact| fact.id() == fragment);
    match outcome {
        AnalysisOutcome::Complete(_) => Some(found),
        AnalysisOutcome::Partial { .. } => found.then_some(true),
        AnalysisOutcome::NotApplicable | AnalysisOutcome::Unavailable(_) => None,
    }
}

pub(super) fn fragment_coverage(
    references: &[AuthoredReference],
    facts: &[ResourceFacts],
) -> ResourceCoverage {
    let mut expected = Vec::new();
    let mut expected_set = HashSet::new();
    for reference in references {
        let AuthoredReference::Href(reference) = reference else {
            continue;
        };
        let HrefTarget::Fragment { resource, .. } = reference.target() else {
            continue;
        };
        if facts_for_row(facts, (*resource).into())
            .is_none_or(|facts| matches!(facts.content(), AnalysisOutcome::NotApplicable))
        {
            continue;
        }
        if expected_set.insert(*resource) {
            expected.push(*resource);
        }
    }
    let mut completed = Vec::new();
    let mut partial = Vec::new();
    let mut unavailable = Vec::new();
    for resource in &expected {
        let outcome = facts_for_row(facts, (*resource).into()).map(ResourceFacts::content);
        match outcome {
            Some(AnalysisOutcome::Complete(_)) => completed.push(*resource),
            Some(AnalysisOutcome::Partial { issue, .. }) => {
                partial.push(IncompleteResource::new(*resource, *issue))
            }
            Some(AnalysisOutcome::Unavailable(issue)) => {
                unavailable.push(IncompleteResource::new(*resource, *issue))
            }
            Some(AnalysisOutcome::NotApplicable) | None => unavailable.push(
                IncompleteResource::new(*resource, AnalysisIssue::Unsupported),
            ),
        }
    }
    ResourceCoverage::new(expected, completed, partial, unavailable)
}
use super::*;
