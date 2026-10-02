use crate::accessibility::{AccessibilityFact, AccessibilityObservation};
use crate::analysis::reference::HrefRole;
use crate::content::extraction::xhtml::{ElementAttrs, ExtractorState};
use crate::content::facts::LinkFact;
use crate::content::{
    FragmentAttribute, FragmentFact, ScriptFact, SvgFacts, SvgForeignObjectFact, SvgTextFact,
};
use crate::resource::AuthoredHref;
use crate::xml::{cdata_content, decode_xml, push_general_ref, text_content};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

#[cfg(test)]
thread_local! {
    static SCAN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const EPUB_NS: &str = "http://www.idpf.org/2007/ops";

#[derive(Debug, Clone, Default)]
pub(crate) struct SvgScan {
    is_svg: bool,
    width: Option<String>,
    height: Option<String>,
    view_box: Option<String>,
    accessibility_text: Vec<SvgText>,
    source_text: Vec<SvgSourceText>,
    fragments: Vec<FragmentFact>,
    scripts: Vec<ScriptFact>,
    foreign_objects: Vec<(usize, SvgForeignObjectFact)>,
    foreign_accessibility: Vec<(usize, AccessibilityFact)>,
    pending_references: Vec<SvgPendingRef>,
    activity: crate::content::DocumentActivities,
    semantic_issue: Option<crate::analysis::AnalysisIssue>,
    malformed: bool,
    root_closed: bool,
}

#[derive(Debug, Clone)]
struct SvgText {
    kind: SvgTextKind,
    value: String,
    source_fragment: Option<FragmentFact>,
    subject_element: String,
    subject_fragment: Option<FragmentFact>,
    root_child: bool,
    element_ordinal: usize,
}

#[derive(Debug, Clone)]
struct SvgSourceText {
    value: String,
    fragment: Option<FragmentFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SvgTextKind {
    Title,
    Description,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SvgPendingRef {
    declared: AuthoredHref,
    bases: Vec<AuthoredHref>,
    element: String,
    attribute: String,
    kind: HrefRole,
    foreign_object_ordinal: Option<usize>,
}

impl SvgPendingRef {
    pub(crate) fn declared(&self) -> &AuthoredHref {
        &self.declared
    }

    pub(crate) fn bases(&self) -> &[AuthoredHref] {
        &self.bases
    }

    pub(crate) fn element(&self) -> &str {
        &self.element
    }

    pub(crate) fn attribute(&self) -> &str {
        &self.attribute
    }

    pub(crate) fn kind(&self) -> HrefRole {
        self.kind
    }
}

impl SvgScan {
    pub(crate) fn is_svg(&self) -> bool {
        self.is_svg
    }

    pub(crate) fn is_malformed(&self) -> bool {
        self.malformed
    }

    pub(crate) fn semantic_issue(&self) -> Option<crate::analysis::AnalysisIssue> {
        self.semantic_issue
    }

    pub(crate) fn root_closed(&self) -> bool {
        self.root_closed
    }

    pub(crate) fn width(&self) -> Option<&str> {
        self.width.as_deref()
    }

    pub(crate) fn height(&self) -> Option<&str> {
        self.height.as_deref()
    }

    pub(crate) fn view_box(&self) -> Option<&str> {
        self.view_box.as_deref()
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.accessibility_text
            .iter()
            .find(|text| text.root_child && text.kind == SvgTextKind::Title)
            .map(|text| text.value.as_str())
    }

    pub(crate) fn description(&self) -> Option<&str> {
        self.accessibility_text
            .iter()
            .find(|text| text.root_child && text.kind == SvgTextKind::Description)
            .map(|text| text.value.as_str())
    }

    pub(crate) fn take_analysis(
        &mut self,
    ) -> (SvgFacts, Vec<AccessibilityFact>, Vec<SvgPendingRef>) {
        let fragments = std::mem::take(&mut self.fragments);
        let mut accessibility = self
            .accessibility_text
            .iter()
            .map(|text| {
                let source_fragment = text.source_fragment.clone();
                let value = text.value.clone();
                let observation = match text.kind {
                    SvgTextKind::Title => AccessibilityObservation::SvgTitle {
                        source_fragment,
                        value,
                    },
                    SvgTextKind::Description => AccessibilityObservation::SvgDescription {
                        source_fragment,
                        value,
                    },
                };
                let fact = AccessibilityFact::new(
                    text.subject_element.clone(),
                    text.subject_fragment.clone(),
                    observation,
                );
                (text.element_ordinal, fact)
            })
            .chain(std::mem::take(&mut self.foreign_accessibility))
            .collect::<Vec<_>>();
        accessibility.sort_by_key(|(ordinal, _)| *ordinal);
        let accessibility = accessibility.into_iter().map(|(_, fact)| fact).collect();
        let text = self
            .source_text
            .drain(..)
            .map(|text| SvgTextFact {
                text: text.value,
                fragment: text.fragment,
            })
            .collect();
        let mut foreign_objects = std::mem::take(&mut self.foreign_objects);
        foreign_objects.sort_by_key(|(ordinal, _)| *ordinal);
        let foreign_objects = foreign_objects.into_iter().map(|(_, fact)| fact).collect();
        (
            SvgFacts::new(
                fragments,
                text,
                std::mem::take(&mut self.scripts),
                foreign_objects,
                self.activity,
            ),
            accessibility,
            std::mem::take(&mut self.pending_references),
        )
    }
}

#[derive(Clone, Copy)]
struct SvgTextCapture {
    depth: usize,
    index: usize,
}

#[derive(Clone, Copy)]
struct SvgSourceTextCapture {
    depth: usize,
    index: usize,
}

#[derive(Clone, Copy)]
struct SvgScriptCapture {
    depth: usize,
    index: usize,
}

struct SvgStyleCapture {
    depth: usize,
    value: String,
    bases: Vec<AuthoredHref>,
}

struct ForeignObjectCapture {
    depth: usize,
    element_ordinal: usize,
    fragment: Option<FragmentFact>,
    projector: ExtractorState,
    projected_elements: Vec<Option<ProjectedForeignElement>>,
    inherited_base_count: usize,
    captured_accessibility: usize,
}

struct ProjectedForeignElement {
    name: String,
    ordinal: usize,
}

pub(crate) fn scan(bytes: &[u8]) -> SvgScan {
    #[cfg(test)]
    SCAN_COUNT.set(SCAN_COUNT.get() + 1);
    let Ok(decoded) = decode_xml(bytes) else {
        return SvgScan {
            malformed: true,
            ..Default::default()
        };
    };
    let mut reader = NsReader::from_reader(decoded.as_bytes());
    let mut scan = SvgScan::default();
    let mut depth = 0usize;
    let mut capture = None;
    let mut source_text_capture = None;
    let mut script_capture = None;
    let mut style_capture = None;
    let mut foreign_captures = Vec::<ForeignObjectCapture>::new();
    let mut nearest_fragment = None;
    let mut fragment_stack = Vec::new();
    let mut element_stack = Vec::new();
    let mut base_chain = Vec::new();
    let mut base_lengths = Vec::new();
    let mut seen_prolog_event = false;
    let mut seen_doctype = false;
    let mut next_element_ordinal = 0usize;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => {
                scan.malformed = true;
                break;
            }
        };
        let declaration_allowed = !seen_prolog_event;
        if !matches!(&event, Event::Decl(_)) {
            seen_prolog_event = true;
        }
        match event {
            Event::Start(element) => {
                let element_ordinal = next_element_ordinal;
                next_element_ordinal += 1;
                if scan.root_closed {
                    scan.malformed = true;
                    break;
                }
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                let in_svg_namespace = namespace_is(&namespace, SVG_NS);
                if in_svg_namespace && is_active_svg_element(local.as_ref()) {
                    scan.activity
                        .insert(crate::content::DocumentActivity::Animation);
                }
                if depth > 0
                    && foreign_captures.is_empty()
                    && !in_svg_namespace
                    && scan.semantic_issue.is_none()
                {
                    scan.semantic_issue = Some(crate::analysis::AnalysisIssue::Unsupported);
                }
                if depth == 0 {
                    scan.is_svg = in_svg_namespace && local.as_ref() == "svg";
                    if !scan.is_svg {
                        break;
                    }
                }
                let previous_fragment = nearest_fragment.clone();
                let previous_base_len = base_chain.len();
                let attributes = scan_attributes(
                    &mut scan,
                    &reader,
                    &element,
                    local.as_ref(),
                    element_ordinal,
                    in_svg_namespace,
                    depth == 0,
                    &mut nearest_fragment,
                    &base_chain,
                );
                if let Some(base) = attributes.base {
                    base_chain.push(base);
                }
                for foreign in &mut foreign_captures {
                    project_foreign_start(
                        &mut scan,
                        foreign,
                        &reader,
                        &element,
                        &namespace,
                        local.as_ref(),
                        element_ordinal,
                        true,
                        &base_chain,
                    );
                }
                if in_svg_namespace && local.as_ref() == "foreignObject" {
                    foreign_captures.push(ForeignObjectCapture {
                        depth: depth + 1,
                        element_ordinal,
                        fragment: nearest_fragment.clone(),
                        projector: ExtractorState::new_fragment(),
                        projected_elements: Vec::new(),
                        inherited_base_count: base_chain.len(),
                        captured_accessibility: 0,
                    });
                }
                if depth > 0 && in_svg_namespace && capture.is_none() {
                    let kind = match local.as_ref() {
                        "title" => Some(SvgTextKind::Title),
                        "desc" => Some(SvgTextKind::Description),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        let index = scan.accessibility_text.len();
                        scan.accessibility_text.push(SvgText {
                            kind,
                            value: String::new(),
                            source_fragment: nearest_fragment.clone(),
                            subject_element: element_stack
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "svg".to_string()),
                            subject_fragment: previous_fragment.clone(),
                            root_child: depth == 1,
                            element_ordinal,
                        });
                        capture = Some(SvgTextCapture {
                            depth: depth + 1,
                            index,
                        });
                    }
                }
                if depth > 0
                    && in_svg_namespace
                    && local.as_ref() == "text"
                    && source_text_capture.is_none()
                {
                    let index = scan.source_text.len();
                    scan.source_text.push(SvgSourceText {
                        value: String::new(),
                        fragment: nearest_fragment.clone(),
                    });
                    source_text_capture = Some(SvgSourceTextCapture {
                        depth: depth + 1,
                        index,
                    });
                }
                if in_svg_namespace && local.as_ref() == "script" {
                    let index = scan.scripts.len();
                    scan.scripts.push(svg_script_fact(
                        nearest_fragment.clone(),
                        attributes.script_type,
                        attributes.has_href,
                    ));
                    script_capture = Some(SvgScriptCapture {
                        depth: depth + 1,
                        index,
                    });
                }
                if in_svg_namespace && local.as_ref() == "style" && style_capture.is_none() {
                    style_capture = Some(SvgStyleCapture {
                        depth: depth + 1,
                        value: String::new(),
                        bases: base_chain.clone(),
                    });
                }
                fragment_stack.push(previous_fragment);
                base_lengths.push(previous_base_len);
                element_stack.push(local.as_ref().to_string());
                depth += 1;
            }
            Event::Empty(element) => {
                let element_ordinal = next_element_ordinal;
                next_element_ordinal += 1;
                if scan.root_closed {
                    scan.malformed = true;
                    break;
                }
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                let in_svg_namespace = namespace_is(&namespace, SVG_NS);
                if in_svg_namespace && is_active_svg_element(local.as_ref()) {
                    scan.activity
                        .insert(crate::content::DocumentActivity::Animation);
                }
                if depth > 0
                    && foreign_captures.is_empty()
                    && !in_svg_namespace
                    && scan.semantic_issue.is_none()
                {
                    scan.semantic_issue = Some(crate::analysis::AnalysisIssue::Unsupported);
                }
                if depth == 0 {
                    scan.is_svg = in_svg_namespace && local.as_ref() == "svg";
                    if !scan.is_svg {
                        break;
                    }
                }
                let mut element_fragment = nearest_fragment.clone();
                let attributes = scan_attributes(
                    &mut scan,
                    &reader,
                    &element,
                    local.as_ref(),
                    element_ordinal,
                    in_svg_namespace,
                    depth == 0,
                    &mut element_fragment,
                    &base_chain,
                );
                let mut effective_bases = base_chain.clone();
                if let Some(base) = &attributes.base {
                    effective_bases.push(base.clone());
                }
                for foreign in &mut foreign_captures {
                    project_foreign_start(
                        &mut scan,
                        foreign,
                        &reader,
                        &element,
                        &namespace,
                        local.as_ref(),
                        element_ordinal,
                        false,
                        &effective_bases,
                    );
                }
                if in_svg_namespace && local.as_ref() == "foreignObject" {
                    finish_foreign_object(
                        &mut scan,
                        ForeignObjectCapture {
                            depth: depth + 1,
                            element_ordinal,
                            fragment: element_fragment.clone(),
                            projector: ExtractorState::new_fragment(),
                            projected_elements: Vec::new(),
                            inherited_base_count: effective_bases.len(),
                            captured_accessibility: 0,
                        },
                    );
                }
                if depth > 0 && in_svg_namespace && capture.is_none() {
                    let kind = match local.as_ref() {
                        "title" => Some(SvgTextKind::Title),
                        "desc" => Some(SvgTextKind::Description),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        scan.accessibility_text.push(SvgText {
                            kind,
                            value: String::new(),
                            source_fragment: element_fragment.clone(),
                            subject_element: element_stack
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "svg".to_string()),
                            subject_fragment: nearest_fragment.clone(),
                            root_child: depth == 1,
                            element_ordinal,
                        });
                    }
                }
                if depth > 0 && in_svg_namespace && local.as_ref() == "text" {
                    scan.source_text.push(SvgSourceText {
                        value: String::new(),
                        fragment: element_fragment.clone(),
                    });
                }
                if in_svg_namespace && local.as_ref() == "script" {
                    scan.scripts.push(svg_script_fact(
                        element_fragment,
                        attributes.script_type,
                        attributes.has_href,
                    ));
                }
                if depth == 0 {
                    scan.root_closed = true;
                }
            }
            Event::Text(text) => {
                if foreign_captures.iter().any(|foreign| {
                    foreign
                        .projected_elements
                        .last()
                        .is_some_and(Option::is_some)
                }) {
                    let value = text_content(&text);
                    for foreign in &mut foreign_captures {
                        if foreign
                            .projected_elements
                            .last()
                            .is_some_and(Option::is_some)
                        {
                            foreign.projector.push_text(&value);
                        }
                    }
                }
                if let Some(capture) = capture {
                    let value = text_content(&text);
                    captured_text_mut(&mut scan, capture).push_str(&value);
                } else if depth == 0 && !text.trim_ascii().is_empty() {
                    scan.malformed |= scan.root_closed;
                    break;
                }
                if let Some(capture) = source_text_capture {
                    let value = text_content(&text);
                    scan.source_text[capture.index].value.push_str(&value);
                }
                if let Some(capture) = script_capture
                    && !text.trim_ascii().is_empty()
                {
                    scan.scripts[capture.index].mark_text();
                }
                if let Some(capture) = &mut style_capture {
                    let value = text_content(&text);
                    capture.value.push_str(&value);
                }
            }
            Event::CData(text) => {
                if foreign_captures.iter().any(|foreign| {
                    foreign
                        .projected_elements
                        .last()
                        .is_some_and(Option::is_some)
                }) {
                    let value = cdata_content(&text);
                    for foreign in &mut foreign_captures {
                        if foreign
                            .projected_elements
                            .last()
                            .is_some_and(Option::is_some)
                        {
                            foreign.projector.push_text(&value);
                        }
                    }
                }
                if let Some(capture) = capture {
                    let value = cdata_content(&text);
                    captured_text_mut(&mut scan, capture).push_str(&value);
                } else if depth == 0 {
                    scan.malformed = true;
                    break;
                }
                if let Some(capture) = source_text_capture {
                    let value = cdata_content(&text);
                    scan.source_text[capture.index].value.push_str(&value);
                }
                if let Some(capture) = script_capture
                    && !text.trim_ascii().is_empty()
                {
                    scan.scripts[capture.index].mark_text();
                }
                if let Some(capture) = &mut style_capture {
                    let value = cdata_content(&text);
                    capture.value.push_str(&value);
                }
            }
            Event::GeneralRef(reference) => {
                if foreign_captures.iter().any(|foreign| {
                    foreign
                        .projected_elements
                        .last()
                        .is_some_and(Option::is_some)
                }) {
                    let mut value = String::new();
                    match push_general_ref(&mut value, &reference) {
                        Ok(false) => {
                            for foreign in &mut foreign_captures {
                                if foreign
                                    .projected_elements
                                    .last()
                                    .is_some_and(Option::is_some)
                                {
                                    foreign.projector.push_text(&value);
                                }
                            }
                        }
                        Ok(true) | Err(_) => scan.malformed = true,
                    }
                }
                if depth == 0 {
                    scan.malformed = true;
                    break;
                }
                let result = if let Some(capture) = capture {
                    push_general_ref(captured_text_mut(&mut scan, capture), &reference)
                } else {
                    push_general_ref(&mut String::new(), &reference)
                };
                scan.malformed |= !matches!(result, Ok(false));
                if let Some(capture) = source_text_capture {
                    scan.malformed |= !matches!(
                        push_general_ref(&mut scan.source_text[capture.index].value, &reference),
                        Ok(false)
                    );
                }
                if let Some(capture) = script_capture {
                    let mut value = String::new();
                    if matches!(push_general_ref(&mut value, &reference), Ok(false))
                        && !value.chars().all(char::is_whitespace)
                    {
                        scan.scripts[capture.index].mark_text();
                    }
                }
                if let Some(capture) = &mut style_capture {
                    scan.malformed |=
                        !matches!(push_general_ref(&mut capture.value, &reference), Ok(false));
                }
            }
            Event::End(element) => {
                if depth == 0 {
                    scan.malformed = true;
                    break;
                }
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                let closing_capture = foreign_captures.iter().rposition(|foreign| {
                    foreign.depth == depth
                        && namespace_is(&namespace, SVG_NS)
                        && local.as_ref() == "foreignObject"
                });
                if let Some(index) = closing_capture {
                    finish_foreign_object(&mut scan, foreign_captures.remove(index));
                }
                for foreign in &mut foreign_captures {
                    if let Some(projected) = foreign.projected_elements.pop().flatten() {
                        foreign.projector.handle_end(&projected.name);
                        collect_foreign_accessibility(&mut scan, foreign, projected.ordinal);
                    }
                }
                if capture.is_some_and(|capture| capture.depth == depth) {
                    capture = None;
                }
                if source_text_capture.is_some_and(|capture| capture.depth == depth) {
                    source_text_capture = None;
                }
                if script_capture.is_some_and(|capture| capture.depth == depth) {
                    script_capture = None;
                }
                if style_capture
                    .as_ref()
                    .is_some_and(|capture| capture.depth == depth)
                {
                    let capture = style_capture.take().expect("matching style capture exists");
                    collect_embedded_style(&mut scan, capture);
                }
                depth -= 1;
                nearest_fragment = fragment_stack.pop().flatten();
                base_chain.truncate(base_lengths.pop().unwrap_or_default());
                element_stack.pop();
                if depth == 0 {
                    scan.root_closed = true;
                }
            }
            Event::Decl(_) => {
                if !declaration_allowed || depth > 0 || scan.is_svg || scan.root_closed {
                    scan.malformed = true;
                    break;
                }
                seen_prolog_event = true;
            }
            Event::DocType(_) => {
                if seen_doctype || depth > 0 || scan.is_svg || scan.root_closed {
                    scan.malformed = true;
                    break;
                }
                seen_doctype = true;
                scan.activity
                    .insert(crate::content::DocumentActivity::Doctype);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    scan
}

#[cfg(test)]
pub(crate) fn reset_scan_count() {
    SCAN_COUNT.set(0);
}

#[cfg(test)]
pub(crate) fn scan_count() -> usize {
    SCAN_COUNT.get()
}

pub(crate) fn has_svg_root(bytes: &[u8]) -> bool {
    let Ok(decoded) = decode_xml(bytes) else {
        return false;
    };
    let mut reader = NsReader::from_reader(decoded.as_bytes());
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                return namespace_is(&namespace, SVG_NS) && local.as_ref() == "svg";
            }
            Ok(Event::Text(text)) if !text.trim_ascii().is_empty() => return false,
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_foreign_start(
    scan: &mut SvgScan,
    foreign: &mut ForeignObjectCapture,
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
    namespace: &ResolveResult<'_>,
    local: &str,
    element_ordinal: usize,
    has_end: bool,
    bases: &[AuthoredHref],
) {
    if !namespace_is(namespace, XHTML_NS)
        && !namespace_is(namespace, SVG_NS)
        && scan.semantic_issue.is_none()
    {
        scan.semantic_issue = Some(crate::analysis::AnalysisIssue::Unsupported);
    }
    if foreign
        .projected_elements
        .last()
        .is_some_and(Option::is_none)
    {
        if has_end {
            foreign.projected_elements.push(None);
        }
        return;
    }
    if namespace_is(namespace, XHTML_NS) {
        let (attrs, malformed) = foreign_element_attrs(reader, element);
        scan.malformed |= malformed;
        if (local == "style" || attrs.value("style").is_some()) && scan.semantic_issue.is_none() {
            scan.semantic_issue = Some(crate::analysis::AnalysisIssue::Unsupported);
        }
        let element_name = local.to_string();
        let first_link = foreign.projector.links_len();
        foreign
            .projector
            .handle_start(&element_name, false, true, &attrs, true);
        collect_foreign_accessibility(scan, foreign, element_ordinal);
        let links = foreign
            .projector
            .links_from(first_link)
            .iter()
            .map(|link| {
                (
                    link.declared().clone(),
                    link.element().to_string(),
                    link.attribute().as_str().to_string(),
                    foreign_link_role(link),
                )
            })
            .collect::<Vec<_>>();
        scan.pending_references.extend(links.into_iter().map(
            |(declared, element, attribute, kind)| SvgPendingRef {
                declared,
                bases: bases.to_vec(),
                element,
                attribute,
                kind,
                foreign_object_ordinal: Some(foreign.element_ordinal),
            },
        ));
        if has_end {
            foreign
                .projected_elements
                .push(Some(ProjectedForeignElement {
                    name: element_name,
                    ordinal: element_ordinal,
                }));
        } else {
            foreign.projector.handle_end(&element_name);
            collect_foreign_accessibility(scan, foreign, element_ordinal);
        }
    } else if has_end {
        foreign.projected_elements.push(None);
    }
}

fn foreign_element_attrs(
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
) -> (ElementAttrs, bool) {
    let mut values = Vec::new();
    let mut malformed = false;
    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            malformed = true;
            continue;
        };
        let (namespace, local) = reader.resolver().resolve_attribute(attribute.key);
        let local = local.as_ref();
        let name = if namespace_is(&namespace, XML_NS) {
            format!("xml:{local}")
        } else if namespace_is(&namespace, XLINK_NS) {
            format!("xlink:{local}")
        } else if namespace_is(&namespace, EPUB_NS) {
            format!("epub:{local}")
        } else {
            attribute.key.as_ref().to_string()
        };
        match attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
            Ok(value) => values.push((name, value.into_owned())),
            Err(_) => malformed = true,
        }
    }
    (ElementAttrs::from_values(values), malformed)
}

fn svg_script_fact(
    fragment: Option<FragmentFact>,
    script_type: Option<String>,
    has_href: bool,
) -> ScriptFact {
    if !super::xhtml::is_executable_script_type(script_type.as_deref()) {
        ScriptFact::DataBlock {
            fragment,
            script_type,
            has_text: false,
        }
    } else if has_href {
        ScriptFact::External {
            fragment,
            script_type,
        }
    } else {
        ScriptFact::Inline {
            fragment,
            script_type,
            has_text: false,
        }
    }
}

fn foreign_link_role(link: &LinkFact) -> HrefRole {
    match link {
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
    }
}

fn finish_foreign_object(scan: &mut SvgScan, foreign: ForeignObjectCapture) {
    let captured_accessibility = foreign.captured_accessibility;
    let element_ordinal = foreign.element_ordinal;
    let fragment = foreign.fragment;
    let extraction = foreign.projector.finish(false);
    if let Some(authored_base) = extraction.authored_base {
        for pending in scan
            .pending_references
            .iter_mut()
            .filter(|pending| pending.foreign_object_ordinal == Some(element_ordinal))
        {
            pending.bases.insert(
                foreign.inherited_base_count.min(pending.bases.len()),
                authored_base.clone(),
            );
        }
    }
    scan.foreign_accessibility.extend(
        extraction
            .accessibility
            .into_iter()
            .skip(captured_accessibility)
            .map(|fact| (element_ordinal, fact)),
    );
    scan.foreign_objects.push((
        element_ordinal,
        SvgForeignObjectFact {
            fragment,
            xhtml: extraction.facts,
        },
    ));
}

fn collect_foreign_accessibility(
    scan: &mut SvgScan,
    foreign: &mut ForeignObjectCapture,
    element_ordinal: usize,
) {
    scan.foreign_accessibility.extend(
        foreign
            .projector
            .accessibility_from(foreign.captured_accessibility)
            .iter()
            .cloned()
            .map(|fact| (element_ordinal, fact)),
    );
    foreign.captured_accessibility = foreign.projector.accessibility_len();
}

#[allow(clippy::too_many_arguments)]
fn scan_attributes(
    scan: &mut SvgScan,
    reader: &NsReader<&[u8]>,
    element: &BytesStart<'_>,
    local: &str,
    element_ordinal: usize,
    in_svg_namespace: bool,
    is_root: bool,
    nearest_fragment: &mut Option<FragmentFact>,
    inherited_bases: &[AuthoredHref],
) -> ScannedAttributes {
    #[derive(Clone)]
    enum RecognizedAttribute {
        Id,
        XmlId,
        XmlBase,
        Width,
        Height,
        ViewBox,
        Href,
        XlinkHref,
        ScriptType,
        EventHandler(String),
        FunctionalIri(String),
    }

    let element_name = local.to_string();
    let supports_href = in_svg_namespace && supports_svg_href(local);
    let mut values = Vec::new();
    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            scan.malformed = true;
            continue;
        };
        let (namespace, attribute_local) = reader.resolver().resolve_attribute(attribute.key);
        let unbound = matches!(namespace, ResolveResult::Unbound);
        let xml = namespace_is(&namespace, XML_NS);
        let xlink = namespace_is(&namespace, XLINK_NS);
        let kind = match (unbound, xml, xlink, attribute_local.as_ref()) {
            (true, _, _, "id") => RecognizedAttribute::Id,
            (_, true, _, "id") => RecognizedAttribute::XmlId,
            (_, true, _, "base") => RecognizedAttribute::XmlBase,
            (true, _, _, "width") => RecognizedAttribute::Width,
            (true, _, _, "height") => RecognizedAttribute::Height,
            (true, _, _, "viewBox") => RecognizedAttribute::ViewBox,
            (true, _, _, "href") if supports_href => RecognizedAttribute::Href,
            (_, _, true, "href") if supports_href => RecognizedAttribute::XlinkHref,
            (true, _, _, "type") if in_svg_namespace && local == "script" => {
                RecognizedAttribute::ScriptType
            }
            (true, _, _, name) if in_svg_namespace && is_svg_event_handler(name) => {
                RecognizedAttribute::EventHandler(name.to_string())
            }
            (true, _, _, name) if in_svg_namespace && is_functional_iri_attribute(name) => {
                RecognizedAttribute::FunctionalIri(name.to_string())
            }
            _ => continue,
        };
        let value = match attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
            Ok(value) => value.into_owned(),
            Err(_) => {
                scan.malformed = true;
                continue;
            }
        };
        values.push((kind, value));
    }
    let own_base = values.iter().find_map(|(kind, value)| {
        matches!(kind, RecognizedAttribute::XmlBase).then(|| AuthoredHref::new(value.clone()))
    });
    let mut effective_bases = inherited_bases.to_vec();
    if let Some(base) = &own_base {
        effective_bases.push(base.clone());
    }
    for (kind, value) in &values {
        match kind {
            RecognizedAttribute::Id => {
                let fragment = FragmentFact::new(
                    value.clone(),
                    element_name.clone(),
                    FragmentAttribute::Id,
                    element_ordinal,
                );
                scan.fragments.push(fragment.clone());
                *nearest_fragment = Some(fragment);
            }
            RecognizedAttribute::XmlId => {
                let fragment = FragmentFact::new(
                    value.clone(),
                    element_name.clone(),
                    FragmentAttribute::XmlId,
                    element_ordinal,
                );
                scan.fragments.push(fragment.clone());
                *nearest_fragment = Some(fragment);
            }
            RecognizedAttribute::Width if is_root => scan.width = Some(value.clone()),
            RecognizedAttribute::Height if is_root => scan.height = Some(value.clone()),
            RecognizedAttribute::ViewBox if is_root => scan.view_box = Some(value.clone()),
            RecognizedAttribute::XmlBase
            | RecognizedAttribute::Href
            | RecognizedAttribute::XlinkHref
            | RecognizedAttribute::ScriptType
            | RecognizedAttribute::EventHandler(_)
            | RecognizedAttribute::FunctionalIri(_)
            | RecognizedAttribute::Width
            | RecognizedAttribute::Height
            | RecognizedAttribute::ViewBox => {}
        }
    }
    let selected_href = values
        .iter()
        .find(|(kind, _)| matches!(kind, RecognizedAttribute::Href))
        .or_else(|| {
            values
                .iter()
                .find(|(kind, _)| matches!(kind, RecognizedAttribute::XlinkHref))
        });
    if let Some((attribute_kind, value)) = selected_href {
        let kind = href_role(local);
        scan.pending_references.push(SvgPendingRef {
            declared: AuthoredHref::new(value.clone()),
            bases: effective_bases.clone(),
            element: element_name.clone(),
            attribute: match attribute_kind {
                RecognizedAttribute::Href => "href".to_string(),
                RecognizedAttribute::XlinkHref => "xlink:href".to_string(),
                _ => unreachable!(),
            },
            kind,
            foreign_object_ordinal: None,
        });
    }
    for (kind, value) in &values {
        match kind {
            RecognizedAttribute::EventHandler(attribute) => {
                scan.scripts.push(ScriptFact::EventHandler {
                    element: element_name.clone(),
                    fragment: nearest_fragment.clone(),
                    attribute: attribute.clone(),
                });
            }
            RecognizedAttribute::FunctionalIri(attribute) => {
                for href in functional_iris(value) {
                    scan.pending_references.push(SvgPendingRef {
                        declared: AuthoredHref::new(href),
                        bases: effective_bases.clone(),
                        element: element_name.clone(),
                        attribute: attribute.clone(),
                        kind: if attribute == "style" {
                            HrefRole::CssUrl
                        } else {
                            HrefRole::Svg
                        },
                        foreign_object_ordinal: None,
                    });
                }
            }
            _ => {}
        }
    }
    ScannedAttributes {
        base: own_base,
        has_href: selected_href.is_some(),
        script_type: values.iter().find_map(|(kind, value)| {
            matches!(kind, RecognizedAttribute::ScriptType).then(|| value.clone())
        }),
    }
}

struct ScannedAttributes {
    base: Option<AuthoredHref>,
    has_href: bool,
    script_type: Option<String>,
}

fn supports_svg_href(local: &str) -> bool {
    matches!(
        local,
        "a" | "animate"
            | "animateMotion"
            | "animateTransform"
            | "discard"
            | "feImage"
            | "image"
            | "linearGradient"
            | "mpath"
            | "pattern"
            | "radialGradient"
            | "script"
            | "set"
            | "textPath"
            | "use"
    )
}

fn is_active_svg_element(local: &str) -> bool {
    matches!(
        local,
        "animate" | "animateColor" | "animateMotion" | "animateTransform" | "discard" | "set"
    )
}

fn href_role(local: &str) -> HrefRole {
    match local {
        "a" => HrefRole::Hyperlink,
        "image" | "feImage" => HrefRole::Image,
        "script" => HrefRole::Script,
        _ => HrefRole::Svg,
    }
}

fn is_functional_iri_attribute(local: &str) -> bool {
    matches!(
        local,
        "clip-path"
            | "cursor"
            | "fill"
            | "filter"
            | "marker"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "mask"
            | "stroke"
            | "style"
    )
}

fn is_svg_event_handler(local: &str) -> bool {
    local.bytes().all(|byte| !byte.is_ascii_uppercase())
        && crate::content::extraction::xhtml::is_event_handler_attr(local)
}

fn functional_iris(value: &str) -> Vec<String> {
    let mut references = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("url(") {
        rest = &rest[start + 4..];
        let Some(end) = rest.find(')') else {
            break;
        };
        let href = rest[..end].trim();
        let href = href.trim_matches(|character| matches!(character, '\'' | '"'));
        if !href.is_empty() {
            references.push(href.to_string());
        }
        rest = &rest[end + 1..];
    }
    references
}

fn collect_embedded_style(scan: &mut SvgScan, capture: SvgStyleCapture) {
    match crate::content::extraction::css::extract(capture.value.as_bytes()) {
        Ok(extraction) => {
            if scan.semantic_issue.is_none() {
                scan.semantic_issue = extraction.issue;
            }
            scan.pending_references
                .extend(
                    extraction
                        .references
                        .into_iter()
                        .map(|reference| SvgPendingRef {
                            declared: reference.declared,
                            bases: capture.bases.clone(),
                            element: "style".to_string(),
                            attribute: "text".to_string(),
                            kind: reference.kind,
                            foreign_object_ordinal: None,
                        }),
                );
        }
        Err(issue) => {
            if scan.semantic_issue.is_none() {
                scan.semantic_issue = Some(issue);
            }
        }
    }
}

fn namespace_is(namespace: &ResolveResult<'_>, expected: &str) -> bool {
    matches!(namespace, ResolveResult::Bound(value) if value.as_ref() == expected)
}

fn captured_text_mut(scan: &mut SvgScan, capture: SvgTextCapture) -> &mut String {
    &mut scan.accessibility_text[capture.index].value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_foreign_object_xhtml_facts_references_and_inherited_bases() {
        let mut scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg" xml:base="../">
                <foreignObject id="foreign" xml:base="content/">
                    <html xmlns="http://www.w3.org/1999/xhtml">
                        <body onload="boot()">
                            <section id="chapter" xml:base="assets/">
                                <h1>Foreign title</h1>
                                <img src="cover.png" alt="Cover"/>
                                <script src="app.js"></script>
                                <form action="submit"><button>Send</button></form>
                            </section>
                        </body>
                    </html>
                </foreignObject>
            </svg>"##,
        );

        assert!(scan.is_svg());
        assert!(!scan.is_malformed());
        assert_eq!(scan.semantic_issue(), None);
        let (facts, accessibility, references) = scan.take_analysis();
        let foreign = &facts.foreign_objects()[0];
        assert_eq!(foreign.fragment().map(FragmentFact::id), Some("foreign"));
        assert_eq!(foreign.xhtml().text_stream().text(), "Foreign title\nSend");
        assert_eq!(foreign.xhtml().fragments().len(), 1);
        assert_eq!(foreign.xhtml().fragments()[0].id(), "chapter");
        assert_eq!(foreign.xhtml().media().len(), 1);
        assert_eq!(foreign.xhtml().forms().len(), 2);
        assert_eq!(foreign.xhtml().scripts().len(), 2);
        assert!(
            foreign
                .xhtml()
                .structure()
                .iter()
                .any(|fact| matches!(fact.role(), crate::content::StructureRole::Heading(_)))
        );

        assert_eq!(references.len(), 3);
        assert_eq!(
            references
                .iter()
                .map(|reference| (
                    reference.declared().as_str(),
                    reference.kind(),
                    reference
                        .bases()
                        .iter()
                        .map(AuthoredHref::as_str)
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "cover.png",
                    HrefRole::Image,
                    vec!["../", "content/", "assets/"]
                ),
                (
                    "app.js",
                    HrefRole::Script,
                    vec!["../", "content/", "assets/"]
                ),
                (
                    "submit",
                    HrefRole::FormAction,
                    vec!["../", "content/", "assets/"]
                ),
            ]
        );
        assert!(accessibility.iter().any(|fact| matches!(
            fact,
            AccessibilityFact {
                observation: AccessibilityObservation::ImageAlt(_),
                ..
            }
        )));
    }

    #[test]
    fn foreign_object_occurrences_are_ordered_and_unknown_namespaces_are_partial() {
        let mut scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg">
                <foreignObject id="one"/>
                <foreignObject id="two"><div xmlns="http://www.w3.org/1999/xhtml">Two</div></foreignObject>
                <foreignObject id="three"><widget xmlns="urn:example"><link href="missed"/></widget></foreignObject>
            </svg>"##,
        );

        assert_eq!(
            scan.semantic_issue(),
            Some(crate::analysis::AnalysisIssue::Unsupported)
        );
        let (facts, _, references) = scan.take_analysis();
        assert_eq!(
            facts
                .foreign_objects()
                .iter()
                .map(|foreign| foreign.fragment().map(FragmentFact::id))
                .collect::<Vec<_>>(),
            [Some("one"), Some("two"), Some("three")]
        );
        assert!(
            facts.foreign_objects()[0]
                .xhtml()
                .text_stream()
                .text()
                .is_empty()
        );
        assert_eq!(
            facts.foreign_objects()[1].xhtml().text_stream().text(),
            "Two"
        );
        assert!(references.is_empty());
    }

    #[test]
    fn foreign_object_applies_xhtml_base_after_inherited_xml_bases() {
        let mut scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg" xml:base="../">
                <foreignObject xml:base="content/">
                    <html xmlns="http://www.w3.org/1999/xhtml">
                        <head><base href="book/"/><base href="ignored/"/></head>
                        <body><img xml:base="images/" src="cover.png"/></body>
                    </html>
                </foreignObject>
            </svg>"##,
        );

        let (_, _, references) = scan.take_analysis();
        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0]
                .bases()
                .iter()
                .map(AuthoredHref::as_str)
                .collect::<Vec<_>>(),
            ["../", "content/", "book/", "images/"]
        );
    }

    #[test]
    fn unknown_namespace_beneath_nested_svg_marks_foreign_object_partial() {
        let scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject>
                <div xmlns="http://www.w3.org/1999/xhtml"><svg xmlns="http://www.w3.org/2000/svg">
                    <widget xmlns="urn:example"><link href="missed"/></widget>
                </svg></div>
            </foreignObject></svg>"##,
        );

        assert_eq!(
            scan.semantic_issue(),
            Some(crate::analysis::AnalysisIssue::Unsupported)
        );
    }

    #[test]
    fn self_closing_non_void_foreign_xhtml_elements_are_finalized() {
        let mut scan = scan(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject>
                <h1 xmlns="http://www.w3.org/1999/xhtml"/>
            </foreignObject></svg>"#,
        );

        let (facts, accessibility, _) = scan.take_analysis();
        assert!(matches!(
            facts.foreign_objects()[0].xhtml().structure()[0].role(),
            crate::content::StructureRole::Heading(_)
        ));
        assert!(accessibility.iter().any(|fact| matches!(
            fact,
            AccessibilityFact {
                observation: AccessibilityObservation::EmptyHeading,
                ..
            }
        )));
    }

    #[test]
    fn nested_svg_in_foreign_object_is_extracted_once_by_the_svg_scanner() {
        let mut scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg">
                <foreignObject><div xmlns="http://www.w3.org/1999/xhtml" aria-label="Outer label">Outer
                    <svg xmlns="http://www.w3.org/2000/svg"><title>Nested title</title><image href="nested.png"/>
                        <foreignObject id="inner"><p xmlns="http://www.w3.org/1999/xhtml">Inner</p></foreignObject>
                    </svg>
                    <img src="html.png" alt="After"/>
                </div></foreignObject>
            </svg>"##,
        );

        let (facts, accessibility, references) = scan.take_analysis();
        assert_eq!(facts.foreign_objects().len(), 2);
        assert_eq!(
            facts.foreign_objects()[0].xhtml().text_stream().text(),
            "Outer"
        );
        assert_eq!(
            facts.foreign_objects()[1].fragment().map(FragmentFact::id),
            Some("inner")
        );
        assert_eq!(
            facts.foreign_objects()[1].xhtml().text_stream().text(),
            "Inner"
        );
        assert_eq!(facts.foreign_objects()[0].xhtml().media().len(), 1);
        assert!(
            facts
                .fragments()
                .iter()
                .any(|fragment| fragment.id() == "inner")
        );
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.declared().as_str())
                .collect::<Vec<_>>(),
            ["nested.png", "html.png"]
        );
        assert!(matches!(
            accessibility[0],
            AccessibilityFact {
                observation: AccessibilityObservation::AriaLabel(_),
                ..
            }
        ));
        assert!(matches!(
            accessibility[1],
            AccessibilityFact {
                observation: AccessibilityObservation::SvgTitle { .. },
                ..
            }
        ));
        assert!(matches!(
            accessibility[2],
            AccessibilityFact {
                observation: AccessibilityObservation::ImageAlt(_),
                ..
            }
        ));
    }

    #[test]
    fn foreign_namespaces_outside_foreign_object_make_svg_partial() {
        let scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg"><x:widget xmlns:x="urn:example" href="missed.png"/></svg>"##,
        );
        assert_eq!(
            scan.semantic_issue(),
            Some(crate::analysis::AnalysisIssue::Unsupported)
        );
    }

    #[test]
    fn foreign_object_css_marks_svg_partial_until_css_references_are_projected() {
        for bytes in [
            br#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><div xmlns="http://www.w3.org/1999/xhtml" style="background:url(image.png)"/></foreignObject></svg>"#.as_slice(),
            br#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><style xmlns="http://www.w3.org/1999/xhtml">body { background:url(image.png) }</style></foreignObject></svg>"#.as_slice(),
        ] {
            let scan = scan(bytes);
            assert_eq!(
                scan.semantic_issue(),
                Some(crate::analysis::AnalysisIssue::Unsupported)
            );
        }
    }

    #[test]
    fn resolves_element_and_attribute_namespaces() {
        let mut scan = scan(br##"<s:svg xmlns:s="http://www.w3.org/2000/svg" xmlns:l="http://www.w3.org/1999/xlink" xml:base="../" width="10" height="20" viewBox="0 0 10 20"><s:g id="plain" xml:id="xml" xml:base="assets/"><s:a l:href="chapter.xhtml" href="#plain"/><s:image xmlns:l="urn:not-xlink" l:href="ignored"/><s:use xmlns:q="http://www.w3.org/1999/xlink" q:href="#xml"/></s:g></s:svg>"##);

        assert!(scan.is_svg());
        assert_eq!((scan.width(), scan.height()), (Some("10"), Some("20")));
        assert_eq!(scan.view_box(), Some("0 0 10 20"));
        assert_eq!(scan.fragments.len(), 2);
        let (_, _, references) = scan.take_analysis();
        assert_eq!(
            references
                .iter()
                .map(|reference| (
                    reference.element(),
                    reference.attribute(),
                    reference.declared().as_str(),
                    reference
                        .bases()
                        .iter()
                        .map(AuthoredHref::as_str)
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            vec![
                ("a", "href", "#plain", vec!["../", "assets/"]),
                ("use", "xlink:href", "#xml", vec!["../", "assets/"]),
            ]
        );
    }

    #[test]
    fn extracts_external_scripts_with_href_precedence() {
        let mut scan = scan(
            br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" xml:base="../scripts/"><g xml:base="modules/"><script href="main.js" xlink:href="ignored.js"/><script xlink:href="legacy.js"/></g></svg>"##,
        );

        let (_, _, references) = scan.take_analysis();
        assert_eq!(
            references
                .iter()
                .map(|reference| (
                    reference.element(),
                    reference.attribute(),
                    reference.declared().as_str(),
                    reference
                        .bases()
                        .iter()
                        .map(AuthoredHref::as_str)
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            [
                ("script", "href", "main.js", vec!["../scripts/", "modules/"]),
                (
                    "script",
                    "xlink:href",
                    "legacy.js",
                    vec!["../scripts/", "modules/"]
                ),
            ]
        );
    }

    #[test]
    fn inline_svg_script_character_references_count_as_text() {
        let mut scan =
            scan(br#"<svg xmlns="http://www.w3.org/2000/svg"><script>&#x61;&amp;</script></svg>"#);
        let (facts, _, _) = scan.take_analysis();

        assert!(matches!(
            facts.scripts(),
            [ScriptFact::Inline { has_text: true, .. }]
        ));
    }

    #[test]
    fn builds_fragments_and_accessibility_from_first_title_and_desc() {
        let mut scan = scan(br#"<svg xmlns="http://www.w3.org/2000/svg" id="root"><title id="title">A &amp; <tspan>nested</tspan> title</title><title>ignored</title><desc xml:id="desc"><![CDATA[Useful]]> description</desc></svg>"#);
        let (facts, accessibility, references) = scan.take_analysis();

        assert!(references.is_empty());
        assert_eq!(facts.fragments().len(), 3);
        assert_eq!(facts.fragments()[0].element(), "svg");
        assert_eq!(facts.fragments()[2].element(), "desc");
        assert_eq!(facts.fragments()[2].id(), "desc");
        assert_eq!(accessibility.len(), 3);
        let title = &accessibility[0];
        let AccessibilityObservation::SvgTitle {
            source_fragment,
            value,
        } = title.observation()
        else {
            panic!()
        };
        assert_eq!(value, "A & nested title");
        assert_eq!(
            source_fragment.as_ref().map(FragmentFact::id),
            Some("title")
        );
        assert_eq!(title.element(), "svg");
        assert_eq!(title.fragment().map(FragmentFact::id), Some("root"));
        let description = &accessibility[2];
        let AccessibilityObservation::SvgDescription {
            source_fragment,
            value,
        } = description.observation()
        else {
            panic!()
        };
        assert_eq!(value, "Useful description");
        assert_eq!(source_fragment.as_ref().map(FragmentFact::id), Some("desc"));
        assert_eq!(description.element(), "svg");
    }

    #[test]
    fn extracts_text_scripts_accessible_subjects_and_broader_dependencies() {
        let mut scan = scan(
            br#"<svg xmlns="http://www.w3.org/2000/svg" id="root" onload="start()">
                <g id="shape">
                    <title id="label">Named shape</title>
                    <text id="copy">Hello <tspan>SVG</tspan></text>
                    <script type="application/ecmascript">run()</script>
                    <style>.shape { mask: url(masks.svg#soft) }</style>
                    <rect onclick="paint()" fill="url(#paint)" style="filter: url('filters.svg#blur')"/>
                    <linearGradient href="palette.svg#main"/>
                </g>
            </svg>"#,
        );
        let (facts, accessibility, references) = scan.take_analysis();

        assert_eq!(facts.text().len(), 1);
        assert_eq!(facts.text()[0].text(), "Hello SVG");
        assert_eq!(
            facts.text()[0].fragment().map(FragmentFact::id),
            Some("copy")
        );
        assert_eq!(facts.scripts().len(), 3);
        assert!(matches!(
            &facts.scripts()[1],
            ScriptFact::Inline {
                script_type: Some(script_type),
                has_text: true,
                ..
            } if script_type == "application/ecmascript"
        ));

        let title = &accessibility[0];
        let AccessibilityObservation::SvgTitle {
            source_fragment, ..
        } = title.observation()
        else {
            panic!()
        };
        assert_eq!(title.element(), "g");
        assert_eq!(title.fragment().map(FragmentFact::id), Some("shape"));
        assert_eq!(
            source_fragment.as_ref().map(FragmentFact::id),
            Some("label")
        );

        assert_eq!(references.len(), 4);
        assert_eq!(references[0].element(), "style");
        assert_eq!(references[0].declared().as_str(), "masks.svg#soft");
        assert_eq!(references[0].kind(), HrefRole::CssUrl);
        assert_eq!(references[1].attribute(), "fill");
        assert_eq!(references[1].declared().as_str(), "#paint");
        assert_eq!(references[2].attribute(), "style");
        assert_eq!(references[2].declared().as_str(), "filters.svg#blur");
        assert_eq!(references[3].element(), "linearGradient");
        assert_eq!(references[3].kind(), HrefRole::Svg);
    }

    #[test]
    fn svg_fragments_preserve_id_and_xml_id_provenance() {
        let mut scan = scan(
            br#"<svg xmlns="http://www.w3.org/2000/svg"><g id="same" xml:id="same"/><g id="html" xml:id="xml"/></svg>"#,
        );
        let (facts, _, _) = scan.take_analysis();

        assert_eq!(
            facts
                .fragments()
                .iter()
                .map(|fact| (fact.id(), fact.attribute()))
                .collect::<Vec<_>>(),
            [
                ("same", FragmentAttribute::Id),
                ("same", FragmentAttribute::XmlId),
                ("html", FragmentAttribute::Id),
                ("xml", FragmentAttribute::XmlId),
            ]
        );
    }

    #[test]
    fn rejects_svg_names_without_the_svg_namespace() {
        assert!(!scan(br#"<svg/>"#).is_svg());
        assert!(!scan(br#"<svg xmlns="urn:not-svg"/>"#).is_svg());
        assert!(scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).is_svg());
    }

    #[test]
    fn rejects_trailing_document_content() {
        let multiple_roots = scan(
            br#"<svg xmlns="http://www.w3.org/2000/svg"/><svg xmlns="http://www.w3.org/2000/svg"/>"#,
        );
        assert!(multiple_roots.is_malformed());

        let trailing = scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/>trailing"#);
        assert!(trailing.is_malformed());

        let trailing_cdata = scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/><![CDATA[x]]>"#);
        assert!(trailing_cdata.is_malformed());

        let trailing_reference = scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/>&amp;"#);
        assert!(trailing_reference.is_malformed());

        let trailing_declaration =
            scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/><?xml version="1.0"?>"#);
        assert!(trailing_declaration.is_malformed());

        let trailing_doctype = scan(br#"<svg xmlns="http://www.w3.org/2000/svg"/><!DOCTYPE svg>"#);
        assert!(trailing_doctype.is_malformed());

        let declaration_after_comment = scan(
            br#"<!-- comment --><?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#,
        );
        assert!(declaration_after_comment.is_malformed());

        let declaration_after_doctype = scan(
            br#"<!DOCTYPE svg><?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#,
        );
        assert!(declaration_after_doctype.is_malformed());

        let unknown_reference =
            scan(br#"<svg xmlns="http://www.w3.org/2000/svg"><text>&unknown;</text></svg>"#);
        assert!(unknown_reference.is_malformed());
    }
}
