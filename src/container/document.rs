use std::io::BufRead;

use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};

use super::{ContainerDocumentError, ContainerStructure, EpubZipError};
use crate::{
    media_type::MediaType,
    package::{RenditionLayout, VERSION},
    resource::EpubPath,
    string::{EpubString, EpubStringEmpty},
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
const MEDIA_TYPE: &str = "media-type";
const OPF_MEDIA_TYPE: &str = "application/oebps-package+xml";
const URN_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
const RENDITION_NS: &str = "http://www.idpf.org/2013/rendition";

type Result<T> = std::result::Result<T, ContainerDocumentError>;

fn is_ocf_element<R>(reader: &NsReader<R>, event: &BytesStart<'_>, name: &str) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(namespace) if namespace.as_ref() == URN_NS)
        && local.as_ref() == name
}

fn xml_error(source: quick_xml::Error) -> ContainerDocumentError {
    ContainerDocumentError::Xml { source }
}

fn is_xml_whitespace(text: &str) -> bool {
    text.trim_ascii().is_empty()
}

fn parse_rootfile<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Result<Rootfile> {
    let mut rootfile = Rootfile::default();
    for attr in event.attributes() {
        let attr = attr.map_err(|source| xml_error(quick_xml::Error::from(source)))?;
        let value = attr
            .normalized_value(quick_xml::XmlVersion::default())
            .map_err(xml_error)?;
        let (namespace, local) = reader.resolver().resolve_attribute(attr.key);
        let is_rendition_attr = matches!(
            namespace,
            ResolveResult::Bound(namespace) if namespace.as_ref() == RENDITION_NS
        );
        let unbound = matches!(namespace, ResolveResult::Unbound);
        match local.as_ref() {
            "full-path" if unbound => rootfile.full_path = RootfilePath::parse(value.as_ref()),
            "media-type" if unbound => rootfile.media_type = MediaType::new(value.as_ref()),
            "media" if is_rendition_attr => {
                rootfile.rendition_media = EpubString::new(value.as_ref());
            }
            "language" if is_rendition_attr => {
                rootfile.rendition_language = EpubString::new(value.as_ref());
            }
            "accessMode" if is_rendition_attr => {
                rootfile.rendition_access_mode = EpubString::new(value.as_ref()).map(Into::into);
            }
            "layout" if is_rendition_attr => {
                rootfile.rendition_layout = EpubString::new(value.as_ref()).map(Into::into);
            }
            "label" if is_rendition_attr => {
                rootfile.rendition_label = EpubString::new(value.as_ref());
            }
            _ => {}
        }
    }
    Ok(rootfile)
}

pub(super) fn parse_rootfiles(input: impl BufRead) -> Result<Vec<Rootfile>> {
    let mut input = input;
    let mut bytes = Vec::new();
    input
        .read_to_end(&mut bytes)
        .map_err(|source| ContainerDocumentError::Read {
            source: EpubZipError::io(source),
        })?;
    let xml = decode_xml(&bytes).map_err(|source| ContainerDocumentError::Decode { source })?;
    let mut reader = NsReader::from_reader(xml.as_bytes());
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut rootfiles = Vec::new();
    let mut depth = 0usize;
    let mut rootfiles_depth = None;
    let mut saw_container = false;
    let mut closed_container = false;
    loop {
        match reader.read_event_into(&mut buf).map_err(xml_error)? {
            Event::Start(event) => {
                if depth == 0 {
                    if saw_container {
                        return Err(ContainerDocumentError::Structure {
                            structure: ContainerStructure::MultipleDocumentElements,
                        });
                    }
                    if !is_ocf_element(&reader, &event, CONTAINER) {
                        return Err(ContainerDocumentError::Structure {
                            structure: ContainerStructure::UnexpectedDocumentElement,
                        });
                    }
                    saw_container = true;
                } else if depth == 1 && is_ocf_element(&reader, &event, ROOTFILES) {
                    rootfiles_depth = Some(depth);
                } else if rootfiles_depth == Some(depth - 1)
                    && is_ocf_element(&reader, &event, ROOTFILE)
                {
                    rootfiles.push(parse_rootfile(&reader, &event)?);
                }
                depth += 1;
            }
            Event::Empty(event) => {
                if depth == 0 {
                    if saw_container {
                        return Err(ContainerDocumentError::Structure {
                            structure: ContainerStructure::MultipleDocumentElements,
                        });
                    }
                    if !is_ocf_element(&reader, &event, CONTAINER) {
                        return Err(ContainerDocumentError::Structure {
                            structure: ContainerStructure::UnexpectedDocumentElement,
                        });
                    }
                    saw_container = true;
                    closed_container = true;
                } else if rootfiles_depth == Some(depth - 1)
                    && is_ocf_element(&reader, &event, ROOTFILE)
                {
                    rootfiles.push(parse_rootfile(&reader, &event)?);
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return Err(ContainerDocumentError::Structure {
                        structure: ContainerStructure::ContentOutsideDocumentElement,
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
                return Err(ContainerDocumentError::Structure {
                    structure: ContainerStructure::ContentOutsideDocumentElement,
                });
            }
            Event::CData(_) if depth == 0 => {
                return Err(ContainerDocumentError::Structure {
                    structure: ContainerStructure::ContentOutsideDocumentElement,
                });
            }
            Event::Eof => {
                if saw_container && (!closed_container || depth != 0) {
                    return Err(ContainerDocumentError::Structure {
                        structure: ContainerStructure::Truncated,
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
        Err(ContainerDocumentError::Structure {
            structure: ContainerStructure::MissingDocumentElement,
        })
    }
}

pub(super) fn serialize_rootfiles<'a>(rootfiles: impl IntoIterator<Item = &'a Rootfile>) -> String {
    const INFALLIBLE: &str = "writing container XML to a vector cannot fail";
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 4);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .expect(INFALLIBLE);

    let mut container = BytesStart::new(CONTAINER);
    container.push_attribute(("xmlns", URN_NS));
    container.push_attribute(("xmlns:rendition", RENDITION_NS));
    container.push_attribute((VERSION, "1.0"));
    writer
        .write_event(Event::Start(container))
        .expect(INFALLIBLE);
    writer
        .write_event(Event::Start(BytesStart::new(ROOTFILES)))
        .expect(INFALLIBLE);
    for rootfile in rootfiles {
        rootfile.write_xml(&mut writer);
    }
    writer
        .write_event(Event::End(BytesEnd::new(ROOTFILES)))
        .expect(INFALLIBLE);
    writer
        .write_event(Event::End(BytesEnd::new(CONTAINER)))
        .expect(INFALLIBLE);
    String::from_utf8(writer.into_inner()).expect("XML writer emits UTF-8")
}

/// A rootfile `rendition:accessMode` token retaining its authored spelling.
pub type RenditionAccessModeToken = crate::vocab::VocabToken<RenditionAccessMode>;

/// A rootfile `rendition:layout` token retaining its authored spelling.
pub type RenditionLayoutToken = crate::vocab::VocabToken<RenditionLayout>;

#[derive(Debug, PartialEq, Eq, Clone)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(tag = "state", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The authored `full-path` of a rootfile.
pub enum RootfilePath {
    /// A canonical provider-relative package path.
    Canonical(
        /// Resolved canonical path.
        EpubPath,
    ),
    /// Authored text that is not a canonical publication path, retained exactly.
    Invalid(
        /// Exact authored text with surrounding whitespace removed.
        EpubString,
    ),
}

impl RootfilePath {
    fn parse(value: impl AsRef<str>) -> Option<Self> {
        let authored = EpubString::new(value)?;
        Some(match EpubPath::new(authored.as_str()) {
            Ok(path) => Self::Canonical(path),
            Err(_) => Self::Invalid(authored),
        })
    }

    /// Returns the canonical path, or `None` when the authored text was not canonical.
    pub fn canonical(&self) -> Option<&EpubPath> {
        match self {
            Self::Canonical(path) => Some(path),
            Self::Invalid(_) => None,
        }
    }

    /// Returns the authored text, canonical or not.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Canonical(path) => path.as_str(),
            Self::Invalid(authored) => authored.as_str(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A package-document rendition declared by `META-INF/container.xml`.
///
/// Every authored attribute has surrounding whitespace removed and is discarded when nothing
/// remains. Unrecognized media types, access modes, and layouts retain their authored spelling
/// and survive normalized container generation.
pub struct Rootfile {
    full_path: Option<RootfilePath>,
    media_type: Option<MediaType>,
    rendition_media: Option<EpubString>,
    rendition_language: Option<EpubString>,
    rendition_access_mode: Option<RenditionAccessModeToken>,
    rendition_layout: Option<RenditionLayoutToken>,
    rendition_label: Option<EpubString>,
}

impl Rootfile {
    /// Creates a rendition pointing at a canonical package document path.
    pub fn new(full_path: EpubPath) -> Self {
        Self {
            full_path: Some(RootfilePath::Canonical(full_path)),
            media_type: MediaType::new(OPF_MEDIA_TYPE),
            ..Self::default()
        }
    }

    /// Borrows the authored `full-path`, or `None` when it was absent or empty.
    pub fn full_path(&self) -> Option<&RootfilePath> {
        self.full_path.as_ref()
    }

    /// Returns the canonical package path when one was authored and is usable.
    pub fn package_path(&self) -> Option<&EpubPath> {
        self.full_path.as_ref().and_then(RootfilePath::canonical)
    }

    /// Borrows the authored `media-type`, retained even when it is not the OPF media type.
    pub fn media_type(&self) -> Option<&MediaType> {
        self.media_type.as_ref()
    }

    /// Reports whether this rootfile declares the OPF package media type.
    ///
    /// A rootfile with no authored `media-type` is not treated as a package document.
    pub fn is_package_document(&self) -> bool {
        self.media_type
            .as_ref()
            .is_some_and(|media_type| media_type.has_essence(OPF_MEDIA_TYPE))
    }

    /// Returns the trimmed, non-empty `rendition:media` value.
    pub fn rendition_media(&self) -> Option<&EpubString> {
        self.rendition_media.as_ref()
    }

    /// Returns the trimmed, non-empty `rendition:language` value.
    pub fn rendition_language(&self) -> Option<&EpubString> {
        self.rendition_language.as_ref()
    }

    /// Borrows the authored `rendition:accessMode`, recognized or not.
    pub fn rendition_access_mode(&self) -> Option<&RenditionAccessModeToken> {
        self.rendition_access_mode.as_ref()
    }

    /// Borrows the authored `rendition:layout`, recognized or not.
    pub fn rendition_layout(&self) -> Option<&RenditionLayoutToken> {
        self.rendition_layout.as_ref()
    }

    /// Returns the trimmed, non-empty `rendition:label` value.
    pub fn rendition_label(&self) -> Option<&EpubString> {
        self.rendition_label.as_ref()
    }

    /// Replaces the package path.
    ///
    /// These setters are the authoring half of [`EpubZip::add_rootfile`], for building the
    /// rendition entries of a multi-rendition container.
    ///
    /// [`EpubZip::add_rootfile`]: crate::container::EpubZip::add_rootfile
    pub fn with_package_path(mut self, full_path: EpubPath) -> Self {
        self.full_path = Some(RootfilePath::Canonical(full_path));
        self
    }

    /// Replaces the authored `media-type`.
    pub fn with_media_type(mut self, media_type: MediaType) -> Self {
        self.media_type = Some(media_type);
        self
    }

    /// Sets `rendition:media`, trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for an empty or whitespace-only value.
    pub fn with_rendition_media(
        mut self,
        media: impl AsRef<str>,
    ) -> std::result::Result<Self, EpubStringEmpty> {
        self.rendition_media = Some(EpubString::try_new(media)?);
        Ok(self)
    }

    /// Sets `rendition:language`, trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for an empty or whitespace-only value.
    pub fn with_rendition_language(
        mut self,
        language: impl AsRef<str>,
    ) -> std::result::Result<Self, EpubStringEmpty> {
        self.rendition_language = Some(EpubString::try_new(language)?);
        Ok(self)
    }

    /// Sets the access mode, retaining the authored spelling of unrecognized values.
    pub fn with_rendition_access_mode(mut self, mode: impl Into<RenditionAccessModeToken>) -> Self {
        self.rendition_access_mode = Some(mode.into());
        self
    }

    /// Sets the layout, retaining the authored spelling of unrecognized values.
    pub fn with_rendition_layout(mut self, layout: impl Into<RenditionLayoutToken>) -> Self {
        self.rendition_layout = Some(layout.into());
        self
    }

    /// Sets the human-readable `rendition:label`, trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for an empty or whitespace-only value.
    pub fn with_rendition_label(
        mut self,
        label: impl AsRef<str>,
    ) -> std::result::Result<Self, EpubStringEmpty> {
        self.rendition_label = Some(EpubString::try_new(label)?);
        Ok(self)
    }

    fn write_xml(&self, writer: &mut Writer<Vec<u8>>) {
        let mut rootfile = BytesStart::new(ROOTFILE);
        if let Some(value) = self.full_path() {
            rootfile.push_attribute((FULL_PATH, value.as_str()));
        }
        if let Some(value) = self.media_type() {
            rootfile.push_attribute((MEDIA_TYPE, value.as_str()));
        }
        if let Some(value) = self.rendition_media() {
            rootfile.push_attribute((RENDITION_MEDIA_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_language() {
            rootfile.push_attribute((RENDITION_LANGUAGE_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_access_mode() {
            rootfile.push_attribute((RENDITION_ACCESS_MODE_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_layout() {
            rootfile.push_attribute((RENDITION_LAYOUT_ATTR, value.as_str()));
        }
        if let Some(value) = self.rendition_label() {
            rootfile.push_attribute((RENDITION_LABEL_ATTR, value.as_str()));
        }
        writer
            .write_event(Event::Empty(rootfile))
            .expect("writing container XML to a vector cannot fail");
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
