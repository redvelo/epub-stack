use super::common::{
    NavParseState, NavigationParseError, ParseResult, attr_value, checked_attr_value,
    finish_navigation_document, is_element, is_end, skip_element, validate_root,
};
use crate::{
    navigation::{
        Heading, MAX_NAV_DEPTH, NavigationDocument, NavigationList, NavigationPoint,
        NavigationSemanticSource, NavigationSemanticToken, NavigationSource,
    },
    resource::{AuthoredHref, EpubPath},
    semantics::{DpubAriaRole, EpubStructuralSemantic},
    string::EpubString,
    xml::{XmlUtf8Reader, cdata_content, normalize_optional, text_content},
};
use quick_xml::{events::Event, reader::NsReader};
use std::io::{BufRead, BufReader};

const NCX: &str = "ncx";
const NCX_NS: &str = "http://www.daisy.org/z3986/2005/ncx/";
const NAV_MAP: &str = "navMap";
const NAV_POINT: &str = "navPoint";
const NAV_LABEL: &str = "navLabel";
const CONTENT: &str = "content";
const SRC: &str = "src";
const HIDDEN: &str = "hidden";
const CLASS: &str = "class";
const ROLE: &str = "role";
const PAGE_LIST: &str = "pageList";
const PAGE_TARGET: &str = "pageTarget";
const NAV_LIST: &str = "navList";
const NAV_TARGET: &str = "navTarget";

/// Parses an EPUB 2 NCX document at its publication path.
///
/// Pass the publication path of the NCX so callers can later interpret relative hrefs. Labels and
/// headings are trimmed when converted to [`EpubString`]. `navMap` and `pageList` receive
/// structural meaning; generic `navList` sections remain auxiliary. The parser requires an NCX
/// `ncx` root and rejects trailing content and point trees deeper than 128 levels.
///
/// # Errors
///
/// Returns [`NavigationParseError`] for malformed XML, an unexpected root or namespace,
/// trailing content, an incomplete document, or excessive navigation depth.
pub fn ncx(path: EpubPath, xml: &str) -> Result<NavigationDocument, NavigationParseError> {
    parse_ncx_impl(path, xml.as_bytes())
}

pub(crate) fn ncx_reader<R: BufRead>(
    path: EpubPath,
    input: R,
) -> Result<NavigationDocument, NavigationParseError> {
    parse_ncx_impl(path, input)
}

fn parse_ncx_impl<R: BufRead>(path: EpubPath, input: R) -> ParseResult<NavigationDocument> {
    let mut state = NavParseState::new();
    let mut reader = NsReader::from_reader(BufReader::new(XmlUtf8Reader::new(input)));
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut lists = Vec::new();
    let mut root_seen = false;
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if !root_seen {
                    validate_root(&reader, &event, NCX, NCX_NS)?;
                    root_seen = true;
                    buf.clear();
                    continue;
                }
                if is_element(&reader, &event, NCX_NS.as_bytes(), NAV_MAP.as_bytes()) {
                    lists.push(parse_ncx_section(
                        &mut reader,
                        &event,
                        NAV_MAP,
                        NAV_POINT,
                        Some(EpubStructuralSemantic::Toc),
                        &mut state,
                    )?);
                } else if is_element(&reader, &event, NCX_NS.as_bytes(), PAGE_LIST.as_bytes()) {
                    lists.push(parse_ncx_section(
                        &mut reader,
                        &event,
                        PAGE_LIST,
                        PAGE_TARGET,
                        Some(EpubStructuralSemantic::PageList),
                        &mut state,
                    )?);
                } else if is_element(&reader, &event, NCX_NS.as_bytes(), NAV_LIST.as_bytes()) {
                    lists.push(parse_ncx_section(
                        &mut reader,
                        &event,
                        NAV_LIST,
                        NAV_TARGET,
                        None,
                        &mut state,
                    )?);
                }
            }
            Event::Empty(event) => {
                if !root_seen {
                    validate_root(&reader, &event, NCX, NCX_NS)?;
                    root_seen = true;
                    finish_navigation_document(&mut reader, &mut buf)?;
                    break;
                }
                if is_element(&reader, &event, NCX_NS.as_bytes(), NAV_MAP.as_bytes()) {
                    lists.push(empty_ncx_section(
                        &reader,
                        &event,
                        Some(EpubStructuralSemantic::Toc),
                        &mut state,
                    ));
                } else if is_element(&reader, &event, NCX_NS.as_bytes(), PAGE_LIST.as_bytes()) {
                    lists.push(empty_ncx_section(
                        &reader,
                        &event,
                        Some(EpubStructuralSemantic::PageList),
                        &mut state,
                    ));
                } else if is_element(&reader, &event, NCX_NS.as_bytes(), NAV_LIST.as_bytes()) {
                    lists.push(empty_ncx_section(&reader, &event, None, &mut state));
                }
            }
            Event::End(event) if is_end(&reader, &event, NCX_NS.as_bytes(), NCX.as_bytes()) => {
                finish_navigation_document(&mut reader, &mut buf)?;
                break;
            }
            Event::Eof if root_seen => {
                return Err(NavigationParseError::UnexpectedEof { expected: NCX });
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
        NavigationSource::Ncx,
        path,
        lists,
    ))
}

fn parse_ncx_semantics<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    state: &mut NavParseState,
) -> (Vec<NavigationSemanticToken>, bool) {
    state.attrs(event);
    let class = attr_value(reader, event, None, CLASS.as_bytes());
    let role = attr_value(reader, event, None, ROLE.as_bytes());
    let mut hidden = false;
    let tokens = class
        .as_deref()
        .into_iter()
        .flat_map(|value| {
            value
                .split_whitespace()
                .map(|token| (NavigationSemanticSource::Class, token))
        })
        .chain(role.as_deref().into_iter().flat_map(|value| {
            value
                .split_whitespace()
                .map(|token| (NavigationSemanticSource::Role, token))
        }))
        .map(|(source, token)| {
            if token.eq_ignore_ascii_case(HIDDEN) {
                hidden = true;
            }
            match source {
                NavigationSemanticSource::Class => NavigationSemanticToken::ncx_class(token),
                NavigationSemanticSource::Role => {
                    NavigationSemanticToken::role(token, DpubAriaRole::from_html_token(token))
                }
                NavigationSemanticSource::EpubType => unreachable!(),
            }
        })
        .collect();
    (tokens, hidden)
}

fn parse_ncx_section<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    section_end: &'static str,
    entry_name: &'static str,
    semantic: Option<EpubStructuralSemantic>,
    state: &mut NavParseState,
) -> ParseResult<NavigationList> {
    let (tokens, hidden) = parse_ncx_semantics(reader, event, state);
    let mut points = Vec::new();
    let mut heading = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_element(reader, &event, NCX_NS.as_bytes(), entry_name.as_bytes()) {
                    points.push(parse_ncx_point(reader, &event, entry_name, 1, state)?);
                } else if is_element(reader, &event, NCX_NS.as_bytes(), NAV_LABEL.as_bytes()) {
                    heading = parse_ncx_label(reader, state)?
                        .and_then(EpubString::new)
                        .map(Heading::h2);
                }
            }
            Event::Empty(event)
                if is_element(reader, &event, NCX_NS.as_bytes(), entry_name.as_bytes()) =>
            {
                points.push(empty_ncx_point(reader, &event, 1, state)?);
            }
            Event::End(end) if is_end(reader, &end, NCX_NS.as_bytes(), section_end.as_bytes()) => {
                break;
            }
            Event::Eof => {
                return Err(NavigationParseError::UnexpectedEof {
                    expected: section_end,
                });
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(NavigationList::from_authored(
        semantic, heading, hidden, points, tokens,
    ))
}

fn empty_ncx_section<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    semantic: Option<EpubStructuralSemantic>,
    state: &mut NavParseState,
) -> NavigationList {
    let (tokens, hidden) = parse_ncx_semantics(reader, event, state);
    NavigationList::from_authored(semantic, None, hidden, Vec::new(), tokens)
}

fn parse_ncx_point<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
    entry_name: &'static str,
    depth: usize,
    state: &mut NavParseState,
) -> ParseResult<NavigationPoint> {
    let (tokens, hidden) = parse_ncx_semantics(reader, event, state);
    if depth > MAX_NAV_DEPTH {
        return Err(NavigationParseError::DepthLimitExceeded {
            limit: MAX_NAV_DEPTH,
        });
    }
    let mut label = None;
    let mut href = None;
    let mut children = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_element(reader, &event, NCX_NS.as_bytes(), NAV_LABEL.as_bytes()) {
                    label = parse_ncx_label(reader, state)?;
                } else if is_element(reader, &event, NCX_NS.as_bytes(), CONTENT.as_bytes()) {
                    href = checked_attr_value(reader, &event, state, None, SRC.as_bytes());
                    skip_element(reader, event.name().as_ref(), CONTENT)?;
                } else if is_element(reader, &event, NCX_NS.as_bytes(), entry_name.as_bytes()) {
                    children.push(parse_ncx_point(
                        reader,
                        &event,
                        entry_name,
                        depth + 1,
                        state,
                    )?);
                }
            }
            Event::Empty(event) => {
                if is_element(reader, &event, NCX_NS.as_bytes(), CONTENT.as_bytes()) {
                    href = checked_attr_value(reader, &event, state, None, SRC.as_bytes());
                } else if is_element(reader, &event, NCX_NS.as_bytes(), entry_name.as_bytes()) {
                    children.push(empty_ncx_point(reader, &event, depth + 1, state)?);
                }
            }
            Event::End(end) if is_end(reader, &end, NCX_NS.as_bytes(), entry_name.as_bytes()) => {
                break;
            }
            Event::Eof => {
                return Err(NavigationParseError::UnexpectedEof {
                    expected: entry_name,
                });
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(navigation_point(label, href, children, hidden, tokens))
}

fn parse_ncx_label<R: BufRead>(
    reader: &mut NsReader<R>,
    state: &mut NavParseState,
) -> ParseResult<Option<String>> {
    let mut text_buffer = String::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Text(text) => text_buffer.push_str(&text_content(&text)?),
            Event::CData(text) => text_buffer.push_str(&cdata_content(&text)?),
            Event::GeneralRef(reference) => state.push_general_ref(&mut text_buffer, &reference)?,
            Event::End(end) if is_end(reader, &end, NCX_NS.as_bytes(), NAV_LABEL.as_bytes()) => {
                break;
            }
            Event::Eof => {
                return Err(NavigationParseError::UnexpectedEof {
                    expected: NAV_LABEL,
                });
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(normalize_optional(Some(text_buffer)))
}

fn empty_ncx_point<R: BufRead>(
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
    let (tokens, hidden) = parse_ncx_semantics(reader, event, state);
    Ok(navigation_point(None, None, Vec::new(), hidden, tokens))
}

fn navigation_point(
    label: Option<String>,
    href: Option<String>,
    children: Vec<NavigationPoint>,
    hidden: bool,
    tokens: Vec<NavigationSemanticToken>,
) -> NavigationPoint {
    NavigationPoint::from_authored(
        label.and_then(EpubString::new),
        href.map(AuthoredHref::new),
        children,
        hidden,
        None,
        tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/ncx_with_page_list_and_landmarks.ncx");

    fn parse(xml: &str) -> Result<NavigationDocument, NavigationParseError> {
        ncx(EpubPath::new("EPUB/toc.ncx").unwrap(), xml)
    }

    #[test]
    fn parses_toc_page_list_and_auxiliary_list() {
        let document = parse(FIXTURE).unwrap();
        assert_eq!(document.source(), NavigationSource::Ncx);
        assert!(document.toc().is_some());
        assert!(document.page_list().is_some());
        assert_eq!(document.auxiliary_lists().count(), 1);
        assert_eq!(document.lists()[2].heading().unwrap().text(), "Landmarks");
        assert_eq!(
            document.lists()[2].points()[0].authored_semantic_tokens()[0].dpub_role(),
            Some(DpubAriaRole::Cover)
        );
    }

    #[test]
    fn generic_nav_lists_remain_auxiliary_and_preserve_tokens() {
        let document = parse(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap/><navList class="vendor toc" role="doc-pagelist mystery"/></ncx>"#).unwrap();
        let list = document.auxiliary_lists().next().unwrap();
        assert_eq!(list.semantic(), None);
        assert_eq!(
            list.authored_semantic_tokens()
                .iter()
                .map(|token| token.raw())
                .collect::<Vec<_>>(),
            vec!["vendor", "toc", "doc-pagelist", "mystery"]
        );
    }

    #[test]
    fn self_closing_sections_match_started_empty_sections() {
        let closed = parse(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap class="hidden"/><pageList/><navList/></ncx>"#).unwrap();
        let started = parse(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap class="hidden"></navMap><pageList></pageList><navList></navList></ncx>"#).unwrap();
        assert_eq!(closed, started);
        assert!(closed.toc().unwrap().hidden());
    }

    #[test]
    fn reader_streams_utf16() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap/></ncx>"#;
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        assert!(
            ncx_reader(
                EpubPath::new("EPUB/toc.ncx").unwrap(),
                std::io::Cursor::new(bytes)
            )
            .is_ok()
        );
    }

    #[test]
    fn roots_namespaces_truncation_and_trailing_content_are_rejected() {
        assert!(matches!(parse(""), Err(NavigationParseError::MissingRoot)));
        assert!(matches!(
            parse("<ncx/>"),
            Err(NavigationParseError::WrongNamespace { .. })
        ));
        assert!(parse(r#"<n:ncx xmlns:n="http://www.daisy.org/z3986/2005/ncx/"/>"#).is_ok());
        assert!(matches!(
            parse(
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel>"#
            ),
            Err(NavigationParseError::UnexpectedEof {
                expected: NAV_LABEL
            })
        ));
        assert!(matches!(
            parse(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"/><extra/>"#),
            Err(NavigationParseError::TrailingContent)
        ));
    }

    fn nested_ncx(depth: usize) -> String {
        let mut xml = String::from("<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\"><navMap>");
        for level in 0..depth {
            xml.push_str(&format!(
                "<navPoint><navLabel><text>Level</text></navLabel><content src=\"{level}.xhtml\"/>"
            ));
        }
        for _ in 0..depth {
            xml.push_str("</navPoint>");
        }
        xml.push_str("</navMap></ncx>");
        xml
    }

    #[test]
    fn depth_128_succeeds_and_129_is_focused() {
        assert!(parse(&nested_ncx(128)).is_ok());
        assert!(matches!(
            parse(&nested_ncx(129)),
            Err(NavigationParseError::DepthLimitExceeded { limit: 128 })
        ));
    }

    #[test]
    fn foreign_children_and_attributes_do_not_masquerade_as_syntax() {
        let document = parse(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" xmlns:f="urn:foreign"><f:navMap/><navMap><f:navPoint/><navPoint f:class="cover"><f:navLabel>Wrong</f:navLabel><navLabel><text>Right</text></navLabel><content f:src="wrong.xhtml" src="right.xhtml"/></navPoint></navMap></ncx>"#).unwrap();
        let point = &document.toc().unwrap().points()[0];
        assert_eq!(point.label().map(EpubString::as_str), Some("Right"));
        assert_eq!(point.href().unwrap().as_str(), "right.xhtml");
        assert!(point.authored_semantic_tokens().is_empty());
    }
}
