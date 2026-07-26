use super::common::{
    NavParseState, NavigationParseError, ParseResult, attr_value, checked_attr_value,
    finish_navigation_document, is_element, is_end, skip_element, validate_root,
};
use crate::{
    navigation::{
        Heading, MAX_NAV_DEPTH, NavigationDocument, NavigationList, NavigationPoint,
        NavigationSemanticToken, NavigationSource, first_semantics,
    },
    resource::{AuthoredHref, EpubPath},
    semantics::{DpubAriaRole, EpubStructuralSemantic, HeadingLevel},
    string::EpubString,
    xml::{cdata_content, normalize_optional, text_content},
};
use quick_xml::{events::Event, reader::NsReader};
use std::io::BufRead;

const HTML: &str = "html";
const NAV: &str = "nav";
const LI: &str = "li";
const A: &str = "a";
const SPAN: &str = "span";
const HREF: &str = "href";
const HIDDEN: &str = "hidden";
const ROLE: &str = "role";
const EPUB_TYPE: &str = "type";
const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const OPS_NS: &str = "http://www.idpf.org/2007/ops";

/// Parses an EPUB navigation XHTML document at its publication path.
///
/// Pass the publication path of the navigation resource so callers can later interpret relative
/// hrefs. Labels and headings are trimmed when converted to [`EpubString`]. The parser requires
/// an XHTML `html` root, rejects trailing content and point trees deeper than 128 levels, and
/// omits tolerated optional markup from the returned model.
///
/// # Errors
///
/// Returns [`NavigationParseError`] for malformed XML, an unexpected root or namespace,
/// trailing content, an incomplete document, or excessive navigation depth.
pub fn epub_nav(path: EpubPath, xml: &str) -> Result<NavigationDocument, NavigationParseError> {
    parse_epub_nav_impl(path, xml.as_bytes())
}

fn parse_epub_nav_impl<R: BufRead>(path: EpubPath, input: R) -> ParseResult<NavigationDocument> {
    let mut state = NavParseState::new();
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut lists = Vec::new();
    let mut root_seen = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if !root_seen {
                    validate_root(&reader, &event, HTML, XHTML_NS)?;
                    root_seen = true;
                    buf.clear();
                    continue;
                }
                if is_element(&reader, &event, XHTML_NS.as_bytes(), NAV.as_bytes()) {
                    lists.push(parse_epub_nav_list(&mut reader, &event, &mut state)?);
                }
            }
            Event::Empty(event) => {
                if !root_seen {
                    validate_root(&reader, &event, HTML, XHTML_NS)?;
                    root_seen = true;
                    finish_navigation_document(&mut reader, &mut buf)?;
                    break;
                }
                if is_element(&reader, &event, XHTML_NS.as_bytes(), NAV.as_bytes()) {
                    state.attrs(&event);
                    let epub_type = attr_value(
                        &reader,
                        &event,
                        Some(OPS_NS.as_bytes()),
                        EPUB_TYPE.as_bytes(),
                    );
                    let role = attr_value(&reader, &event, None, ROLE.as_bytes());
                    let hidden_attr = attr_value(&reader, &event, None, HIDDEN.as_bytes());
                    let (tokens, hidden) = parse_epub_nav_semantics(epub_type, role, hidden_attr);
                    lists.push(NavigationList::from_semantics(
                        tokens,
                        None,
                        hidden,
                        Vec::new(),
                    ));
                }
            }
            Event::End(event) if is_end(&reader, &event, XHTML_NS.as_bytes(), HTML.as_bytes()) => {
                finish_navigation_document(&mut reader, &mut buf)?;
                break;
            }
            Event::Eof if root_seen => {
                return Err(NavigationParseError::UnexpectedEof { expected: HTML });
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if !root_seen {
        return Err(NavigationParseError::MissingRoot);
    }
    Ok(NavigationDocument::from_parsed(
        NavigationSource::EpubNav,
        path,
        lists,
    ))
}

fn parse_epub_type_and_hidden(
    epub_type: Option<String>,
    hidden_attr: Option<String>,
) -> (Vec<NavigationSemanticToken>, bool) {
    let mut hidden = hidden_attr.is_some();
    let tokens = epub_type
        .as_deref()
        .into_iter()
        .flat_map(|val| val.split_whitespace())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| {
            if token.eq_ignore_ascii_case(HIDDEN) {
                hidden = true;
            }
            NavigationSemanticToken::epub_type(token, EpubStructuralSemantic::from_token(token))
        })
        .collect();
    (tokens, hidden)
}

fn parse_epub_nav_semantics(
    epub_type: Option<String>,
    role: Option<String>,
    hidden_attr: Option<String>,
) -> (Vec<NavigationSemanticToken>, bool) {
    let (mut tokens, hidden) = parse_epub_type_and_hidden(epub_type, hidden_attr);
    if let Some(role) = role {
        tokens.extend(
            role.split_whitespace()
                .map(|raw| NavigationSemanticToken::role(raw, DpubAriaRole::from_html_token(raw))),
        );
    }
    (tokens, hidden)
}

fn parse_element_semantics<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    state: &mut NavParseState,
) -> (Vec<NavigationSemanticToken>, bool) {
    state.attrs(event);
    parse_epub_nav_semantics(
        attr_value(reader, event, Some(OPS_NS.as_bytes()), EPUB_TYPE.as_bytes()),
        attr_value(reader, event, None, ROLE.as_bytes()),
        attr_value(reader, event, None, HIDDEN.as_bytes()),
    )
}

fn parse_epub_nav_list<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    state: &mut NavParseState,
) -> ParseResult<NavigationList> {
    let (semantic_tokens, hidden) = parse_element_semantics(reader, event, state);
    let mut heading = None;
    let mut points = Vec::new();
    let mut text_target = None;
    let mut text_buffer = String::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                let name = event.local_name().as_ref().to_vec();
                if is_element(reader, &event, XHTML_NS.as_bytes(), LI.as_bytes()) {
                    points.push(parse_epub_nav_point(reader, &event, 1, state)?);
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), NAV.as_bytes()) {
                    skip_element(reader, event.name().as_ref(), NAV)?;
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), name.as_slice())
                    && let Some(level) = heading_level_from_name(name.as_slice())
                    && heading.is_none()
                    && text_target.is_none()
                {
                    text_target = Some(TextTarget::Heading(level));
                    text_buffer.clear();
                }
            }
            Event::Empty(event) => {
                if is_element(reader, &event, XHTML_NS.as_bytes(), LI.as_bytes()) {
                    points.push(empty_epub_nav_point(reader, &event, 1, state)?);
                }
            }
            Event::Text(text) if text_target.is_some() => {
                text_buffer.push_str(&text_content(&text)?);
            }
            Event::CData(text) if text_target.is_some() => {
                text_buffer.push_str(&cdata_content(&text)?);
            }
            Event::GeneralRef(reference) if text_target.is_some() => {
                state.push_general_ref(&mut text_buffer, &reference)?;
            }
            Event::End(end) => {
                let name = end.local_name().as_ref().to_vec();
                if is_end(reader, &end, XHTML_NS.as_bytes(), NAV.as_bytes()) {
                    break;
                } else if is_end(reader, &end, XHTML_NS.as_bytes(), name.as_slice())
                    && let Some(level) = heading_level_from_name(name.as_slice())
                    && matches!(text_target, Some(TextTarget::Heading(current)) if current == level)
                {
                    heading = normalize_optional(Some(text_buffer.clone()))
                        .and_then(|text| build_heading(level, text));
                    text_target = None;
                    text_buffer.clear();
                }
            }
            Event::Eof => return Err(NavigationParseError::UnexpectedEof { expected: NAV }),
            _ => {}
        }
        buf.clear();
    }
    Ok(NavigationList::from_semantics(
        semantic_tokens,
        heading,
        hidden,
        points,
    ))
}

fn parse_epub_nav_point<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    depth: usize,
    state: &mut NavParseState,
) -> ParseResult<NavigationPoint> {
    state.attrs(event);
    let mut hidden = attr_value(reader, event, None, HIDDEN.as_bytes()).is_some();
    if depth > MAX_NAV_DEPTH {
        return Err(NavigationParseError::DepthLimitExceeded {
            limit: MAX_NAV_DEPTH,
        });
    }
    let mut semantic_tokens = Vec::new();
    let mut label = None;
    let mut href = None;
    let mut children = Vec::new();
    let mut text_target = None;
    let mut text_buffer = String::new();
    let mut label_semantics_claimed = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_element(reader, &event, XHTML_NS.as_bytes(), A.as_bytes()) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) =
                            parse_element_semantics(reader, &event, state);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    if href.is_none() {
                        href = checked_attr_value(reader, &event, state, None, HREF.as_bytes());
                    }
                    if label.is_none() && text_target.is_none() {
                        text_target = Some(TextTarget::NavAnchorLabel);
                        text_buffer.clear();
                    }
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), SPAN.as_bytes()) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) =
                            parse_element_semantics(reader, &event, state);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    if label.is_none() && text_target.is_none() {
                        text_target = Some(TextTarget::NavSpanLabel);
                        text_buffer.clear();
                    }
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), LI.as_bytes()) {
                    children.push(parse_epub_nav_point(reader, &event, depth + 1, state)?);
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), NAV.as_bytes()) {
                    skip_element(reader, event.name().as_ref(), NAV)?;
                }
            }
            Event::Empty(event) => {
                if is_element(reader, &event, XHTML_NS.as_bytes(), A.as_bytes()) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) =
                            parse_element_semantics(reader, &event, state);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    if href.is_none() {
                        href = checked_attr_value(reader, &event, state, None, HREF.as_bytes());
                    }
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), SPAN.as_bytes()) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) =
                            parse_element_semantics(reader, &event, state);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                } else if is_element(reader, &event, XHTML_NS.as_bytes(), LI.as_bytes()) {
                    children.push(empty_epub_nav_point(reader, &event, depth + 1, state)?);
                }
            }
            Event::Text(text) if text_target.is_some() => {
                text_buffer.push_str(&text_content(&text)?);
            }
            Event::CData(text) if text_target.is_some() => {
                text_buffer.push_str(&cdata_content(&text)?);
            }
            Event::GeneralRef(reference) if text_target.is_some() => {
                state.push_general_ref(&mut text_buffer, &reference)?;
            }
            Event::End(end) => {
                if is_end(reader, &end, XHTML_NS.as_bytes(), LI.as_bytes()) {
                    break;
                } else if matches!(text_target, Some(TextTarget::NavAnchorLabel))
                    && is_end(reader, &end, XHTML_NS.as_bytes(), A.as_bytes())
                    || matches!(text_target, Some(TextTarget::NavSpanLabel))
                        && is_end(reader, &end, XHTML_NS.as_bytes(), SPAN.as_bytes())
                {
                    label = normalize_optional(Some(text_buffer.clone()));
                    text_target = None;
                    text_buffer.clear();
                }
            }
            Event::Eof => return Err(NavigationParseError::UnexpectedEof { expected: LI }),
            _ => {}
        }
        buf.clear();
    }
    Ok(navigation_point(
        label,
        href,
        children,
        hidden,
        semantic_tokens,
    ))
}

fn empty_epub_nav_point<R: BufRead>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    depth: usize,
    state: &mut NavParseState,
) -> ParseResult<NavigationPoint> {
    if depth > MAX_NAV_DEPTH {
        return Err(NavigationParseError::DepthLimitExceeded {
            limit: MAX_NAV_DEPTH,
        });
    }
    state.attrs(event);
    Ok(navigation_point(
        None,
        None,
        Vec::new(),
        attr_value(reader, event, None, HIDDEN.as_bytes()).is_some(),
        Vec::new(),
    ))
}

fn navigation_point(
    label: Option<String>,
    href: Option<String>,
    children: Vec<NavigationPoint>,
    hidden: bool,
    semantic_tokens: Vec<NavigationSemanticToken>,
) -> NavigationPoint {
    let semantic = first_semantics(&semantic_tokens);
    NavigationPoint::from_authored(
        label.and_then(EpubString::new),
        href.map(AuthoredHref::new),
        children,
        hidden,
        semantic,
        semantic_tokens,
    )
}

fn heading_level_from_name(name: &[u8]) -> Option<HeadingLevel> {
    match name {
        b"h1" => HeadingLevel::new(1),
        b"h2" => HeadingLevel::new(2),
        b"h3" => HeadingLevel::new(3),
        b"h4" => HeadingLevel::new(4),
        b"h5" => HeadingLevel::new(5),
        b"h6" => HeadingLevel::new(6),
        _ => None,
    }
}

fn build_heading(level: HeadingLevel, text: String) -> Option<Heading> {
    Some(Heading::new(level, EpubString::new(text)?))
}

#[derive(Debug, Clone, Copy)]
enum TextTarget {
    Heading(HeadingLevel),
    NavAnchorLabel,
    NavSpanLabel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{NavigationSemanticSource, parse::epub_nav};

    fn parse(xml: &str) -> Result<NavigationDocument, NavigationParseError> {
        epub_nav(EpubPath::new("EPUB/nav.xhtml").unwrap(), xml)
    }

    #[test]
    fn parses_toc_and_nested_points() {
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><h1>Table of contents</h1><ol><li><a href="one.xhtml">One</a><ol><li><a href="two.xhtml">Two</a></li></ol></li></ol></nav><nav epub:type="page-list" hidden="hidden"/></body></html>"#;
        let document = parse(xml).unwrap();
        assert_eq!(document.source(), NavigationSource::EpubNav);
        assert_eq!(
            document.toc().unwrap().heading().unwrap().text(),
            "Table of contents"
        );
        assert_eq!(
            document.toc().unwrap().points()[0].children()[0].label(),
            Some(&EpubString::new("Two").unwrap())
        );
        assert!(document.page_list().unwrap().hidden());
    }

    #[test]
    fn preserves_entities_empty_hrefs_and_semantic_tokens() {
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="vendor toc" role="doc-toc"><ol><li><a href="" epub:type="unknown cover" role="doc-chapter">A &amp; &unknown; B</a></li></ol></nav></body></html>"#;
        let document = parse(xml).unwrap();
        let list = document.toc().unwrap();
        assert_eq!(
            list.authored_semantic_tokens()
                .iter()
                .map(|token| token.raw())
                .collect::<Vec<_>>(),
            vec!["vendor", "toc", "doc-toc"]
        );
        let point = &list.points()[0];
        assert_eq!(
            point.label().map(EpubString::as_str),
            Some("A & &unknown; B")
        );
        assert_eq!(point.href(), None);
        assert_eq!(point.authored_href().map(AuthoredHref::as_str), Some(""));
        assert_eq!(point.semantic(), Some(EpubStructuralSemantic::Cover));
        assert_eq!(
            point.authored_semantic_tokens()[2].source(),
            NavigationSemanticSource::Role
        );
    }

    #[test]
    fn malformed_attributes_are_omitted_while_navigation_is_preserved() {
        let document = parse(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav bad="one" bad="two" epub:type="toc"><ol/></nav></body></html>"#,
        )
        .unwrap();

        assert!(document.toc().is_some());
    }

    #[test]
    fn dpub_roles_do_not_select_epub_list_types() {
        let document = parse(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav role="doc-pagelist"><ol/></nav></body></html>"#,
        )
        .unwrap();

        assert!(document.page_list().is_none());
        assert_eq!(document.lists()[0].semantic(), None);
        assert_eq!(document.auxiliary_lists().count(), 1);
        assert_eq!(
            document.lists()[0].authored_semantic_tokens()[0].dpub_role(),
            Some(DpubAriaRole::PageList)
        );
    }

    #[test]
    fn semantics_are_scoped_to_the_label_element() {
        let document = parse(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="landmarks"><ol><li role="none"><a href="cover.xhtml" epub:type="cover">Cover</a></li><li epub:type="chapter" role="doc-chapter" hidden="hidden"><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
        )
        .unwrap();

        assert_eq!(
            document.landmarks().unwrap().points()[0].semantic(),
            Some(EpubStructuralSemantic::Cover)
        );
        let bare_label = &document.landmarks().unwrap().points()[1];
        assert_eq!(bare_label.semantic(), None);
        assert!(bare_label.authored_semantic_tokens().is_empty());
        assert!(bare_label.hidden());
    }

    #[test]
    fn known_non_list_terms_do_not_mask_list_types() {
        let document = parse(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="frontmatter toc"><ol/></nav><nav epub:type="chapter page-list"><ol/></nav><nav epub:type="toc page-list"><ol/></nav></body></html>"#,
        )
        .unwrap();

        assert_eq!(
            document
                .lists()
                .iter()
                .map(NavigationList::semantic)
                .collect::<Vec<_>>(),
            vec![
                Some(EpubStructuralSemantic::Toc),
                Some(EpubStructuralSemantic::PageList),
                Some(EpubStructuralSemantic::Toc),
            ]
        );
    }

    #[test]
    fn roots_namespaces_truncation_and_trailing_content_are_rejected() {
        assert!(matches!(parse(""), Err(NavigationParseError::MissingRoot)));
        assert!(matches!(
            parse("<body xmlns=\"http://www.w3.org/1999/xhtml\"/>"),
            Err(NavigationParseError::WrongRoot { .. })
        ));
        assert!(matches!(
            parse("<html/>"),
            Err(NavigationParseError::WrongNamespace { .. })
        ));
        assert!(matches!(
            parse(r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol><li>"#),
            Err(NavigationParseError::UnexpectedEof { expected: LI })
        ));
        assert!(matches!(
            parse(r#"<html xmlns="http://www.w3.org/1999/xhtml"/><extra/>"#),
            Err(NavigationParseError::TrailingContent)
        ));
    }

    fn nested_nav(depth: usize) -> String {
        let mut xml = String::from("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><nav><ol>");
        for level in 0..depth {
            xml.push_str(&format!("<li><a href=\"{level}.xhtml\">Level</a><ol>"));
        }
        for _ in 0..depth {
            xml.push_str("</ol></li>");
        }
        xml.push_str("</ol></nav></body></html>");
        xml
    }

    #[test]
    fn depth_128_succeeds_and_129_is_focused() {
        assert!(parse(&nested_nav(128)).is_ok());
        assert!(matches!(
            parse(&nested_nav(129)),
            Err(NavigationParseError::DepthLimitExceeded { limit: 128 })
        ));
    }

    #[test]
    fn nested_and_foreign_nav_elements_do_not_masquerade_as_syntax() {
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xmlns:f="urn:foreign"><body><f:nav epub:type="toc"/><nav f:type="page-list" epub:type="toc"><ol><li><a href="one.xhtml">One</a></li><li f:type="cover"><f:a href="wrong.xhtml">Wrong</f:a><a f:href="wrong.xhtml" href="two.xhtml">Two</a></li></ol><nav><ol><li><a href="ignored.xhtml">Ignored</a></li></ol></nav></nav></body></html>"#;
        let document = parse(xml).unwrap();
        assert_eq!(document.lists().len(), 1);
        assert_eq!(document.lists()[0].points().len(), 2);
        assert_eq!(
            document.lists()[0].semantic(),
            Some(EpubStructuralSemantic::Toc)
        );
        assert_eq!(
            document.lists()[0].points()[1].href().unwrap().as_str(),
            "two.xhtml"
        );
        assert!(
            document.lists()[0].points()[1]
                .authored_semantic_tokens()
                .is_empty()
        );
    }

    #[test]
    fn repeated_labels_and_hrefs_keep_first_values() {
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol><li><a href="first.xhtml">First <![CDATA[&]]> <span>nested</span> tail</a><a href="second.xhtml">Second</a></li></ol></nav></body></html>"#;
        let document = parse(xml).unwrap();
        let point = &document.lists()[0].points()[0];
        assert_eq!(
            point.label().map(EpubString::as_str),
            Some("First & nested tail")
        );
        assert_eq!(point.href().unwrap().as_str(), "first.xhtml");
    }
}
