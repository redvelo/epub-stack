use crate::xml::{XmlAttrs, local_name, push_general_ref};
use quick_xml::{
    events::{BytesEnd, BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};
use std::io::BufRead;

pub(crate) type ParseResult<T> = Result<T, NavigationParseError>;

#[derive(Debug, thiserror::Error)]
/// Failure to parse a focused EPUB NAV or NCX document.
pub enum NavigationParseError {
    /// The document ended without a root element.
    #[error("Navigation document has no root element")]
    MissingRoot,
    /// The root local name does not match the requested navigation syntax.
    #[error("Expected navigation root {expected}, found {found}")]
    WrongRoot {
        /// The required root local name.
        expected: &'static str,
        /// The encountered root local name.
        found: String,
    },
    /// The root namespace does not match the requested navigation syntax.
    #[error("Expected namespace {expected} on {root}, found {found:?}")]
    WrongNamespace {
        /// The root local name being checked.
        root: &'static str,
        /// The required namespace URI.
        expected: &'static str,
        /// The encountered namespace URI, if bound.
        found: Option<String>,
    },
    /// The point tree exceeds the supported bounded depth.
    #[error("Navigation nesting exceeds the supported depth of {limit}")]
    DepthLimitExceeded {
        /// The maximum supported point depth.
        limit: usize,
    },
    /// The document ended before a required closing element.
    #[error("Unexpected end of navigation document before closing </{expected}>")]
    UnexpectedEof {
        /// The required closing element's local name.
        expected: &'static str,
    },
    /// Non-ignorable content follows the root element.
    #[error("Navigation document contains content after its root element")]
    TrailingContent,
    /// The underlying XML stream is not well formed or decodable.
    #[error("XML error: {source}")]
    Xml {
        /// The XML reader failure.
        #[from]
        source: quick_xml::Error,
    },
}

pub(crate) struct NavParseState;

impl NavParseState {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn attrs(&mut self, event: &BytesStart<'_>) -> XmlAttrs {
        let attrs = XmlAttrs::from_event(event);
        let _ = attrs.invalid;
        attrs
    }

    pub(crate) fn push_general_ref(
        &mut self,
        output: &mut String,
        reference: &quick_xml::events::BytesRef<'_>,
    ) -> ParseResult<()> {
        let _ = push_general_ref(output, reference)?;
        Ok(())
    }
}

pub(crate) fn is_element<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    namespace: &[u8],
    name: &[u8],
) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == namespace)
        && local.as_ref() == name
}

pub(crate) fn is_end<R>(
    reader: &NsReader<R>,
    event: &BytesEnd<'_>,
    namespace: &[u8],
    name: &[u8],
) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == namespace)
        && local.as_ref() == name
}

pub(crate) fn attr_value<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    namespace: Option<&[u8]>,
    name: &[u8],
) -> Option<String> {
    event
        .attributes()
        .with_checks(false)
        .filter_map(|attr| attr.ok())
        .find_map(|attr| {
            let (resolved, local) = reader.resolver().resolve_attribute(attr.key);
            let namespace_matches = match namespace {
                Some(expected) => {
                    matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == expected)
                }
                None => matches!(resolved, ResolveResult::Unbound),
            };
            (namespace_matches && local.as_ref() == name)
                .then(|| attr.normalized_value(quick_xml::XmlVersion::default()).ok())
                .flatten()
                .map(|value| value.to_string())
        })
}

pub(crate) fn checked_attr_value<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    state: &mut NavParseState,
    namespace: Option<&[u8]>,
    name: &[u8],
) -> Option<String> {
    state.attrs(event);
    attr_value(reader, event, namespace, name)
}

pub(crate) fn skip_element<R: BufRead>(
    reader: &mut NsReader<R>,
    end: &[u8],
    expected: &'static str,
) -> ParseResult<()> {
    let end = end.to_vec();
    let mut depth = 1usize;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) if event.name().as_ref() == end => depth += 1,
            Event::End(event) if event.name().as_ref() == end => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => return Err(NavigationParseError::UnexpectedEof { expected }),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

pub(crate) fn validate_root<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    expected_root: &'static str,
    expected_namespace: &'static str,
) -> ParseResult<()> {
    let found = String::from_utf8_lossy(local_name(event.name().as_ref())).into_owned();
    if found != expected_root {
        return Err(NavigationParseError::WrongRoot {
            expected: expected_root,
            found,
        });
    }
    let (resolved, _) = reader.resolver().resolve_element(event.name());
    let namespace = match resolved {
        ResolveResult::Bound(value) => Some(String::from_utf8_lossy(value.as_ref()).into_owned()),
        ResolveResult::Unbound | ResolveResult::Unknown(_) => None,
    };
    if namespace.as_deref() != Some(expected_namespace) {
        return Err(NavigationParseError::WrongNamespace {
            root: expected_root,
            expected: expected_namespace,
            found: namespace,
        });
    }
    Ok(())
}

pub(crate) fn finish_navigation_document<R: BufRead>(
    reader: &mut NsReader<R>,
    buf: &mut Vec<u8>,
) -> ParseResult<()> {
    loop {
        buf.clear();
        match reader.read_event_into(buf)? {
            Event::Eof => return Ok(()),
            Event::Text(text) if text.iter().all(u8::is_ascii_whitespace) => {}
            Event::Comment(_) | Event::PI(_) => {}
            _ => return Err(NavigationParseError::TrailingContent),
        }
    }
}
