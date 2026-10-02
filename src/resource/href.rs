use super::{AuthoredHref, EpubHref, EpubPath, ResourceAddress};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Syntactic classification of exact authored href text.
///
/// Parsing does not establish resource existence. Every variant retains the original text;
/// valid targets retain checked href syntax and fragments are percent-decoded as UTF-8.
pub(crate) enum ParsedHref {
    /// A source-relative local reference.
    Local {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked target text, including any query but excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// An HTTP(S) or scheme-relative URL.
    Remote {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked remote target excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A `data:` URL.
    Data {
        /// Exact authored data URL.
        original: AuthoredHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A valid non-HTTP, non-data URI with a scheme.
    ExternalScheme {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked target excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A fragment reference to the source document itself.
    SameDocument {
        /// Exact authored href.
        original: AuthoredHref,
        /// Percent-decoded fragment, which may be empty for `#`.
        fragment: String,
    },
    /// An exactly empty authored value.
    Empty {
        /// Empty authored href retained for source fidelity.
        original: AuthoredHref,
    },
    /// Non-empty authored text with rejected lexical or URL/path syntax.
    Invalid {
        /// Exact malformed authored href.
        original: AuthoredHref,
    },
}

impl ParsedHref {
    /// Returns a non-empty percent-decoded fragment when one exists.
    ///
    /// This intentionally maps an explicitly empty fragment such as `#` to `None`.
    pub(crate) fn fragment(&self) -> Option<&str> {
        match self {
            Self::Local { fragment, .. }
            | Self::Remote { fragment, .. }
            | Self::ExternalScheme { fragment, .. } => {
                fragment.as_deref().filter(|fragment| !fragment.is_empty())
            }
            Self::SameDocument { fragment, .. } => {
                (!fragment.is_empty()).then_some(fragment.as_str())
            }
            Self::Data { fragment, .. } => {
                fragment.as_deref().filter(|fragment| !fragment.is_empty())
            }
            Self::Empty { .. } | Self::Invalid { .. } => None,
        }
    }
}

/// Classifies exact authored href text without establishing target existence.
///
/// Local path and fragment percent escapes must decode as UTF-8. Encoded path separators,
/// malformed escapes, controls, backslashes, absolute local paths, and local scheme-like first
/// segments are rejected rather than repaired. The one repair is percent-encoding interior
/// spaces, which authoring tools commonly leave unencoded; the authored text stays available as
/// the original. Leading, trailing, and other whitespace remain invalid.
pub(crate) fn parse_href(href: AuthoredHref) -> ParsedHref {
    let original = href.clone();
    if href.as_str().is_empty() {
        return ParsedHref::Empty { original };
    }
    // Authoring tools commonly leave interior spaces unencoded; every other syntax fault,
    // including leading or trailing whitespace, stays invalid.
    let repaired;
    let value = if href.as_str().contains(' ') && href.as_str().trim() == href.as_str() {
        repaired = href.as_str().replace(' ', "%20");
        repaired.as_str()
    } else {
        href.as_str()
    };
    if !valid_href_syntax(value) {
        return ParsedHref::Invalid { original };
    }
    let (target, raw_fragment) = value
        .split_once('#')
        .map(|(target, fragment)| (target, Some(fragment)))
        .unwrap_or((value, None));
    let fragment = match raw_fragment {
        Some(fragment) => {
            let Ok(fragment) = percent_encoding::percent_decode_str(fragment).decode_utf8() else {
                return ParsedHref::Invalid { original };
            };
            Some(fragment.into_owned())
        }
        None => None,
    };
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if value.starts_with('#') {
        ParsedHref::SameDocument {
            original,
            fragment: fragment.unwrap_or_default(),
        }
    } else if target.starts_with('?') {
        ParsedHref::Local {
            original,
            target: EpubHref::try_new(target).expect("href syntax was checked"),
            fragment,
        }
    } else if target.starts_with("//")
        || starts_with_ascii_case_insensitive(target, "http://")
        || starts_with_ascii_case_insensitive(target, "https://")
    {
        match valid_remote_url(target)
            .then(|| EpubHref::try_new(target))
            .transpose()
        {
            Ok(Some(target)) => ParsedHref::Remote {
                original,
                target,
                fragment,
            },
            Ok(None) | Err(_) => ParsedHref::Invalid { original },
        }
    } else if starts_with_ascii_case_insensitive(target, "data:") {
        ParsedHref::Data { original, fragment }
    } else if has_scheme(target) {
        match EpubHref::try_new(target) {
            Ok(target) => ParsedHref::ExternalScheme {
                original,
                target,
                fragment,
            },
            Err(_) => ParsedHref::Invalid { original },
        }
    } else if path.starts_with('/')
        || path
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains(':'))
        || contains_encoded_separator(path)
        || decoded_local_path(path).is_none()
    {
        ParsedHref::Invalid { original }
    } else {
        match EpubHref::try_new(target) {
            Ok(target) => ParsedHref::Local {
                original,
                target,
                fragment,
            },
            Err(_) => ParsedHref::Invalid { original },
        }
    }
}

/// A local or non-local target resolved from authored href text.
///
/// Resolution does not establish resource or fragment existence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHref {
    /// Canonical local path or retained non-local address.
    pub address: ResourceAddress,
    /// The query of a local target without the leading `?`; remote queries stay in the address.
    pub query: Option<String>,
    /// The percent-decoded non-empty fragment of a local or remote target.
    pub fragment: Option<String>,
}

/// Authored href text that cannot be resolved to an address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("href cannot be resolved: {0}")]
pub struct InvalidHref(AuthoredHref);

impl InvalidHref {
    pub(crate) fn new(href: AuthoredHref) -> Self {
        Self(href)
    }

    /// Returns the exact authored text.
    pub fn authored(&self) -> &AuthoredHref {
        &self.0
    }
}

/// Resolves authored href text against the canonical path of the document containing it.
///
/// This is the resolution the resource index applies to authored links, available without an
/// index so that an application holding only a canonical path can reproduce it. Local paths are
/// percent-decoded and dot and empty segments are removed; no other authored syntax is repaired
/// except that interior spaces are percent-encoded. Resolution establishes address identity, not
/// resource or fragment existence.
///
/// # Errors
///
/// Returns [`InvalidHref`] for empty text, rejected syntax, and local targets that escape the
/// publication root.
pub fn resolve_href(href: &AuthoredHref, source: &EpubPath) -> Result<ResolvedHref, InvalidHref> {
    let parsed = parse_href(href.clone());
    let fragment = parsed.fragment().map(str::to_string);
    let invalid = || InvalidHref(href.clone());
    let resolved = match &parsed {
        ParsedHref::Remote { target, .. } => ResolvedHref {
            address: ResourceAddress::Remote(target.as_str().to_string()),
            query: None,
            fragment,
        },
        ParsedHref::Data { original, .. } => ResolvedHref {
            address: ResourceAddress::Data(original.as_str().to_string()),
            query: None,
            fragment: None,
        },
        ParsedHref::ExternalScheme { target, .. } => ResolvedHref {
            address: ResourceAddress::External(target.as_str().to_string()),
            query: None,
            fragment: None,
        },
        ParsedHref::Local { target, .. } => {
            let (path, query) = target
                .as_str()
                .split_once('?')
                .map_or((target.as_str(), None), |(path, query)| {
                    (path, Some(query.to_string()))
                });
            ResolvedHref {
                address: ResourceAddress::Local(
                    local_path_for_target(path, source).ok_or_else(invalid)?,
                ),
                query,
                fragment,
            }
        }
        ParsedHref::SameDocument { .. } => ResolvedHref {
            address: ResourceAddress::Local(source.clone()),
            query: None,
            fragment,
        },
        ParsedHref::Empty { .. } | ParsedHref::Invalid { .. } => return Err(invalid()),
    };
    Ok(resolved)
}

/// Resolves an authored reference that must identify one publication resource exactly.
///
/// This is the strict counterpart of [`resolve_href`], for authored references that name a
/// resource rather than link to a location in one: an annotation source, or a host-supplied
/// path checked at a trust boundary. It accepts only a local relative href, and rejects what
/// [`resolve_href`] accepts and repairs — a fragment, an empty path segment such as `a//b`, and
/// a trailing `.` or `..` segment, each checked after percent-decoding. A query is ignored,
/// because it does not participate in resource identity.
///
/// [`resolve_href`] remains the lenient resolver, and is what the resource index and analysis
/// apply to authored links.
pub fn resolve_publication_href(href: &AuthoredHref, source: &EpubPath) -> Option<EpubPath> {
    let target = href
        .as_str()
        .split_once('#')
        .map_or(href.as_str(), |(target, _)| target);
    let authored_path = target.split_once('?').map_or(target, |(path, _)| path);
    if !authored_path.is_empty() {
        let decoded = decoded_local_path(authored_path)?;
        let mut segments = decoded.split('/');
        if segments.clone().any(str::is_empty)
            || segments
                .next_back()
                .is_some_and(|segment| matches!(segment, "." | ".."))
        {
            return None;
        }
    }
    let (path, fragment) = resolve_local_href_from_source(href, source)?;
    fragment.is_none().then_some(path)
}

pub(crate) fn resolve_local_href_from_source(
    href: &AuthoredHref,
    source: &EpubPath,
) -> Option<(EpubPath, Option<String>)> {
    let ParsedHref::Local {
        target, fragment, ..
    } = parse_href(href.clone())
    else {
        return None;
    };
    let path = target
        .as_str()
        .split_once('?')
        .map_or(target.as_str(), |(path, _)| path);
    local_path_for_target(path, source).map(|path| (path, fragment))
}

fn local_path_for_target(authored_path: &str, source: &EpubPath) -> Option<EpubPath> {
    if authored_path.is_empty() {
        return Some(source.clone());
    }
    let decoded = decoded_local_path(authored_path)?;
    let mut parts = source
        .parent_dir()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            segment => parts.push(segment),
        }
    }
    EpubPath::new(parts.join("/")).ok()
}

fn has_scheme(value: &str) -> bool {
    value.split_once(':').is_some_and(|(scheme, _)| {
        scheme
            .as_bytes()
            .first()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
            && scheme
                .chars()
                .skip(1)
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    })
}

fn starts_with_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn valid_remote_url(value: &str) -> bool {
    if value.starts_with("//") {
        url::Url::parse(&format!("https:{value}")).is_ok()
    } else {
        url::Url::parse(value).is_ok()
    }
}

pub(super) fn valid_href_syntax(value: &str) -> bool {
    if value.trim() != value
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return false;
    }

    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let Some(_) = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok())
        else {
            return false;
        };
        index += 3;
    }
    true
}

fn contains_encoded_separator(value: &str) -> bool {
    value.as_bytes().windows(3).any(|escape| {
        escape[0] == b'%'
            && u8::from_str_radix(std::str::from_utf8(&escape[1..]).unwrap_or_default(), 16)
                .is_ok_and(|decoded| matches!(decoded, b'/' | b'\\'))
    })
}

fn decoded_local_path(value: &str) -> Option<std::borrow::Cow<'_, str>> {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()
}
