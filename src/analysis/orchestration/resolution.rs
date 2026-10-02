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
                let Some(local) = name.as_ref().rsplit(':').next() else {
                    return (None, inspection);
                };
                let format = if local.eq_ignore_ascii_case("html") {
                    Some(SemanticFormat::Xhtml)
                } else if local.eq_ignore_ascii_case("smil") {
                    Some(SemanticFormat::Smil)
                } else {
                    None
                };
                return (format, inspection);
            }
            quick_xml::events::Event::Text(text) if !text.trim_ascii().is_empty() => {
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
    facts: &[ResourceAnalysis],
    references: &mut Vec<AuthoredReference>,
    source: ResourceOrdinal,
    declared: &AuthoredHref,
    kind: HrefRole,
    source_path: &EpubPath,
    authored_base: Option<&AuthoredHref>,
    context: ReferenceContext,
) -> ReferenceSlot {
    let bases = authored_base.into_iter().collect::<Vec<_>>();
    let resolved = resolve_with_bases(source_path, &bases, declared);
    let target = href_target(resources, facts, declared, resolved);
    href_reference(references, source, declared.clone(), kind, target, context)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_svg_href_reference(
    resources: &ResourceIndex,
    facts: &[ResourceAnalysis],
    references: &mut Vec<AuthoredReference>,
    source: ResourceOrdinal,
    declared: &AuthoredHref,
    authored_bases: &[AuthoredHref],
    kind: HrefRole,
    source_path: &EpubPath,
    context: ReferenceContext,
) -> ReferenceSlot {
    let bases = authored_bases.iter().collect::<Vec<_>>();
    let resolved = resolve_with_bases(source_path, &bases, declared);
    let target = href_target(resources, facts, declared, resolved);
    href_reference(references, source, declared.clone(), kind, target, context)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_href_reference(
    resources: &ResourceIndex,
    facts: &[ResourceAnalysis],
    references: &mut Vec<AuthoredReference>,
    source: ResourceOrdinal,
    declared: &AuthoredHref,
    kind: HrefRole,
    source_path: &EpubPath,
    context: ReferenceContext,
) -> ReferenceSlot {
    let resolved = resolve_href(declared, source_path);
    let target = href_target(resources, facts, declared, resolved);
    href_reference(references, source, declared.clone(), kind, target, context)
}

fn resolve_with_bases(
    source_path: &EpubPath,
    bases: &[&AuthoredHref],
    declared: &AuthoredHref,
) -> Result<ResolvedHref, InvalidHref> {
    if bases.is_empty() {
        return resolve_href(declared, source_path);
    }
    resolve_href_with_bases(source_path, bases, declared)
}

fn href_target(
    resources: &ResourceIndex,
    facts: &[ResourceAnalysis],
    declared: &AuthoredHref,
    resolved: Result<ResolvedHref, InvalidHref>,
) -> HrefTarget {
    let Ok(ResolvedHref {
        address,
        query,
        fragment,
    }) = resolved
    else {
        return HrefTarget::Invalid(declared.clone());
    };
    match &address {
        ResourceAddress::Local(path) => {
            let Some(resource) = resources.resource_at(&address) else {
                return HrefTarget::MissingLocal(path.clone());
            };
            match fragment {
                Some(fragment) => HrefTarget::Fragment {
                    resource: resource.ordinal(),
                    query,
                    exists: fragment_exists(facts, resource.ordinal(), &fragment),
                    fragment,
                },
                None => HrefTarget::Resource {
                    resource: resource.ordinal(),
                    query,
                },
            }
        }
        ResourceAddress::Remote(href) => HrefTarget::Remote {
            href: match &fragment {
                Some(fragment) => format!(
                    "{href}#{}",
                    percent_encoding::utf8_percent_encode(fragment, FRAGMENT)
                ),
                None => href.clone(),
            },
            declared_resource: resources.resource_at(&address).map(ResourceRef::ordinal),
        },
        ResourceAddress::Data(value) => HrefTarget::Data(value.clone()),
        ResourceAddress::External(value) => HrefTarget::External(value.clone()),
    }
}

const FRAGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`');

pub(super) fn facts_for_row(
    facts: &[ResourceAnalysis],
    resource: ResourceOrdinal,
) -> Option<&ResourceAnalysis> {
    facts
        .get(resource.index())
        .filter(|facts| facts.resource() == resource)
}

fn fragment_exists(
    facts: &[ResourceAnalysis],
    resource: ResourceOrdinal,
    fragment: &str,
) -> Option<bool> {
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
    facts: &[ResourceAnalysis],
) -> Vec<ResourceCompleteness> {
    let mut seen = HashSet::new();
    references
        .iter()
        .filter_map(|reference| match reference {
            AuthoredReference::Href(reference) => match reference.target() {
                HrefTarget::Fragment { resource, .. } => Some(*resource),
                _ => None,
            },
            AuthoredReference::Manifest(_) => None,
        })
        .filter(|resource| {
            facts_for_row(facts, *resource)
                .is_some_and(|facts| !facts.content().is_not_applicable())
        })
        .filter(|resource| seen.insert(*resource))
        .map(|resource| ResourceCompleteness {
            resource,
            completeness: facts_for_row(facts, resource)
                .and_then(|facts| facts.content().completeness())
                .expect("fragment targets with applicable content have an outcome"),
        })
        .collect()
}
use super::*;
use crate::resource::base::resolve_href_with_bases;
use crate::resource::{InvalidHref, resolve_href};
