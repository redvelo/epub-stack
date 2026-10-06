//! The prefixed CSS properties EPUB retains, restated as the properties CSS defines.
//!
//! EPUB 3.4 keeps the `-epub-` prefixed properties as outdated features, defined in
//! EPUB 3.3 appendix E. A browser drops the ones it does not implement at parse time,
//! so a reading system that wants them honoured has to say them again. The rewrite
//! places each unprefixed declaration immediately after the prefixed one it came
//! from, which leaves specificity and authored order as the publication wrote them.
//!
//! Anything the rewrite cannot read with certainty is left exactly as authored: the
//! worst outcome is the stylesheet a reading system would have served anyway. That is
//! why this walks the source itself rather than tokenizing with `cssparser` as
//! reference extraction does: a rewrite needs the authored bytes back unchanged
//! wherever it is unsure, and it only ever inserts at a declaration it recognized.

/// The prefixed properties, each one named after the property it stands for.
const SUPPORTED: &[&str] = &[
    "-epub-hyphens",
    "-epub-line-break",
    "-epub-text-align-last",
    "-epub-text-combine",
    "-epub-text-combine-horizontal",
    "-epub-text-emphasis-color",
    "-epub-text-emphasis-position",
    "-epub-text-emphasis-style",
    "-epub-text-orientation",
    "-epub-text-underline-position",
    "-epub-word-break",
    "-epub-writing-mode",
];

/// The two whose property is not its prefixed name without the prefix.
const RENAMED: &[(&str, &str)] = &[
    ("-epub-text-combine", "text-combine-upright"),
    ("-epub-text-combine-horizontal", "text-combine-upright"),
];

/// A keyword CSS renamed, or that it has nothing to offer for.
type Keyword = (&'static str, Option<&'static str>);

/// Keywords CSS renamed, or that it has nothing to offer for.
const KEYWORDS: &[(&str, &[Keyword])] = &[
    ("-epub-hyphens", &[("all", None)]),
    (
        "-epub-text-combine",
        &[("none", Some("none")), ("horizontal", Some("all"))],
    ),
    (
        "-epub-text-combine-horizontal",
        &[("none", Some("none")), ("all", Some("all"))],
    ),
    (
        "-epub-text-orientation",
        &[
            ("vertical-right", Some("mixed")),
            ("rotate-right", Some("sideways")),
            ("rotate-normal", Some("sideways")),
        ],
    ),
    (
        "-epub-text-underline-position",
        &[("alphabetic", Some("auto"))],
    ),
];

/// Properties whose prefixed grammar is only the keywords listed for them.
const CLOSED: &[&str] = &["-epub-text-combine", "-epub-text-combine-horizontal"];

/// Restates every prefixed declaration in a stylesheet, or `None` when it has none.
///
/// A block that holds no block of its own is a declaration block, which reads
/// `@media`, `@supports` and `@keyframes` without a list of grouping at-rules. An
/// unterminated block is left as authored.
pub fn rewrite_prefixed_stylesheet(text: &str) -> Option<String> {
    if !names_prefix(text) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut insertions: Vec<(usize, String)> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = comment_end(text, index);
                continue;
            }
            b'"' | b'\'' => {
                index = string_end(text, index);
                continue;
            }
            b'{' => {
                if let Some(parent) = open.last_mut() {
                    parent.1 = true;
                }
                open.push((index + 1, false));
            }
            b'}' => {
                if let Some((start, false)) = open.pop() {
                    declaration_insertions(&text[start..index], start, &mut insertions);
                }
            }
            _ => {}
        }
        index += 1;
    }
    apply(text, insertions)
}

/// Restates every prefixed declaration in one declaration block, such as a `style`
/// attribute, or `None` when it has none.
pub fn rewrite_prefixed_declarations(block: &str) -> Option<String> {
    if !names_prefix(block) {
        return None;
    }
    let mut insertions = Vec::new();
    declaration_insertions(block, 0, &mut insertions);
    apply(block, insertions)
}

fn names_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .windows(6)
        .any(|window| window.eq_ignore_ascii_case(b"-epub-"))
}

fn apply(text: &str, mut insertions: Vec<(usize, String)>) -> Option<String> {
    if insertions.is_empty() {
        return None;
    }
    insertions.sort_by_key(|(at, _)| *at);
    let mut output = String::with_capacity(text.len() + insertions.len() * 32);
    let mut cursor = 0;
    for (at, declaration) in insertions {
        output.push_str(&text[cursor..at]);
        output.push(';');
        output.push_str(&declaration);
        cursor = at;
    }
    output.push_str(&text[cursor..]);
    Some(output)
}

/// Where each declaration in one block gains its restatement, as an offset into the
/// text the block came from. Strings, comments and parentheses are stepped over, so a
/// `url(data:…;base64,…)` or a quoted `-epub-` keeps its meaning.
fn declaration_insertions(block: &str, offset: usize, insertions: &mut Vec<(usize, String)>) {
    let bytes = block.as_bytes();
    let mut start = 0;
    let mut colon = None;
    let mut parentheses = 0_usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = comment_end(block, index);
                continue;
            }
            b'"' | b'\'' => {
                index = string_end(block, index);
                continue;
            }
            b'(' => parentheses += 1,
            b')' => parentheses = parentheses.saturating_sub(1),
            b':' if parentheses == 0 && colon.is_none() => colon = Some(index),
            b';' if parentheses == 0 => {
                record(block, offset, start, colon, index, insertions);
                start = index + 1;
                colon = None;
            }
            _ => {}
        }
        index += 1;
    }
    record(block, offset, start, colon, bytes.len(), insertions);
}

fn record(
    block: &str,
    offset: usize,
    start: usize,
    colon: Option<usize>,
    end: usize,
    insertions: &mut Vec<(usize, String)>,
) {
    let Some(colon) = colon else {
        return;
    };
    let property = block[start..colon].trim();
    if property.starts_with("--") {
        return;
    }
    let value = &block[colon + 1..end];
    let Some(declaration) = map_declaration(property, value) else {
        return;
    };
    // Immediately after the value rather than after the whitespace that follows it.
    let at = colon + 1 + value.trim_end().len();
    // A second pass over an already rewritten block would otherwise restate it again.
    if !block[at..].starts_with(&format!(";{declaration}")) {
        insertions.push((offset + at, declaration));
    }
}

fn map_declaration(property: &str, value: &str) -> Option<String> {
    let (body, important) = split_important(value);
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let name = property.to_ascii_lowercase();
    let suffix = if important { " !important" } else { "" };
    if name == "text-transform" {
        let mapped = map_text_transform(body)?;
        return Some(format!("text-transform: {mapped}{suffix}"));
    }
    if !SUPPORTED.contains(&name.as_str()) {
        return None;
    }
    let target = RENAMED
        .iter()
        .find(|(prefixed, _)| *prefixed == name)
        .map_or(&name[6..], |(_, property)| *property);
    let mapped = map_value(&name, body)?;
    Some(format!("{target}: {mapped}{suffix}"))
}

fn map_value<'a>(property: &str, value: &'a str) -> Option<&'a str> {
    let Some((_, replacements)) = KEYWORDS.iter().find(|(name, _)| *name == property) else {
        return Some(value);
    };
    let keyword = value.to_ascii_lowercase();
    match replacements
        .iter()
        .find(|(from, _)| *from == keyword.as_str())
    {
        Some((_, replacement)) => *replacement,
        None if CLOSED.contains(&property) => None,
        None => Some(value),
    }
}

/// `-epub-fullwidth` is a prefixed value rather than a property.
fn map_text_transform(value: &str) -> Option<String> {
    let mut replaced = false;
    let mapped = value
        .split_inclusive(char::is_whitespace)
        .map(|token| {
            if token.trim().eq_ignore_ascii_case("-epub-fullwidth") {
                replaced = true;
                return token.replacen(token.trim(), "full-width", 1);
            }
            token.to_owned()
        })
        .collect::<String>();
    replaced.then_some(mapped)
}

fn split_important(value: &str) -> (&str, bool) {
    let trimmed = value.trim_end();
    let Some(head) = trimmed
        .get(trimmed.len().saturating_sub(9)..)
        .filter(|tail| tail.eq_ignore_ascii_case("important"))
        .map(|_| &trimmed[..trimmed.len() - 9])
    else {
        return (value, false);
    };
    let head = head.trim_end();
    match head.strip_suffix('!') {
        Some(body) => (body, true),
        None => (value, false),
    }
}

fn string_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

fn comment_end(text: &str, start: usize) -> usize {
    text[start + 2..]
        .find("*/")
        .map_or(text.len(), |end| start + 2 + end + 2)
}

#[cfg(test)]
mod tests {
    use super::{rewrite_prefixed_declarations, rewrite_prefixed_stylesheet};

    #[test]
    fn restates_every_prefixed_property() {
        for (authored, expected) in [
            (
                "-epub-writing-mode: vertical-rl",
                "writing-mode: vertical-rl",
            ),
            (
                "-epub-text-orientation: upright",
                "text-orientation: upright",
            ),
            (
                "-epub-text-combine-horizontal: all",
                "text-combine-upright: all",
            ),
            (
                "-epub-text-combine: horizontal",
                "text-combine-upright: all",
            ),
            ("-epub-hyphens: manual", "hyphens: manual"),
            ("-epub-line-break: strict", "line-break: strict"),
            ("-epub-text-align-last: start", "text-align-last: start"),
            ("-epub-word-break: keep-all", "word-break: keep-all"),
            ("-epub-text-emphasis-color: red", "text-emphasis-color: red"),
            (
                "-epub-text-emphasis-position: over left",
                "text-emphasis-position: over left",
            ),
            (
                "-epub-text-emphasis-style: filled dot",
                "text-emphasis-style: filled dot",
            ),
            (
                "-epub-text-underline-position: under left",
                "text-underline-position: under left",
            ),
            (
                "text-transform: -epub-fullwidth",
                "text-transform: full-width",
            ),
        ] {
            assert_eq!(
                rewrite_prefixed_declarations(authored).as_deref(),
                Some(format!("{authored};{expected}").as_str()),
                "{authored}"
            );
        }
    }

    #[test]
    fn maps_kept_keywords_and_drops_those_without_an_equivalent() {
        for (authored, expected) in [
            (
                "-epub-text-orientation: vertical-right",
                "text-orientation: mixed",
            ),
            (
                "-epub-text-orientation: rotate-right",
                "text-orientation: sideways",
            ),
            (
                "-epub-text-underline-position: alphabetic",
                "text-underline-position: auto",
            ),
        ] {
            assert_eq!(
                rewrite_prefixed_declarations(authored).as_deref(),
                Some(format!("{authored};{expected}").as_str())
            );
        }
        for authored in [
            "-epub-hyphens: all",
            "-epub-text-combine: horizontal 2",
            "-epub-text-combine-horizontal: digits 2",
            "-epub-hyphens:",
        ] {
            assert_eq!(rewrite_prefixed_declarations(authored), None, "{authored}");
        }
    }

    #[test]
    fn keeps_authored_order_and_importance() {
        assert_eq!(
            rewrite_prefixed_declarations("hyphens: none; -epub-hyphens: auto").as_deref(),
            Some("hyphens: none; -epub-hyphens: auto;hyphens: auto")
        );
        assert_eq!(
            rewrite_prefixed_declarations("-epub-hyphens: auto; hyphens: none").as_deref(),
            Some("-epub-hyphens: auto;hyphens: auto; hyphens: none")
        );
        assert_eq!(
            rewrite_prefixed_declarations("margin: 1em; -epub-hyphens: none !important;")
                .as_deref(),
            Some("margin: 1em; -epub-hyphens: none !important;hyphens: none !important;")
        );
    }

    #[test]
    fn restates_a_declaration_only_once() {
        let once = rewrite_prefixed_declarations("-epub-line-break: loose").unwrap();
        assert_eq!(rewrite_prefixed_declarations(&once), None);
    }

    #[test]
    fn leaves_strings_comments_parentheses_and_custom_properties_alone() {
        for authored in [
            "content: \"-epub-hyphens: none\"",
            "/* -epub-hyphens: none */ color: red",
            "background: url(data:image/gif;base64,-epub-hyphens:none)",
            "--kept: -epub-hyphens: none",
            "color: red",
        ] {
            assert_eq!(rewrite_prefixed_declarations(authored), None, "{authored}");
        }
    }

    #[test]
    fn rewrites_blocks_inside_grouping_and_keyframe_at_rules() {
        let sheet = concat!(
            "@media (min-width: 30em) {\n",
            "  @supports (display: grid) { p.a { -epub-hyphens: none } }\n",
            "}\n",
            "@keyframes slide { from { -epub-word-break: break-all } }\n",
            "@font-face { font-family: X; src: url(x.otf) }\n",
            "p[class=\"-epub-trap\"]::before { content: \"{\" }"
        );
        let rewritten = rewrite_prefixed_stylesheet(sheet).unwrap();
        assert!(rewritten.contains("-epub-hyphens: none;hyphens: none"));
        assert!(rewritten.contains("-epub-word-break: break-all;word-break: break-all"));
        assert!(rewritten.contains("p[class=\"-epub-trap\"]::before { content: \"{\" }"));
        assert_eq!(rewritten.matches("hyphens").count(), 2);
    }

    #[test]
    fn reports_nothing_without_a_prefixed_declaration() {
        for sheet in [
            "p { color: red }",
            "p[data-x=\"-epub-hyphens\"] { color: red }",
            "/* -epub-hyphens */ p { color: red }",
            "p { -epub-hyphens: none",
        ] {
            assert_eq!(rewrite_prefixed_stylesheet(sheet), None, "{sheet}");
        }
    }
}
