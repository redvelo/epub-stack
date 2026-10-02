use super::common::{
    NavigationParseError, ParseResult, attr_value, checked_attr_value, finish_navigation_document,
    is_element, is_end, push_general_ref_text, skip_element, validate_root,
};
use crate::{
    navigation::{
        Heading, MAX_NAV_DEPTH, NavigationDocument, NavigationList, NavigationPoint,
        NavigationSource, first_semantics,
    },
    resource::{AuthoredHref, EpubPath},
    semantics::{HeadingLevel, SemanticToken},
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
/// an XHTML `html` root and rejects trailing content, point trees deeper than 128 levels, and
/// label markup deeper than 128 elements (including its heading, anchor, or span owner).
/// Non-text label content uses an unqualified `alt` attribute when present, otherwise `title`,
/// in place of descendant text. Ordinary inline text is retained in document order.
///
/// # Errors
///
/// Returns [`NavigationParseError`] for malformed XML, an unexpected root or namespace,
/// trailing content, an incomplete document, or excessive navigation depth.
pub fn epub_nav(path: EpubPath, xml: &str) -> Result<NavigationDocument, NavigationParseError> {
    parse_epub_nav_impl(path, xml.as_bytes())
}

fn parse_epub_nav_impl<R: BufRead>(path: EpubPath, input: R) -> ParseResult<NavigationDocument> {
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut lists = Vec::new();
    let mut root_seen = false;
    let mut in_head = false;
    let mut authored_base = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if !root_seen {
                    validate_root(&reader, &event, HTML, XHTML_NS)?;
                    root_seen = true;
                    buf.clear();
                    continue;
                }
                if is_element(&reader, &event, XHTML_NS, "head") {
                    in_head = true;
                }
                if in_head
                    && authored_base.is_none()
                    && is_element(&reader, &event, XHTML_NS, "base")
                {
                    authored_base =
                        attr_value(&reader, &event, None, "href").map(AuthoredHref::new);
                }
                if is_element(&reader, &event, XHTML_NS, NAV) {
                    lists.push(parse_epub_nav_list(&mut reader, &event)?);
                }
            }
            Event::Empty(event) => {
                if !root_seen {
                    validate_root(&reader, &event, HTML, XHTML_NS)?;
                    root_seen = true;
                    finish_navigation_document(&mut reader, &mut buf)?;
                    break;
                }
                if in_head
                    && authored_base.is_none()
                    && is_element(&reader, &event, XHTML_NS, "base")
                {
                    authored_base =
                        attr_value(&reader, &event, None, "href").map(AuthoredHref::new);
                }
                if is_element(&reader, &event, XHTML_NS, NAV) {
                    let epub_type = attr_value(&reader, &event, Some(OPS_NS), EPUB_TYPE);
                    let role = attr_value(&reader, &event, None, ROLE);
                    let hidden_attr = attr_value(&reader, &event, None, HIDDEN);
                    let (tokens, hidden) = parse_epub_nav_semantics(epub_type, role, hidden_attr);
                    lists.push(NavigationList::from_semantics(
                        tokens,
                        None,
                        hidden,
                        Vec::new(),
                    ));
                }
            }
            Event::End(event) if is_end(&reader, &event, XHTML_NS, "head") => {
                in_head = false;
            }
            Event::End(event) if is_end(&reader, &event, XHTML_NS, HTML) => {
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
    let mut document = NavigationDocument::from_parsed(NavigationSource::EpubNav, path, lists);
    document.authored_base = authored_base;
    Ok(document)
}

fn parse_epub_type_and_hidden(
    epub_type: Option<String>,
    hidden_attr: Option<String>,
) -> (Vec<SemanticToken>, bool) {
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
            SemanticToken::epub_type(token)
        })
        .filter_map(Result::ok)
        .collect();
    (tokens, hidden)
}

fn parse_epub_nav_semantics(
    epub_type: Option<String>,
    role: Option<String>,
    hidden_attr: Option<String>,
) -> (Vec<SemanticToken>, bool) {
    let (mut tokens, hidden) = parse_epub_type_and_hidden(epub_type, hidden_attr);
    if let Some(role) = role {
        tokens.extend(
            role.split_whitespace()
                .filter_map(|raw| SemanticToken::aria_role(raw).ok()),
        );
    }
    (tokens, hidden)
}

fn parse_element_semantics<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
) -> (Vec<SemanticToken>, bool) {
    parse_epub_nav_semantics(
        attr_value(reader, event, Some(OPS_NS), EPUB_TYPE),
        attr_value(reader, event, None, ROLE),
        attr_value(reader, event, None, HIDDEN),
    )
}

fn parse_epub_nav_list<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &quick_xml::events::BytesStart<'_>,
) -> ParseResult<NavigationList> {
    let (semantic_tokens, hidden) = parse_element_semantics(reader, event);
    let mut heading = None;
    let mut points = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                let name = event.local_name().as_ref().to_string();
                if is_element(reader, &event, XHTML_NS, LI) {
                    points.push(parse_epub_nav_point(reader, &event, 1)?);
                } else if is_element(reader, &event, XHTML_NS, NAV) {
                    skip_element(reader, event.name().as_ref(), NAV)?;
                } else if is_element(reader, &event, XHTML_NS, &name)
                    && let Some(level) = heading_level_from_name(&name)
                    && heading.is_none()
                {
                    heading = parse_label_text(reader, &event)?
                        .and_then(|text| build_heading(level, text));
                }
            }
            Event::Empty(event) => {
                if is_element(reader, &event, XHTML_NS, LI) {
                    points.push(empty_epub_nav_point(reader, &event, 1)?);
                }
            }
            Event::End(end) => {
                if is_end(reader, &end, XHTML_NS, NAV) {
                    break;
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
) -> ParseResult<NavigationPoint> {
    let mut hidden = attr_value(reader, event, None, HIDDEN).is_some();
    if depth > MAX_NAV_DEPTH {
        return Err(NavigationParseError::DepthLimitExceeded {
            limit: MAX_NAV_DEPTH,
        });
    }
    let mut semantic_tokens = Vec::new();
    let mut label = None;
    let mut href = None;
    let mut children = Vec::new();
    let mut label_semantics_claimed = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_element(reader, &event, XHTML_NS, A) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) = parse_element_semantics(reader, &event);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    if href.is_none() {
                        href = checked_attr_value(reader, &event, None, HREF);
                    }
                    let text = parse_label_text(reader, &event)?;
                    if label.is_none() {
                        label = text;
                    }
                } else if is_element(reader, &event, XHTML_NS, SPAN) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) = parse_element_semantics(reader, &event);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    let text = parse_label_text(reader, &event)?;
                    if label.is_none() {
                        label = text;
                    }
                } else if is_element(reader, &event, XHTML_NS, LI) {
                    children.push(parse_epub_nav_point(reader, &event, depth + 1)?);
                } else if is_element(reader, &event, XHTML_NS, NAV) {
                    skip_element(reader, event.name().as_ref(), NAV)?;
                }
            }
            Event::Empty(event) => {
                if is_element(reader, &event, XHTML_NS, A) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) = parse_element_semantics(reader, &event);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                    if href.is_none() {
                        href = checked_attr_value(reader, &event, None, HREF);
                    }
                } else if is_element(reader, &event, XHTML_NS, SPAN) {
                    if !label_semantics_claimed {
                        let (tokens, element_hidden) = parse_element_semantics(reader, &event);
                        semantic_tokens = tokens;
                        hidden |= element_hidden;
                        label_semantics_claimed = true;
                    }
                } else if is_element(reader, &event, XHTML_NS, LI) {
                    children.push(empty_epub_nav_point(reader, &event, depth + 1)?);
                }
            }
            Event::End(end) => {
                if is_end(reader, &end, XHTML_NS, LI) {
                    break;
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
) -> ParseResult<NavigationPoint> {
    if depth > MAX_NAV_DEPTH {
        return Err(NavigationParseError::DepthLimitExceeded {
            limit: MAX_NAV_DEPTH,
        });
    }
    Ok(navigation_point(
        None,
        None,
        Vec::new(),
        attr_value(reader, event, None, HIDDEN).is_some(),
        Vec::new(),
    ))
}

fn navigation_point(
    label: Option<String>,
    href: Option<String>,
    children: Vec<NavigationPoint>,
    hidden: bool,
    semantic_tokens: Vec<SemanticToken>,
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

fn heading_level_from_name(name: &str) -> Option<HeadingLevel> {
    match name {
        "h1" => HeadingLevel::new(1),
        "h2" => HeadingLevel::new(2),
        "h3" => HeadingLevel::new(3),
        "h4" => HeadingLevel::new(4),
        "h5" => HeadingLevel::new(5),
        "h6" => HeadingLevel::new(6),
        _ => None,
    }
}

fn build_heading(level: HeadingLevel, text: String) -> Option<Heading> {
    Some(Heading::new(level, EpubString::new(text)?))
}

fn parse_label_text<R: BufRead>(
    reader: &mut NsReader<R>,
    owner: &quick_xml::events::BytesStart<'_>,
) -> ParseResult<Option<String>> {
    let expected = match owner.local_name().as_ref() {
        "a" => A,
        "span" => SPAN,
        "h1" => "h1",
        "h2" => "h2",
        "h3" => "h3",
        "h4" => "h4",
        "h5" => "h5",
        _ => "h6",
    };
    let mut depth = 1;
    let mut substituted_at = None;
    let mut text = String::new();
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf)?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(event) | Event::Empty(event) => {
                if depth == MAX_NAV_DEPTH {
                    return Err(NavigationParseError::DepthLimitExceeded {
                        limit: MAX_NAV_DEPTH,
                    });
                }
                if substituted_at.is_none() {
                    let name = event.local_name();
                    let non_text = (is_element(reader, &event, XHTML_NS, name.as_ref())
                        && matches!(
                            name.as_ref(),
                            "img"
                                | "object"
                                | "embed"
                                | "iframe"
                                | "audio"
                                | "video"
                                | "canvas"
                                | "input"
                        ))
                        || is_element(reader, &event, "http://www.w3.org/2000/svg", "svg")
                        || is_element(reader, &event, "http://www.w3.org/1998/Math/MathML", "math");
                    if non_text {
                        // Attribute presence, including an empty alt, takes precedence over title.
                        let alt_present =
                            event.attributes().with_checks(false).flatten().any(|attr| {
                                let (namespace, local) =
                                    reader.resolver().resolve_attribute(attr.key);
                                matches!(namespace, quick_xml::name::ResolveResult::Unbound)
                                    && local.as_ref() == "alt"
                            });
                        let alternative = attr_value(
                            reader,
                            &event,
                            None,
                            if alt_present { "alt" } else { "title" },
                        );
                        if alt_present || alternative.is_some() {
                            if let Some(alternative) = alternative {
                                text.push_str(&alternative);
                            }
                            if !empty {
                                substituted_at = Some(depth + 1);
                            }
                        }
                    }
                }
                if !empty {
                    depth += 1;
                }
            }
            Event::End(_) => {
                if substituted_at == Some(depth) {
                    substituted_at = None;
                }
                depth -= 1;
                if depth == 0 {
                    return Ok(normalize_optional(Some(text)));
                }
            }
            Event::Text(event) if substituted_at.is_none() => {
                text.push_str(&text_content(&event));
            }
            Event::CData(event) if substituted_at.is_none() => {
                text.push_str(&cdata_content(&event));
            }
            Event::GeneralRef(event) if substituted_at.is_none() => {
                push_general_ref_text(&mut text, &event)?;
            }
            Event::Eof => return Err(NavigationParseError::UnexpectedEof { expected }),
            _ => {}
        }
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::parse::epub_nav;
    use crate::semantics::{DpubAriaRole, EpubStructuralSemantic, SemanticToken};

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
                .map(|token| token.as_str())
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
        assert!(matches!(
            point.authored_semantic_tokens()[2],
            SemanticToken::AriaRole { .. }
        ));
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

    #[test]
    fn heading_and_point_labels_share_ordered_image_alternatives() {
        let cases = [
            (r#"<img alt="Contents" title="Ignored"/>"#, Some("Contents")),
            (r#"<img title="Contents"/>"#, Some("Contents")),
            (r#"<img alt="" title="Ignored"/>"#, None),
            (r#"<img alt=" " title="Ignored"/>"#, None),
            (r#"<img/>"#, None),
            (
                r#"Before <img alt="A"/> / <img title="B"/> after"#,
                Some("Before A / B after"),
            ),
            (r#"<img alt="" title="Ignored"/>Tail"#, Some("Tail")),
            (
                r#"<img alt="IV &amp; &#x2163; &#233;"/> &amp; &unknown;"#,
                Some("IV & \u{2163} \u{e9} & &unknown;"),
            ),
            (
                r#"<span title="Not an alternative">Outer <span>inner</span> tail</span> end"#,
                Some("Outer inner tail end"),
            ),
            (
                r#"<object title="Once"><span>Not twice<img alt="Nor this"/></span></object> tail"#,
                Some("Once tail"),
            ),
            (
                r#"<object alt="" title="Ignored">Ignored descendant</object>"#,
                None,
            ),
            (
                r#"<object><span>Fallback</span> text</object>"#,
                Some("Fallback text"),
            ),
            (
                r#"<s:svg xmlns:s="http://www.w3.org/2000/svg" title="Diagram"><s:title>Duplicate</s:title></s:svg> tail"#,
                Some("Diagram tail"),
            ),
            (
                r#"<m:math xmlns:m="http://www.w3.org/1998/Math/MathML" alt="Formula"><m:mi>x</m:mi></m:math>"#,
                Some("Formula"),
            ),
            (
                r#"<x:img xmlns:x="http://www.w3.org/1999/xhtml" alt="Prefixed"/>"#,
                Some("Prefixed"),
            ),
            (
                r#"<f:img xmlns:f="urn:foreign" alt="Ignored">Foreign text</f:img>"#,
                Some("Foreign text"),
            ),
            (
                r#"<img xmlns:f="urn:foreign" f:alt="Ignored" title="Fallback"/>"#,
                Some("Fallback"),
            ),
            (r#"<img xmlns:f="urn:foreign" f:title="Ignored"/>"#, None),
            (
                r#"<img alt="Once">Not twice</img><![CDATA[ & tail]]>"#,
                Some("Once & tail"),
            ),
        ];
        for (markup, expected) in cases {
            let xml = format!(
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav><h2>{markup}</h2><ol><li><a href="chapter.xhtml">{markup}</a></li><li><span>{markup}</span></li></ol></nav></html>"#
            );
            let document = parse(&xml).unwrap();
            let list = &document.lists()[0];
            assert_eq!(
                list.heading().map(|heading| heading.text().as_str()),
                expected,
                "{markup}"
            );
            for point in list.points() {
                assert_eq!(point.label().map(EpubString::as_str), expected, "{markup}");
            }
        }
    }

    #[test]
    fn nested_span_owner_retains_tail_without_claiming_descendant_facts() {
        let document = parse(r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><nav hidden="" epub:type="toc"><h2><span>First</span> heading</h2><h2>Second heading</h2><ol><li><span epub:type="part" hidden="">Outer <span epub:type="chapter">inner <span>deep</span> tail</span> end</span><a href="  chapter.xhtml#one  " epub:type="cover">Later</a><a href="other.xhtml">Other</a><ol><li><a href="child.xhtml"><img alt="Child"/></a></li></ol></li></ol></nav></html>"#).unwrap();
        let list = &document.lists()[0];
        assert!(list.hidden());
        assert_eq!(list.heading().unwrap().text(), "First heading");
        let point = &list.points()[0];
        assert_eq!(
            point.label().map(EpubString::as_str),
            Some("Outer inner deep tail end")
        );
        assert_eq!(
            point.authored_href().map(AuthoredHref::as_str),
            Some("  chapter.xhtml#one  ")
        );
        assert!(point.hidden());
        assert_eq!(point.semantic(), Some(EpubStructuralSemantic::Part));
        assert_eq!(point.authored_semantic_tokens().len(), 1);
        assert_eq!(
            point.children()[0].label().map(EpubString::as_str),
            Some("Child")
        );
    }

    #[test]
    fn empty_alternative_does_not_claim_first_nonempty_label_or_replace_first_href() {
        let document = parse(r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav><ol><li><a href=""><img alt="" title="Ignored"/></a><a href="later.xhtml"><img title="Later"/></a></li></ol></nav></html>"#).unwrap();
        let point = &document.lists()[0].points()[0];
        assert_eq!(point.label().map(EpubString::as_str), Some("Later"));
        assert_eq!(point.authored_href().map(AuthoredHref::as_str), Some(""));
    }

    #[test]
    fn label_depth_is_bounded_even_inside_substituted_content() {
        for owner in ["h1", "a", "span"] {
            for substitute in [false, true] {
                for depth in [MAX_NAV_DEPTH, MAX_NAV_DEPTH + 1] {
                    let opening = if substitute {
                        r#"<object title="Once">"#
                    } else {
                        "<span>"
                    };
                    let closing = if substitute { "</object>" } else { "</span>" };
                    let label = format!(
                        "<{owner}>{}Text{}</{owner}>",
                        opening.repeat(depth - 1),
                        closing.repeat(depth - 1)
                    );
                    let content = if owner == "h1" {
                        label
                    } else {
                        format!("<ol><li>{label}</li></ol>")
                    };
                    let xml = format!(
                        r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav>{content}</nav></html>"#
                    );
                    if depth == MAX_NAV_DEPTH {
                        assert!(parse(&xml).is_ok(), "{owner}, {substitute}");
                    } else {
                        assert!(
                            matches!(
                                parse(&xml),
                                Err(NavigationParseError::DepthLimitExceeded {
                                    limit: MAX_NAV_DEPTH
                                })
                            ),
                            "{owner}, {substitute}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn label_attribute_scans_preserve_presence_with_many_and_invalid_attributes() {
        let attributes = (0..1024)
            .map(|i| format!(" data-{i}=\"value\""))
            .collect::<String>();
        let xml = format!(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav><h1><img {attributes} alt="Heading"/></h1><ol><li><a><img {attributes} alt="" title="Ignored"/></a></li><li><span><img duplicate="one" duplicate="two" alt="First" alt="Second" title="Ignored"/></span></li><li><span><img alt="&unknown;" title="Ignored"/></span></li></ol></nav></html>"#
        );
        let document = parse(&xml).unwrap();
        let list = &document.lists()[0];
        assert_eq!(list.heading().unwrap().text(), "Heading");
        assert_eq!(list.points()[0].label(), None);
        assert_eq!(
            list.points()[1].label().map(EpubString::as_str),
            Some("First")
        );
        assert_eq!(list.points()[2].label(), None);
    }

    #[test]
    fn label_collector_checks_empty_element_depth_and_requires_owner_end() {
        for depth in [MAX_NAV_DEPTH, MAX_NAV_DEPTH + 1] {
            let xml = format!(
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav><h1>{}<img alt="Label"/>{}</h1></nav></html>"#,
                "<span>".repeat(depth - 2),
                "</span>".repeat(depth - 2),
            );
            assert_eq!(parse(&xml).is_ok(), depth == MAX_NAV_DEPTH);
        }
        for owner in ["h1", "a", "span"] {
            let prefix = if owner == "h1" { "" } else { "<ol><li>" };
            let xml = format!(
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><nav>{prefix}<{owner}><object title="Label"><span>Ignored</span>"#
            );
            assert!(
                matches!(parse(&xml), Err(NavigationParseError::UnexpectedEof { expected }) if expected == owner)
            );
        }
    }
}
