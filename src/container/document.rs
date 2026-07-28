use std::io::BufRead;

use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};

use super::ContainerError;
use crate::{
    package::{RenditionLayout, VERSION},
    string::EpubString,
    xml::decode_xml,
};

const CONTAINER: &str = "container";
const RENDITION_MEDIA_ATTR: &str = "rendition:media";
const RENDITION_LANGUAGE_ATTR: &str = "rendition:language";
const RENDITION_ACCESS_MODE_ATTR: &str = "rendition:accessMode";
const RENDITION_LAYOUT_ATTR: &str = "rendition:layout";
const RENDITION_LABEL_ATTR: &str = "rendition:label";
const ROOTFILE: &str = "rootfile";
const ROOTFILES: &str = "rootfiles";
const FULL_PATH: &str = "full-path";
const URN_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
const RENDITION_NS: &str = "http://www.idpf.org/2013/rendition";

type Result<T> = std::result::Result<T, ContainerError>;

fn required_container_string(value: impl AsRef<str>, field: &'static str) -> Result<EpubString> {
    EpubString::try_new(value).map_err(|_| ContainerError::EmptyField { field })
}

fn parsed_container_string(value: impl AsRef<str>) -> Option<EpubString> {
    EpubString::new(value)
}

fn is_ocf_element<R>(reader: &NsReader<R>, event: &BytesStart<'_>, name: &[u8]) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(namespace) if namespace.as_ref() == URN_NS.as_bytes())
        && local.as_ref() == name
}

fn is_xml_whitespace(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_whitespace)
}

fn parse_rootfile<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Result<Rootfile> {
    let (
        full_path,
        rendition_media,
        rendition_language,
        rendition_access_mode,
        rendition_layout,
        rendition_label,
    ) = event.attributes().try_fold(
        (None, None, None, None, None, None),
        |state, attr| -> Result<_> {
            let attr = attr.map_err(quick_xml::Error::from)?;
            let value = attr.normalized_value(quick_xml::XmlVersion::default())?;
            let (namespace, local) = reader.resolver().resolve_attribute(attr.key);
            let is_rendition_attr = matches!(
                namespace,
                ResolveResult::Bound(namespace) if namespace.as_ref() == RENDITION_NS.as_bytes()
            );
            match local.as_ref() {
                b"media-type" => Ok(state),
                b"full-path" if matches!(namespace, ResolveResult::Unbound) => {
                    let (
                        _,
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ) = state;
                    Ok((
                        parsed_container_string(value.as_ref()),
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ))
                }
                b"media" if is_rendition_attr => {
                    let (
                        full_path,
                        _,
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ) = state;
                    Ok((
                        full_path,
                        Some(value.into_owned()),
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ))
                }
                b"language" if is_rendition_attr => {
                    let (
                        full_path,
                        rendition_media,
                        _,
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ) = state;
                    Ok((
                        full_path,
                        rendition_media,
                        Some(value.into_owned()),
                        rendition_access_mode,
                        rendition_layout,
                        rendition_label,
                    ))
                }
                b"accessMode" if is_rendition_attr => {
                    let (
                        full_path,
                        rendition_media,
                        rendition_language,
                        _,
                        rendition_layout,
                        rendition_label,
                    ) = state;
                    Ok((
                        full_path,
                        rendition_media,
                        rendition_language,
                        Some(value.into_owned()),
                        rendition_layout,
                        rendition_label,
                    ))
                }
                b"layout" if is_rendition_attr => {
                    let (
                        full_path,
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        _,
                        rendition_label,
                    ) = state;
                    Ok((
                        full_path,
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        Some(value.into_owned()),
                        rendition_label,
                    ))
                }
                b"label" if is_rendition_attr => {
                    let (
                        full_path,
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        _,
                    ) = state;
                    Ok((
                        full_path,
                        rendition_media,
                        rendition_language,
                        rendition_access_mode,
                        rendition_layout,
                        Some(value.into_owned()),
                    ))
                }
                _ => Ok(state),
            }
        },
    )?;

    Ok(Rootfile::from_parsed(
        full_path,
        rendition_media,
        rendition_language,
        rendition_access_mode,
        rendition_layout,
        rendition_label,
    ))
}

pub(super) fn parse_rootfiles(input: impl BufRead) -> Result<Vec<Rootfile>> {
    let mut input = input;
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    let xml = decode_xml(&bytes).map_err(ContainerError::from)?;
    let mut reader = NsReader::from_reader(xml.as_bytes());
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut rootfiles = Vec::new();
    let mut depth = 0usize;
    let mut rootfiles_depth = None;
    let mut saw_container = false;
    let mut closed_container = false;
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if depth == 0 {
                    if saw_container {
                        return Err(ContainerError::MalformedContainer {
                            message: "multiple document elements".to_string(),
                        });
                    }
                    if !is_ocf_element(&reader, &event, CONTAINER.as_bytes()) {
                        return Err(ContainerError::MalformedContainer {
                            message: format!("expected {{{URN_NS}}}{CONTAINER} document element"),
                        });
                    }
                    saw_container = true;
                } else if depth == 1 && is_ocf_element(&reader, &event, ROOTFILES.as_bytes()) {
                    rootfiles_depth = Some(depth);
                } else if rootfiles_depth == Some(depth - 1)
                    && is_ocf_element(&reader, &event, ROOTFILE.as_bytes())
                {
                    rootfiles.push(parse_rootfile(&reader, &event)?);
                }
                depth += 1;
            }
            Event::Empty(event) => {
                if depth == 0 {
                    if saw_container {
                        return Err(ContainerError::MalformedContainer {
                            message: "multiple document elements".to_string(),
                        });
                    }
                    if !is_ocf_element(&reader, &event, CONTAINER.as_bytes()) {
                        return Err(ContainerError::MalformedContainer {
                            message: format!("expected {{{URN_NS}}}{CONTAINER} document element"),
                        });
                    }
                    saw_container = true;
                    closed_container = true;
                } else if rootfiles_depth == Some(depth - 1)
                    && is_ocf_element(&reader, &event, ROOTFILE.as_bytes())
                {
                    rootfiles.push(parse_rootfile(&reader, &event)?);
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return Err(ContainerError::MalformedContainer {
                        message: "content outside document element".to_string(),
                    });
                }
                depth = depth.saturating_sub(1);
                if saw_container && depth == 0 {
                    closed_container = true;
                }
                if rootfiles_depth == Some(depth) {
                    rootfiles_depth = None;
                }
            }
            Event::Text(event) if depth == 0 && !is_xml_whitespace(event.as_ref()) => {
                return Err(ContainerError::MalformedContainer {
                    message: "non-whitespace content outside document element".to_string(),
                });
            }
            Event::CData(_) if depth == 0 => {
                return Err(ContainerError::MalformedContainer {
                    message: "non-whitespace content outside document element".to_string(),
                });
            }
            Event::Eof => {
                if saw_container && (!closed_container || depth != 0) {
                    return Err(ContainerError::MalformedContainer {
                        message: "truncated container document".to_string(),
                    });
                }
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    if saw_container {
        Ok(rootfiles)
    } else {
        Err(ContainerError::MalformedContainer {
            message: "missing document element".to_string(),
        })
    }
}

pub(super) fn serialize_rootfiles<'a>(
    rootfiles: impl IntoIterator<Item = &'a Rootfile>,
) -> Result<String> {
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 4);
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    let mut container = BytesStart::new(CONTAINER);
    container.push_attribute(("xmlns", URN_NS));
    container.push_attribute(("xmlns:rendition", RENDITION_NS));
    container.push_attribute((VERSION, "1.0"));
    writer.write_event(Event::Start(container))?;
    writer.write_event(Event::Start(BytesStart::new(ROOTFILES)))?;
    rootfiles
        .into_iter()
        .try_for_each(|rootfile| rootfile.write_xml(&mut writer))?;
    writer.write_event(Event::End(BytesEnd::new(ROOTFILES)))?;
    writer.write_event(Event::End(BytesEnd::new(CONTAINER)))?;
    Ok(String::from_utf8_lossy(writer.into_inner().as_slice()).to_string())
}

#[derive(Debug, PartialEq, Eq, Clone)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A package-document rendition declared by `META-INF/container.xml`.
///
/// The path, media query, language, and label have surrounding whitespace trimmed, and empty
/// values are discarded. Parsed access-mode and layout attributes retain their authored strings
/// so unknown values survive normalized container generation.
pub struct Rootfile {
    full_path: Option<EpubString>,
    rendition_media: Option<EpubString>,
    rendition_language: Option<EpubString>,
    rendition_access_mode_raw: Option<String>,
    rendition_access_mode: Option<RenditionAccessMode>,
    rendition_layout_raw: Option<String>,
    rendition_layout: Option<RenditionLayout>,
    rendition_label: Option<EpubString>,
}

impl Rootfile {
    /// Creates a rendition pointing to a package document.
    ///
    /// Surrounding whitespace is trimmed. The remaining path is not resolved or normalized.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::EmptyField`] for an empty or whitespace-only path.
    pub fn new(full_path: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            full_path: Some(required_container_string(full_path, "rootfile full-path")?),
            rendition_media: None,
            rendition_language: None,
            rendition_access_mode_raw: None,
            rendition_access_mode: None,
            rendition_layout_raw: None,
            rendition_layout: None,
            rendition_label: None,
        })
    }

    fn from_parsed(
        full_path: Option<EpubString>,
        rendition_media_raw: Option<String>,
        rendition_language_raw: Option<String>,
        rendition_access_mode_raw: Option<String>,
        rendition_layout_raw: Option<String>,
        rendition_label_raw: Option<String>,
    ) -> Self {
        let rendition_media = rendition_media_raw.as_deref().and_then(EpubString::new);
        let rendition_language = rendition_language_raw.as_deref().and_then(EpubString::new);
        let rendition_access_mode = rendition_access_mode_raw
            .as_deref()
            .and_then(|value| value.parse().ok());
        let rendition_layout = rendition_layout_raw
            .as_deref()
            .and_then(|value| value.parse().ok());
        let rendition_label = rendition_label_raw.as_deref().and_then(EpubString::new);
        Self {
            full_path,
            rendition_media,
            rendition_language,
            rendition_access_mode_raw,
            rendition_access_mode,
            rendition_layout_raw,
            rendition_layout,
            rendition_label,
        }
    }

    /// Returns the trimmed `full-path`, or `None` when it was absent or empty when parsed.
    pub fn full_path(&self) -> Option<&EpubString> {
        self.full_path.as_ref()
    }

    /// Borrows the package path as text.
    ///
    /// The value is not path-normalized or resolved.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::MissingRootfilePath`] if no usable path was modeled.
    pub fn package_path(&self) -> Result<&str> {
        self.full_path()
            .map(EpubString::as_str)
            .ok_or(ContainerError::MissingRootfilePath)
    }

    /// Returns the trimmed, non-empty `rendition:media` value.
    pub fn rendition_media(&self) -> Option<&EpubString> {
        self.rendition_media.as_ref()
    }

    /// Returns the trimmed, non-empty `rendition:language` value.
    pub fn rendition_language(&self) -> Option<&EpubString> {
        self.rendition_language.as_ref()
    }

    /// Borrows the authored `rendition:accessMode` spelling, including invalid or empty text.
    pub fn rendition_access_mode_raw(&self) -> Option<&str> {
        self.rendition_access_mode_raw.as_deref()
    }

    /// Returns the recognized `rendition:accessMode` semantic value.
    pub fn rendition_access_mode(&self) -> Option<RenditionAccessMode> {
        self.rendition_access_mode
    }

    /// Borrows the authored `rendition:layout` spelling, including invalid or empty text.
    pub fn rendition_layout_raw(&self) -> Option<&str> {
        self.rendition_layout_raw.as_deref()
    }

    /// Returns the recognized `rendition:layout` semantic value.
    pub fn rendition_layout(&self) -> Option<RenditionLayout> {
        self.rendition_layout
    }

    /// Returns the trimmed, non-empty `rendition:label` value.
    pub fn rendition_label(&self) -> Option<&EpubString> {
        self.rendition_label.as_ref()
    }

    /// Replaces the package path, trimming surrounding whitespace.
    ///
    /// The remaining path is not normalized or resolved.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::EmptyField`] for an empty or whitespace-only value.
    pub fn with_full_path(mut self, full_path: impl AsRef<str>) -> Result<Self> {
        self.full_path = Some(required_container_string(full_path, "rootfile full-path")?);
        Ok(self)
    }

    /// Sets `rendition:media`, trimming surrounding whitespace.
    ///
    /// The remaining media query is not normalized or validated.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::EmptyField`] for an empty or whitespace-only value.
    pub fn with_rendition_media(mut self, media: impl AsRef<str>) -> Result<Self> {
        self.rendition_media = Some(required_container_string(
            media,
            "rootfile rendition media",
        )?);
        Ok(self)
    }

    /// Sets `rendition:language`, trimming surrounding whitespace.
    ///
    /// The remaining language tag is not normalized or validated.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::EmptyField`] for an empty or whitespace-only value.
    pub fn with_rendition_language(mut self, language: impl AsRef<str>) -> Result<Self> {
        self.rendition_language = Some(required_container_string(
            language,
            "rootfile rendition language",
        )?);
        Ok(self)
    }

    /// Sets the access mode and replaces any parsed raw spelling with its canonical token.
    pub fn with_rendition_access_mode(mut self, mode: RenditionAccessMode) -> Self {
        self.rendition_access_mode_raw = Some(mode.to_string());
        self.rendition_access_mode = Some(mode);
        self
    }

    /// Sets the layout and replaces any parsed raw spelling with its canonical token.
    pub fn with_rendition_layout(mut self, layout: RenditionLayout) -> Self {
        self.rendition_layout_raw = Some(layout.to_string());
        self.rendition_layout = Some(layout);
        self
    }

    /// Sets the human-readable `rendition:label`, trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::EmptyField`] for an empty or whitespace-only value.
    pub fn with_rendition_label(mut self, label: impl AsRef<str>) -> Result<Self> {
        self.rendition_label = Some(required_container_string(
            label,
            "rootfile rendition label",
        )?);
        Ok(self)
    }

    fn write_xml(&self, writer: &mut Writer<Vec<u8>>) -> Result<()> {
        let mut rootfile = BytesStart::new(ROOTFILE);
        rootfile.push_attribute(("media-type", "application/oebps-package+xml"));
        if let Some(value) = self.full_path() {
            rootfile.push_attribute((FULL_PATH, value.as_str()));
        }
        if let Some(value) = self.rendition_media() {
            rootfile.push_attribute((RENDITION_MEDIA_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_language() {
            rootfile.push_attribute((RENDITION_LANGUAGE_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_access_mode_raw() {
            rootfile.push_attribute((RENDITION_ACCESS_MODE_ATTR, value));
        }
        if let Some(value) = self.rendition_layout_raw() {
            rootfile.push_attribute((RENDITION_LAYOUT_ATTR, value));
        }
        if let Some(value) = self.rendition_label() {
            rootfile.push_attribute((RENDITION_LABEL_ATTR, value.as_str()));
        }
        writer.write_event(Event::Empty(rootfile))?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
/// A recognized OCF `rendition:accessMode` value.
pub enum RenditionAccessMode {
    /// Content is perceived through hearing.
    Auditory,
    /// Content is perceived through touch.
    Tactile,
    /// Content is perceived as text.
    Textual,
    /// Content is perceived visually.
    Visual,
}
