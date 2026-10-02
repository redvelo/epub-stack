use crate::analysis::AnalysisIssue;
use crate::analysis::reference::HrefRole;
use crate::resource::AuthoredHref;
use cssparser::{Parser, Token};

const MAX_NESTING_DEPTH: usize = 256;

#[derive(Debug)]
pub(crate) struct CssExtraction {
    pub(crate) references: Vec<CssPendingReference>,
    pub(crate) issue: Option<AnalysisIssue>,
}

#[derive(Debug, Clone)]
pub(crate) struct CssPendingReference {
    pub(crate) declared: AuthoredHref,
    pub(crate) kind: HrefRole,
    pub(crate) at_rule: Option<String>,
    pub(crate) property: Option<String>,
}

#[derive(Clone, Default)]
struct ScanContext {
    at_rule: Option<String>,
    property: Option<String>,
    expect_import: bool,
}

pub(crate) fn extract(bytes: &[u8]) -> Result<CssExtraction, AnalysisIssue> {
    let text = decode_css(bytes).ok_or(AnalysisIssue::Malformed)?;
    let mut parser = Parser::new(&text);
    // Defer to MAX_NESTING_DEPTH so depth limiting stays reportable rather than malformed.
    parser.set_nested_block_limit(0);
    let mut references = Vec::new();
    let mut malformed = false;
    let mut nesting_limited = false;
    scan(
        &mut parser,
        ScanContext::default(),
        &mut references,
        &mut malformed,
        &mut nesting_limited,
        0,
    );
    Ok(CssExtraction {
        references,
        issue: if nesting_limited {
            Some(AnalysisIssue::Unsupported)
        } else {
            malformed.then_some(AnalysisIssue::Malformed)
        },
    })
}

fn scan(
    parser: &mut Parser<'_>,
    mut context: ScanContext,
    references: &mut Vec<CssPendingReference>,
    malformed: &mut bool,
    nesting_limited: &mut bool,
    depth: usize,
) {
    let mut pending_at_rule = None;
    let mut pending_property = None;
    while let Ok(token) = parser.next_including_whitespace_and_comments().cloned() {
        if token.is_parse_error() {
            *malformed = true;
        }
        match token {
            Token::AtKeyword(name) => {
                let name = name.to_ascii_lowercase();
                context.expect_import = name == "import";
                pending_at_rule = Some(name);
            }
            Token::Ident(name) => pending_property = Some(name.to_string()),
            Token::Colon => context.property = pending_property.take(),
            Token::Semicolon => {
                context.property = None;
                context.expect_import = false;
                pending_at_rule = None;
            }
            Token::QuotedString(value) if context.expect_import => {
                push_reference(
                    references,
                    value.as_ref(),
                    HrefRole::CssImport,
                    Some("import"),
                    None,
                );
                context.expect_import = false;
            }
            Token::UnquotedUrl(value) => {
                let kind = reference_kind(&context);
                push_reference(
                    references,
                    value.as_ref(),
                    kind,
                    context.at_rule.as_deref().or(pending_at_rule.as_deref()),
                    context.property.as_deref(),
                );
                context.expect_import = false;
            }
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                let mut value = None;
                if parser
                    .parse_nested_block(|nested| {
                        while let Ok(token) = nested.next_including_whitespace_and_comments() {
                            if token.is_parse_error() {
                                *malformed = true;
                            }
                            match token {
                                Token::QuotedString(url)
                                | Token::Ident(url)
                                | Token::UnquotedUrl(url) => {
                                    value = Some(url.to_string());
                                }
                                _ => {}
                            }
                        }
                        Ok::<_, cssparser::ParseError<()>>(())
                    })
                    .is_err()
                {
                    *malformed = true;
                }
                if let Some(value) = value {
                    let kind = reference_kind(&context);
                    push_reference(
                        references,
                        &value,
                        kind,
                        context.at_rule.as_deref().or(pending_at_rule.as_deref()),
                        context.property.as_deref(),
                    );
                }
                context.expect_import = false;
            }
            Token::Function(_)
            | Token::CurlyBracketBlock
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock => {
                if depth >= MAX_NESTING_DEPTH {
                    *nesting_limited = true;
                    continue;
                }
                let mut nested_context = context.clone();
                if let Some(at_rule) = pending_at_rule.take() {
                    nested_context.at_rule = Some(at_rule);
                    nested_context.property = None;
                }
                if parser
                    .parse_nested_block(|nested| {
                        scan(
                            nested,
                            nested_context,
                            references,
                            malformed,
                            nesting_limited,
                            depth + 1,
                        );
                        Ok::<_, cssparser::ParseError<()>>(())
                    })
                    .is_err()
                {
                    *malformed = true;
                }
            }
            _ => {}
        }
    }
}

fn reference_kind(context: &ScanContext) -> HrefRole {
    if context.expect_import || context.at_rule.as_deref() == Some("import") {
        HrefRole::CssImport
    } else if context.at_rule.as_deref() == Some("font-face")
        && context
            .property
            .as_deref()
            .is_some_and(|property| property.eq_ignore_ascii_case("src"))
    {
        HrefRole::Font
    } else {
        HrefRole::CssUrl
    }
}

fn push_reference(
    references: &mut Vec<CssPendingReference>,
    value: &str,
    kind: HrefRole,
    at_rule: Option<&str>,
    property: Option<&str>,
) {
    references.push(CssPendingReference {
        declared: AuthoredHref::new(value),
        kind,
        at_rule: at_rule.map(str::to_string),
        property: property.map(str::to_string),
    });
}

fn decode_css(bytes: &[u8]) -> Option<String> {
    if let Some(content) = bytes.strip_prefix(&[0xff, 0xfe]) {
        if content.len() % 2 != 0 {
            return None;
        }
        let units = content
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).ok()
    } else if let Some(content) = bytes.strip_prefix(&[0xfe, 0xff]) {
        if content.len() % 2 != 0 {
            return None;
        }
        let units = content
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&units).ok()
    } else {
        std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .ok()
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_import_urls_and_font_sources() {
        let extraction = extract(
            br#"@import "base.css"; body { background: url(images/paper.png) } @font-face { src: url('font.woff2') format('woff2'); }"#,
        )
        .unwrap();
        assert_eq!(extraction.references.len(), 3);
        assert_eq!(extraction.references[0].kind, HrefRole::CssImport);
        assert_eq!(extraction.references[1].kind, HrefRole::CssUrl);
        assert_eq!(extraction.references[2].kind, HrefRole::Font);
        assert_eq!(extraction.issue, None);
    }

    #[test]
    fn preserves_references_but_marks_malformed_css_partial() {
        let extraction = extract(
            br#"a { background: url(first.png) } b { color: "bad
c { background: url(second.png) }"#,
        )
        .unwrap();

        assert_eq!(extraction.references.len(), 2);
        assert_eq!(extraction.issue, Some(AnalysisIssue::Malformed));
    }

    #[test]
    fn extracts_urls_nested_in_arbitrary_functions() {
        let extraction = extract(
            br#"body { background-image: image-set(url(first.png) 1x, cross-fade(url(second.png), url(third.png)) 2x); }"#,
        )
        .unwrap();

        assert_eq!(extraction.references.len(), 3);
        assert_eq!(extraction.references[0].declared.as_str(), "first.png");
        assert_eq!(extraction.references[1].declared.as_str(), "second.png");
        assert_eq!(extraction.references[2].declared.as_str(), "third.png");
        assert_eq!(extraction.issue, None);
    }

    #[test]
    fn preserves_context_for_urls_nested_in_functions() {
        let extraction = extract(br#"@font-face { src: wrapper(url(font.woff2)); }"#).unwrap();

        assert_eq!(extraction.references.len(), 1);
        assert_eq!(extraction.references[0].kind, HrefRole::Font);
        assert_eq!(
            extraction.references[0].at_rule.as_deref(),
            Some("font-face")
        );
        assert_eq!(extraction.references[0].property.as_deref(), Some("src"));
        assert_eq!(extraction.issue, None);
    }

    #[test]
    fn rejects_odd_length_utf16() {
        assert!(matches!(
            extract(&[0xff, 0xfe, b'a']),
            Err(AnalysisIssue::Malformed)
        ));
    }

    #[test]
    fn limits_nesting_while_retaining_earlier_references() {
        let mut css = "a { background: url(first.png); value: ".to_string();
        css.push_str(&"(".repeat(MAX_NESTING_DEPTH + 1));
        css.push_str(&")".repeat(MAX_NESTING_DEPTH + 1));
        css.push_str(" }");

        let extraction = extract(css.as_bytes()).unwrap();
        assert_eq!(extraction.references.len(), 1);
        assert_eq!(extraction.references[0].declared.as_str(), "first.png");
        assert_eq!(extraction.issue, Some(AnalysisIssue::Unsupported));
    }
}
