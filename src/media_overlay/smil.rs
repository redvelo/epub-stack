use crate::analysis::reference::HrefRole;
use crate::media_overlay::{SmilFacts, SmilNodeFact, SmilNodeId, SmilTime};
use crate::resource::{AuthoredHref, EpubHref, ParsedHref, parse_href};
use crate::semantics::TextDirection;
use crate::string::{EpubString, optional_epub_string};
use crate::xml::{
    XmlAttrs, XmlUtf8Reader, cdata_content, local_name, normalize_optional, push_general_ref,
    text_content,
};
use quick_xml::Writer;
use quick_xml::escape::escape;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use std::io::{BufRead, BufReader};
use std::str::FromStr;

const SMIL: &str = "smil";
const HEAD: &str = "head";
const BODY: &str = "body";
const METADATA: &str = "metadata";
const META: &str = "meta";
const SEQ: &str = "seq";
const PAR: &str = "par";
const TEXT: &str = "text";
const AUDIO: &str = "audio";
const SRC: &str = "src";
const ID: &str = "id";
const CLASS: &str = "class";
const TYPE: &str = "type";
const CUSTOM_TEST: &str = "customTest";
const TEXTREF: &str = "textref";
const NAME: &str = "name";
const CONTENT: &str = "content";
const SCHEME: &str = "scheme";
const REGION: &str = "region";
const CLIP_BEGIN: &str = "clipBegin";
const CLIP_END: &str = "clipEnd";
const SMIL_NS: &str = "http://www.w3.org/ns/SMIL";
const EPUB_NS: &str = "http://www.idpf.org/2007/ops";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

const VERSION: &str = "version";
const LANG: &str = "lang";
const DIR: &str = "dir";

const DEFAULT_VERSION: &str = "3.0";

type Result<T> = std::result::Result<T, SmilError>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Failure to parse or serialize the normalized standalone SMIL model.
pub enum SmilError {
    /// The input ended without a document element.
    #[error("SMIL document has no root element")]
    MissingRoot,
    /// The document element was not `smil`.
    #[error("Expected SMIL root smil, found {found}")]
    WrongRoot {
        /// The local name of the document element.
        found: String,
    },
    /// The document element did not use the SMIL namespace.
    #[error("Expected SMIL namespace {expected}, found {found:?}")]
    WrongNamespace {
        /// The required SMIL namespace URI.
        expected: &'static str,
        /// The namespace URI found on the root, if any.
        found: Option<String>,
    },
    /// More than one `head` element was encountered.
    #[error("SMIL document contains more than one head element")]
    DuplicateHead,
    /// More than one `body` element was encountered.
    #[error("SMIL document contains more than one body element")]
    DuplicateBody,
    /// A parallel container contained more than one text child.
    #[error("SMIL par contains more than one text element")]
    RepeatedText,
    /// A parallel container contained more than one audio child.
    #[error("SMIL par contains more than one audio element")]
    RepeatedAudio,
    /// The input ended before an open element was closed.
    #[error("Unexpected end of SMIL document before closing </{expected}>")]
    UnexpectedEof {
        /// The local name of the expected closing element.
        expected: String,
    },
    /// Non-whitespace content followed the document element.
    #[error("SMIL document contains content after its root element")]
    TrailingContent,
    /// Parsing exceeded the configured XML node budget.
    #[error("SMIL XML node count exceeds the configured limit of {limit}")]
    NodeLimitExceeded {
        /// The maximum accepted number of XML nodes.
        limit: usize,
    },
    /// Parsing exceeded the configured XML nesting budget.
    #[error("SMIL XML nesting exceeds the configured limit of {limit}")]
    NestingLimitExceeded {
        /// The maximum accepted element nesting depth.
        limit: usize,
    },
    /// An authored clip attribute was not a supported clock value.
    #[error("SMIL {attribute} is not a recognized clock value: {value}")]
    InvalidClock {
        /// The clip attribute being parsed.
        attribute: &'static str,
        /// The unrecognized authored value.
        value: String,
    },
    /// The end of an audio clip was not later than its beginning.
    #[error("SMIL audio clipEnd must be later than clipBegin: {begin} >= {end}")]
    InvalidClipRange {
        /// The normalized `clipBegin` value.
        begin: String,
        /// The normalized `clipEnd` value.
        end: String,
    },
    /// XML tokenization or writing failed.
    #[error("XML error: {source}")]
    Xml {
        /// The underlying XML failure.
        #[from]
        source: quick_xml::Error,
    },
    /// Reading parser input failed.
    #[error("IO error: {source}")]
    Io {
        /// The underlying I/O failure.
        #[from]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SmilParseLimits {
    pub(crate) max_nodes: usize,
    pub(crate) max_nesting: usize,
}

impl SmilParseLimits {
    pub(crate) const fn new(max_nodes: usize, max_nesting: usize) -> Self {
        Self {
            max_nodes,
            max_nesting,
        }
    }

    const fn unbounded() -> Self {
        Self::new(usize::MAX, usize::MAX)
    }
}

impl Default for SmilParseLimits {
    fn default() -> Self {
        Self::new(100_000, 256)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A normalized standalone SMIL document for focused parse, construction, and serialization.
pub struct SmilDocument {
    version: Option<EpubString>,
    xml_lang: Option<EpubString>,
    id: Option<EpubString>,
    head: SmilHead,
    body: SmilBody,
}

#[bon::bon]
impl SmilDocument {
    /// Builds a normalized SMIL document around a caller-supplied body.
    #[builder]
    pub fn new(
        body: SmilBody,
        #[builder(default)] head: SmilHead,
        xml_lang: Option<EpubString>,
        id: Option<EpubString>,
    ) -> Self {
        Self {
            version: EpubString::new(DEFAULT_VERSION),
            xml_lang,
            id,
            head,
            body,
        }
    }

    fn empty_parsed() -> Self {
        Self {
            version: EpubString::new(DEFAULT_VERSION),
            xml_lang: None,
            id: None,
            head: SmilHead::new(),
            body: SmilBody::new(),
        }
    }

    /// Parses a standalone document while preserving modeled authored states.
    ///
    /// Parsing accepts at most 100,000 XML nodes and 256 nested elements.
    pub fn parse(xml: &str) -> Result<Self> {
        parse_smil(xml.as_bytes())
    }

    #[cfg(test)]
    pub(crate) fn parse_with_limits<R: BufRead>(input: R, limits: SmilParseLimits) -> Result<Self> {
        parse_smil_with_limits(input, limits)
    }

    /// Serializes the modeled document as normalized, indented UTF-8 XML.
    pub fn to_string(&self) -> Result<String> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 4);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

        let mut smil = BytesStart::new(SMIL);
        push_escaped_attribute(&mut smil, "xmlns", SMIL_NS);
        push_escaped_attribute(&mut smil, "xmlns:epub", EPUB_NS);
        if let Some(version) = self.version.as_deref() {
            push_escaped_attribute(&mut smil, VERSION, version);
        }
        if let Some(lang) = self.xml_lang.as_deref() {
            push_escaped_attribute(&mut smil, "xml:lang", lang);
        }
        if let Some(id) = self.id.as_deref() {
            push_escaped_attribute(&mut smil, ID, id);
        }
        writer.write_event(Event::Start(smil))?;

        write_head(&mut writer, &self.head)?;
        write_body(&mut writer, &self.body)?;

        writer.write_event(Event::End(BytesEnd::new(SMIL)))?;
        Ok(String::from_utf8_lossy(writer.into_inner().as_slice()).to_string())
    }

    /// Returns the authored or normalized SMIL version.
    pub fn version(&self) -> Option<&EpubString> {
        self.version.as_ref()
    }

    /// Returns the root `xml:lang` value.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }

    /// Returns the root element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Returns the document head.
    pub fn head(&self) -> &SmilHead {
        &self.head
    }

    /// Returns the document body.
    pub fn body(&self) -> &SmilBody {
        &self.body
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// The document head and its modeled metadata entries.
pub struct SmilHead {
    metadata: Vec<SmilMeta>,
}

impl SmilHead {
    /// Creates an empty SMIL head.
    pub fn new() -> Self {
        Self {
            metadata: Vec::new(),
        }
    }

    /// Returns metadata entries in document order.
    pub fn metadata(&self) -> &[SmilMeta] {
        self.metadata.as_slice()
    }

    /// Appends a metadata entry.
    pub fn add_meta(&mut self, meta: SmilMeta) {
        self.metadata.push(meta);
    }
}

impl Default for SmilHead {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// One modeled SMIL metadata entry.
pub struct SmilMeta {
    name: Option<EpubString>,
    content: Option<EpubString>,
    scheme: Option<EpubString>,
    xml_lang: Option<EpubString>,
    dir: Option<TextDirection>,
    id: Option<EpubString>,
}

#[bon::bon]
impl SmilMeta {
    /// Builds normalized metadata with its required name and content.
    #[builder]
    pub fn new(
        name: EpubString,
        content: EpubString,
        scheme: Option<EpubString>,
        xml_lang: Option<EpubString>,
        dir: Option<TextDirection>,
        id: Option<EpubString>,
    ) -> Self {
        Self {
            name: Some(name),
            content: Some(content),
            scheme,
            xml_lang,
            dir,
            id,
        }
    }

    /// Returns the metadata name.
    pub fn name(&self) -> Option<&EpubString> {
        self.name.as_ref()
    }

    /// Returns the metadata content.
    pub fn content(&self) -> Option<&EpubString> {
        self.content.as_ref()
    }

    /// Returns the optional metadata scheme.
    pub fn scheme(&self) -> Option<&EpubString> {
        self.scheme.as_ref()
    }

    /// Returns the optional metadata language.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }

    /// Returns the optional metadata base direction.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }

    /// Returns the optional metadata element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// The ordered top-level timing children of a SMIL body.
pub struct SmilBody {
    children: Vec<SmilSequenceChild>,
}

impl SmilBody {
    /// Creates an empty SMIL body.
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Returns top-level timing children in document order.
    pub fn children(&self) -> &[SmilSequenceChild] {
        self.children.as_slice()
    }

    /// Appends a sequence or parallel child in serialization order.
    pub fn add_child(&mut self, child: impl Into<SmilSequenceChild>) {
        self.children.push(child.into());
    }
}

impl Default for SmilBody {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A sequential SMIL timing container.
pub struct SmilSeq {
    children: Vec<SmilSequenceChild>,
    epub_type: Option<EpubString>,
    id: Option<EpubString>,
    class: Option<EpubString>,
    custom_test: Option<EpubString>,
    authored_textref: Option<AuthoredHref>,
}

#[bon::bon]
impl SmilSeq {
    /// Builds a normalized sequence and its ordered children.
    #[builder]
    pub fn new(
        #[builder(default)] children: Vec<SmilSequenceChild>,
        epub_type: Option<EpubString>,
        id: Option<EpubString>,
        class: Option<EpubString>,
        custom_test: Option<EpubString>,
        textref: Option<EpubHref>,
    ) -> Self {
        Self {
            children,
            epub_type,
            id,
            class,
            custom_test,
            authored_textref: textref.as_ref().map(AuthoredHref::from),
        }
    }

    /// Returns nested timing children in document order.
    pub fn children(&self) -> &[SmilSequenceChild] {
        self.children.as_slice()
    }

    /// Returns the optional `epub:type` value.
    pub fn epub_type(&self) -> Option<&EpubString> {
        self.epub_type.as_ref()
    }

    /// Returns the optional element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Returns the optional CSS class value.
    pub fn class(&self) -> Option<&EpubString> {
        self.class.as_ref()
    }

    /// Returns the optional custom-test value.
    pub fn custom_test(&self) -> Option<&EpubString> {
        self.custom_test.as_ref()
    }

    /// Returns the authored `epub:textref` value.
    pub fn authored_textref(&self) -> Option<&AuthoredHref> {
        self.authored_textref.as_ref()
    }

    /// Parses the authored `epub:textref` value on demand.
    pub fn parsed_textref(&self) -> Option<ParsedHref> {
        self.authored_textref.clone().map(parse_href)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A parallel SMIL timing container with at most one text and one audio child.
pub struct SmilPar {
    children: Vec<SmilParallelChild>,
    epub_type: Option<EpubString>,
    id: Option<EpubString>,
    class: Option<EpubString>,
    custom_test: Option<EpubString>,
    authored_textref: Option<AuthoredHref>,
}

#[bon::bon]
impl SmilPar {
    /// Builds a normalized parallel with exactly one text child and at most one audio child.
    #[builder]
    pub fn new(
        text: SmilText,
        audio: Option<SmilAudio>,
        epub_type: Option<EpubString>,
        id: Option<EpubString>,
        class: Option<EpubString>,
        custom_test: Option<EpubString>,
        textref: Option<EpubHref>,
    ) -> Self {
        let mut children = vec![SmilParallelChild::Text(text)];
        children.extend(audio.map(SmilParallelChild::Audio));
        Self {
            children,
            epub_type,
            id,
            class,
            custom_test,
            authored_textref: textref.as_ref().map(AuthoredHref::from),
        }
    }

    /// Returns text and audio children in document order.
    pub fn children(&self) -> &[SmilParallelChild] {
        &self.children
    }

    /// Returns the first text child.
    pub fn text(&self) -> Option<&SmilText> {
        self.children.iter().find_map(|child| match child {
            SmilParallelChild::Text(text) => Some(text),
            SmilParallelChild::Audio(_) => None,
        })
    }

    /// Returns the first audio child.
    pub fn audio(&self) -> Option<&SmilAudio> {
        self.children.iter().find_map(|child| match child {
            SmilParallelChild::Audio(audio) => Some(audio),
            SmilParallelChild::Text(_) => None,
        })
    }

    /// Returns the optional `epub:type` value.
    pub fn epub_type(&self) -> Option<&EpubString> {
        self.epub_type.as_ref()
    }

    /// Returns the optional element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Returns the optional CSS class value.
    pub fn class(&self) -> Option<&EpubString> {
        self.class.as_ref()
    }

    /// Returns the optional custom-test value.
    pub fn custom_test(&self) -> Option<&EpubString> {
        self.custom_test.as_ref()
    }

    /// Returns the authored `epub:textref` value.
    pub fn authored_textref(&self) -> Option<&AuthoredHref> {
        self.authored_textref.as_ref()
    }

    /// Parses the authored `epub:textref` value on demand.
    pub fn parsed_textref(&self) -> Option<ParsedHref> {
        self.authored_textref.clone().map(parse_href)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A text or audio child retained in authored order within a parsed parallel.
pub enum SmilParallelChild {
    /// The parallel's text target.
    Text(SmilText),
    /// The parallel's audio target.
    Audio(SmilAudio),
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A SMIL text target.
pub struct SmilText {
    authored_src: Option<AuthoredHref>,
    id: Option<EpubString>,
    region: Option<EpubString>,
    epub_type: Option<EpubString>,
}

#[bon::bon]
impl SmilText {
    /// Builds a normalized text node with a required local or remote source href.
    #[builder]
    pub fn new(
        src: EpubHref,
        id: Option<EpubString>,
        region: Option<EpubString>,
        epub_type: Option<EpubString>,
    ) -> Self {
        Self {
            authored_src: Some(AuthoredHref::from(&src)),
            id,
            region,
            epub_type,
        }
    }

    /// Returns the authored text source.
    pub fn authored_src(&self) -> Option<&AuthoredHref> {
        self.authored_src.as_ref()
    }

    /// Parses the authored text source on demand.
    pub fn parsed_src(&self) -> Option<ParsedHref> {
        self.authored_src.clone().map(parse_href)
    }

    /// Returns the optional element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Returns the optional rendering region.
    pub fn region(&self) -> Option<&EpubString> {
        self.region.as_ref()
    }

    /// Returns the optional `epub:type` value.
    pub fn epub_type(&self) -> Option<&EpubString> {
        self.epub_type.as_ref()
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A SMIL audio target and its lexical clip clocks.
pub struct SmilAudio {
    authored_src: Option<AuthoredHref>,
    clip_begin: Option<String>,
    clip_end: Option<String>,
    id: Option<EpubString>,
    epub_type: Option<EpubString>,
}

#[bon::bon]
impl SmilAudio {
    /// Builds a normalized audio node with a required local or remote source href.
    #[builder]
    pub fn new(
        src: EpubHref,
        clip_begin: Option<String>,
        clip_end: Option<String>,
        id: Option<EpubString>,
        epub_type: Option<EpubString>,
    ) -> Result<Self> {
        let clip_begin = normalized_clock(CLIP_BEGIN, clip_begin)?;
        let clip_end = normalized_clock(CLIP_END, clip_end)?;
        if let (Some(begin), Some(end)) = (&clip_begin, &clip_end) {
            let begin_time = crate::media_overlay::parse_media_time(begin)
                .expect("normalized clipBegin is a recognized clock");
            let end_time = crate::media_overlay::parse_media_time(end)
                .expect("normalized clipEnd is a recognized clock");
            if begin_time.milliseconds() >= end_time.milliseconds() {
                return Err(SmilError::InvalidClipRange {
                    begin: begin.clone(),
                    end: end.clone(),
                });
            }
        }
        Ok(Self {
            authored_src: Some(AuthoredHref::from(&src)),
            clip_begin,
            clip_end,
            id,
            epub_type,
        })
    }

    /// Returns the authored audio source.
    pub fn authored_src(&self) -> Option<&AuthoredHref> {
        self.authored_src.as_ref()
    }

    /// Parses the authored audio source on demand.
    pub fn parsed_src(&self) -> Option<ParsedHref> {
        self.authored_src.clone().map(parse_href)
    }

    /// Returns the normalized lexical clip start.
    pub fn clip_begin(&self) -> Option<&str> {
        self.clip_begin.as_deref()
    }

    /// Returns the normalized lexical clip end.
    pub fn clip_end(&self) -> Option<&str> {
        self.clip_end.as_deref()
    }

    /// Returns the optional element ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Returns the optional `epub:type` value.
    pub fn epub_type(&self) -> Option<&EpubString> {
        self.epub_type.as_ref()
    }
}

fn normalized_clock(attribute: &'static str, value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| {
            let normalized = value.trim();
            crate::media_overlay::parse_media_time(normalized).ok_or_else(|| {
                SmilError::InvalidClock {
                    attribute,
                    value: value.clone(),
                }
            })?;
            Ok(normalized.to_string())
        })
        .transpose()
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A sequence or parallel timing child.
pub enum SmilSequenceChild {
    /// A nested sequential timing container.
    Seq(SmilSeq),
    /// A nested parallel timing container.
    Par(SmilPar),
}

impl From<SmilSeq> for SmilSequenceChild {
    fn from(value: SmilSeq) -> Self {
        Self::Seq(value)
    }
}

impl From<SmilPar> for SmilSequenceChild {
    fn from(value: SmilPar) -> Self {
        Self::Par(value)
    }
}

fn parse_smil<R: BufRead>(input: R) -> Result<SmilDocument> {
    parse_smil_with_limits(input, SmilParseLimits::default())
}

fn parse_smil_with_limits<R: BufRead>(input: R, limits: SmilParseLimits) -> Result<SmilDocument> {
    let mut parser = SmilParser::new(input, limits);
    parser.parse()
}

struct SmilParser<R: BufRead> {
    reader: NsReader<BufReader<XmlUtf8Reader<R>>>,
    limits: SmilParseLimits,
    nodes: usize,
    nesting: usize,
}

impl<R: BufRead> SmilParser<R> {
    fn new(input: R, limits: SmilParseLimits) -> Self {
        let mut reader = NsReader::from_reader(BufReader::new(XmlUtf8Reader::new(input)));
        reader.config_mut().trim_text(false);
        Self {
            reader,
            limits,
            nodes: 0,
            nesting: 0,
        }
    }

    fn parse(&mut self) -> Result<SmilDocument> {
        let mut smil = SmilDocument::empty_parsed();
        let mut buf = Vec::new();

        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    self.validate_root(&event)?;
                    self.parse_smil_attrs(&event, &mut smil);
                    self.parse_root_content(&mut smil)?;
                    self.finish_document()?;
                    return Ok(smil);
                }
                Event::Empty(event) => {
                    self.validate_root(&event)?;
                    self.parse_smil_attrs(&event, &mut smil);
                    self.finish_document()?;
                    return Ok(smil);
                }
                Event::Eof => return Err(SmilError::MissingRoot),
                _ => {}
            }
            buf.clear();
        }
    }

    fn parse_root_content(&mut self, smil: &mut SmilDocument) -> Result<()> {
        let mut head_seen = false;
        let mut body_seen = false;
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if self.is_smil_element(&event, HEAD.as_bytes()) {
                        if head_seen {
                            return Err(SmilError::DuplicateHead);
                        }
                        head_seen = true;
                        smil.head = self.parse_head_block()?;
                    } else if self.is_smil_element(&event, BODY.as_bytes()) {
                        if body_seen {
                            return Err(SmilError::DuplicateBody);
                        }
                        body_seen = true;
                        smil.body = self.parse_body_block()?;
                    } else {
                        self.skip_element(event.name().as_ref())?;
                    }
                }
                Event::Empty(event) => {
                    if self.is_smil_element(&event, HEAD.as_bytes()) {
                        if head_seen {
                            return Err(SmilError::DuplicateHead);
                        }
                        head_seen = true;
                    } else if self.is_smil_element(&event, BODY.as_bytes()) {
                        if body_seen {
                            return Err(SmilError::DuplicateBody);
                        }
                        body_seen = true;
                    }
                }
                Event::End(event) if self.is_smil_end(&event, SMIL.as_bytes()) => break,
                Event::Eof => return Err(unexpected_eof(SMIL.as_bytes())),
                _ => {}
            }
            buf.clear();
        }
        Ok(())
    }

    fn finish_document(&mut self) -> Result<()> {
        let mut buf = Vec::new();
        loop {
            match self.reader.read_event_into(&mut buf)? {
                Event::Eof => return Ok(()),
                Event::Text(text) if text.iter().all(u8::is_ascii_whitespace) => {}
                Event::Comment(_) | Event::PI(_) => {}
                _ => return Err(SmilError::TrailingContent),
            }
            buf.clear();
        }
    }

    fn read_event_into<'b>(&mut self, buf: &'b mut Vec<u8>) -> Result<Event<'b>> {
        let event = self.reader.read_event_into(buf)?;
        match &event {
            Event::Start(_) => {
                self.nodes = self
                    .nodes
                    .checked_add(1)
                    .ok_or(SmilError::NodeLimitExceeded {
                        limit: self.limits.max_nodes,
                    })?;
                if self.nodes > self.limits.max_nodes {
                    return Err(SmilError::NodeLimitExceeded {
                        limit: self.limits.max_nodes,
                    });
                }
                self.nesting =
                    self.nesting
                        .checked_add(1)
                        .ok_or(SmilError::NestingLimitExceeded {
                            limit: self.limits.max_nesting,
                        })?;
                if self.nesting > self.limits.max_nesting {
                    return Err(SmilError::NestingLimitExceeded {
                        limit: self.limits.max_nesting,
                    });
                }
            }
            Event::Empty(_) => {
                self.nodes = self
                    .nodes
                    .checked_add(1)
                    .ok_or(SmilError::NodeLimitExceeded {
                        limit: self.limits.max_nodes,
                    })?;
                if self.nodes > self.limits.max_nodes {
                    return Err(SmilError::NodeLimitExceeded {
                        limit: self.limits.max_nodes,
                    });
                }
                if self.nesting == usize::MAX || self.nesting + 1 > self.limits.max_nesting {
                    return Err(SmilError::NestingLimitExceeded {
                        limit: self.limits.max_nesting,
                    });
                }
            }
            Event::End(_) => self.nesting = self.nesting.saturating_sub(1),
            _ => {}
        }
        Ok(event)
    }

    fn skip_element(&mut self, end: &[u8]) -> Result<()> {
        let end = end.to_vec();
        let mut depth = 1usize;
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) if event.name().as_ref() == end => depth += 1,
                Event::End(event) if event.name().as_ref() == end => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Event::Eof => return Err(unexpected_eof(&end)),
                _ => {}
            }
            buf.clear();
        }
        Ok(())
    }

    fn validate_root(&mut self, event: &BytesStart<'_>) -> Result<()> {
        let found = String::from_utf8_lossy(local_name(event.name().as_ref())).into_owned();
        if found != SMIL {
            return Err(SmilError::WrongRoot { found });
        }
        let (resolved, _) = self.reader.resolver().resolve_element(event.name());
        let namespace = match resolved {
            ResolveResult::Bound(value) => {
                Some(String::from_utf8_lossy(value.as_ref()).into_owned())
            }
            ResolveResult::Unbound | ResolveResult::Unknown(_) => None,
        };
        if namespace.as_deref() != Some(SMIL_NS) {
            return Err(SmilError::WrongNamespace {
                expected: SMIL_NS,
                found: namespace,
            });
        }
        Ok(())
    }

    fn attrs(&mut self, event: &BytesStart<'_>) -> XmlAttrs {
        let attrs = XmlAttrs::from_event(event);
        let _ = attrs.invalid;
        attrs
    }

    fn is_smil_element(&self, event: &BytesStart<'_>, name: &[u8]) -> bool {
        let (resolved, local) = self.reader.resolver().resolve_element(event.name());
        matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == SMIL_NS.as_bytes())
            && local.as_ref() == name
    }

    fn is_smil_end(&self, event: &BytesEnd<'_>, name: &[u8]) -> bool {
        let (resolved, local) = self.reader.resolver().resolve_element(event.name());
        matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == SMIL_NS.as_bytes())
            && local.as_ref() == name
    }

    fn attr_value(
        &self,
        event: &BytesStart<'_>,
        namespace: Option<&[u8]>,
        name: &[u8],
    ) -> Option<String> {
        event
            .attributes()
            .with_checks(false)
            .filter_map(|attr| attr.ok())
            .find_map(|attr| {
                let (resolved, local) = self.reader.resolver().resolve_attribute(attr.key);
                let namespace_matches = match namespace {
                    Some(expected) => matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == expected),
                    None => matches!(resolved, ResolveResult::Unbound),
                };
                (namespace_matches && local.as_ref() == name)
                    .then(|| match attr.normalized_value(quick_xml::XmlVersion::default()) {
                        Ok(value) => Some(value.to_string()),
                        Err(_) => String::from_utf8(attr.value.into_owned())
                            .ok()
                            .filter(|value| {
                                value.contains("&#0;")
                                    || value.contains("&#x0;")
                                    || value.contains("&#X0;")
                            }),
                    })
                    .flatten()
            })
    }

    fn attr_value_trimmed(
        &self,
        event: &BytesStart<'_>,
        namespace: Option<&[u8]>,
        name: &[u8],
    ) -> Option<String> {
        normalize_optional(self.attr_value(event, namespace, name))
    }

    fn push_general_ref(
        &mut self,
        output: &mut String,
        reference: &quick_xml::events::BytesRef<'_>,
    ) -> Result<()> {
        let _ = push_general_ref(output, reference)?;
        Ok(())
    }

    fn parse_smil_attrs(&mut self, event: &BytesStart<'_>, smil: &mut SmilDocument) {
        self.attrs(event);
        smil.version =
            optional_epub_string(self.attr_value_trimmed(event, None, VERSION.as_bytes()));
        smil.xml_lang = optional_epub_string(self.attr_value_trimmed(
            event,
            Some(XML_NS.as_bytes()),
            LANG.as_bytes(),
        ));
        smil.id = optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes()));
    }

    fn parse_head_block(&mut self) -> Result<SmilHead> {
        let mut head = SmilHead::new();
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if self.is_smil_element(&event, METADATA.as_bytes()) {
                        head.metadata.extend(self.parse_metadata_block()?);
                    } else if self.is_smil_element(&event, META.as_bytes()) {
                        head.metadata.push(self.parse_meta_element(&event)?);
                    } else {
                        self.skip_element(event.name().as_ref())?;
                    }
                }
                Event::Empty(event) => {
                    if self.is_smil_element(&event, META.as_bytes()) {
                        head.metadata.push(self.parse_meta_empty(&event))
                    }
                }
                Event::End(event) if self.is_smil_end(&event, HEAD.as_bytes()) => break,
                Event::Eof => return Err(unexpected_eof(HEAD.as_bytes())),
                _ => {}
            }
            buf.clear();
        }
        Ok(head)
    }

    fn parse_metadata_block(&mut self) -> Result<Vec<SmilMeta>> {
        let mut metadata = Vec::new();
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if self.is_smil_element(&event, META.as_bytes()) {
                        metadata.push(self.parse_meta_element(&event)?);
                    } else {
                        self.skip_element(event.name().as_ref())?;
                    }
                }
                Event::Empty(event) if self.is_smil_element(&event, META.as_bytes()) => {
                    metadata.push(self.parse_meta_empty(&event))
                }
                Event::End(event) if self.is_smil_end(&event, METADATA.as_bytes()) => {
                    break;
                }
                Event::Eof => return Err(unexpected_eof(METADATA.as_bytes())),
                _ => {}
            }
            buf.clear();
        }
        Ok(metadata)
    }

    fn parse_meta_element(&mut self, event: &BytesStart<'_>) -> Result<SmilMeta> {
        let mut meta = self.parse_meta_empty(event);
        let content = self.read_text_content(META.as_bytes())?;
        if meta.content.is_none() {
            meta.content = optional_epub_string(normalize_optional(Some(content)));
        }
        Ok(meta)
    }

    fn parse_meta_empty(&mut self, event: &BytesStart<'_>) -> SmilMeta {
        self.attrs(event);
        SmilMeta {
            name: optional_epub_string(self.attr_value_trimmed(event, None, NAME.as_bytes())),
            content: optional_epub_string(self.attr_value_trimmed(event, None, CONTENT.as_bytes())),
            scheme: optional_epub_string(self.attr_value_trimmed(event, None, SCHEME.as_bytes())),
            xml_lang: optional_epub_string(self.attr_value_trimmed(
                event,
                Some(XML_NS.as_bytes()),
                LANG.as_bytes(),
            )),
            dir: self
                .attr_value_trimmed(event, None, DIR.as_bytes())
                .and_then(|value| TextDirection::from_str(&value).ok()),
            id: optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes())),
        }
    }

    fn parse_body_block(&mut self) -> Result<SmilBody> {
        let mut body = SmilBody::new();
        let mut sequences = Vec::<SmilSeq>::new();
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if self.is_smil_element(&event, SEQ.as_bytes()) {
                        sequences.push(self.parse_seq_empty(&event));
                    } else if self.is_smil_element(&event, PAR.as_bytes()) {
                        let child = SmilSequenceChild::Par(self.parse_par(&event)?);
                        append_sequence_child(&mut body.children, &mut sequences, child);
                    } else {
                        self.skip_element(event.name().as_ref())?;
                    }
                }
                Event::Empty(event) => {
                    if self.is_smil_element(&event, SEQ.as_bytes()) {
                        let child = SmilSequenceChild::Seq(self.parse_seq_empty(&event));
                        append_sequence_child(&mut body.children, &mut sequences, child);
                    } else if self.is_smil_element(&event, PAR.as_bytes()) {
                        let child = SmilSequenceChild::Par(self.parse_par_empty(&event));
                        append_sequence_child(&mut body.children, &mut sequences, child);
                    }
                }
                Event::End(event) if self.is_smil_end(&event, SEQ.as_bytes()) => {
                    let sequence = sequences
                        .pop()
                        .ok_or_else(|| unexpected_eof(SEQ.as_bytes()))?;
                    append_sequence_child(
                        &mut body.children,
                        &mut sequences,
                        SmilSequenceChild::Seq(sequence),
                    );
                }
                Event::End(event) if self.is_smil_end(&event, BODY.as_bytes()) => {
                    if !sequences.is_empty() {
                        return Err(unexpected_eof(SEQ.as_bytes()));
                    }
                    break;
                }
                Event::Eof => return Err(unexpected_eof(BODY.as_bytes())),
                _ => {}
            }
            buf.clear();
        }
        Ok(body)
    }

    fn parse_seq_empty(&mut self, event: &BytesStart<'_>) -> SmilSeq {
        self.attrs(event);
        let authored_textref = self
            .attr_value(event, Some(EPUB_NS.as_bytes()), TEXTREF.as_bytes())
            .map(AuthoredHref::new);
        SmilSeq {
            children: Vec::new(),
            epub_type: optional_epub_string(self.attr_value_trimmed(
                event,
                Some(EPUB_NS.as_bytes()),
                TYPE.as_bytes(),
            )),
            id: optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes())),
            class: optional_epub_string(self.attr_value_trimmed(event, None, CLASS.as_bytes())),
            custom_test: optional_epub_string(self.attr_value_trimmed(
                event,
                None,
                CUSTOM_TEST.as_bytes(),
            )),
            authored_textref,
        }
    }

    fn parse_par(&mut self, event: &BytesStart<'_>) -> Result<SmilPar> {
        let mut par = self.parse_par_empty(event);
        let mut buf = Vec::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if self.is_smil_element(&event, TEXT.as_bytes()) {
                        if par.text().is_some() {
                            return Err(SmilError::RepeatedText);
                        }
                        par.children
                            .push(SmilParallelChild::Text(self.parse_text(&event)));
                        self.skip_element(event.name().as_ref())?;
                    } else if self.is_smil_element(&event, AUDIO.as_bytes()) {
                        if par.audio().is_some() {
                            return Err(SmilError::RepeatedAudio);
                        }
                        par.children
                            .push(SmilParallelChild::Audio(self.parse_audio(&event)));
                        self.skip_element(event.name().as_ref())?;
                    } else {
                        self.skip_element(event.name().as_ref())?;
                    }
                }
                Event::Empty(event) => {
                    if self.is_smil_element(&event, TEXT.as_bytes()) {
                        if par.text().is_some() {
                            return Err(SmilError::RepeatedText);
                        }
                        par.children
                            .push(SmilParallelChild::Text(self.parse_text(&event)));
                    } else if self.is_smil_element(&event, AUDIO.as_bytes()) {
                        if par.audio().is_some() {
                            return Err(SmilError::RepeatedAudio);
                        }
                        par.children
                            .push(SmilParallelChild::Audio(self.parse_audio(&event)));
                    }
                }
                Event::End(event) if self.is_smil_end(&event, PAR.as_bytes()) => break,
                Event::Eof => return Err(unexpected_eof(PAR.as_bytes())),
                _ => {}
            }
            buf.clear();
        }
        Ok(par)
    }

    fn parse_par_empty(&mut self, event: &BytesStart<'_>) -> SmilPar {
        self.attrs(event);
        let authored_textref = self
            .attr_value(event, Some(EPUB_NS.as_bytes()), TEXTREF.as_bytes())
            .map(AuthoredHref::new);
        SmilPar {
            children: Vec::new(),
            epub_type: optional_epub_string(self.attr_value_trimmed(
                event,
                Some(EPUB_NS.as_bytes()),
                TYPE.as_bytes(),
            )),
            id: optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes())),
            class: optional_epub_string(self.attr_value_trimmed(event, None, CLASS.as_bytes())),
            custom_test: optional_epub_string(self.attr_value_trimmed(
                event,
                None,
                CUSTOM_TEST.as_bytes(),
            )),
            authored_textref,
        }
    }

    fn parse_text(&mut self, event: &BytesStart<'_>) -> SmilText {
        self.attrs(event);
        let authored_src = self
            .attr_value(event, None, SRC.as_bytes())
            .map(AuthoredHref::new);
        SmilText {
            authored_src,
            id: optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes())),
            region: optional_epub_string(self.attr_value_trimmed(event, None, REGION.as_bytes())),
            epub_type: optional_epub_string(self.attr_value_trimmed(
                event,
                Some(EPUB_NS.as_bytes()),
                TYPE.as_bytes(),
            )),
        }
    }

    fn parse_audio(&mut self, event: &BytesStart<'_>) -> SmilAudio {
        self.attrs(event);
        let authored_src = self
            .attr_value(event, None, SRC.as_bytes())
            .map(AuthoredHref::new);
        SmilAudio {
            authored_src,
            clip_begin: self.attr_value(event, None, CLIP_BEGIN.as_bytes()),
            clip_end: self.attr_value(event, None, CLIP_END.as_bytes()),
            id: optional_epub_string(self.attr_value_trimmed(event, None, ID.as_bytes())),
            epub_type: optional_epub_string(self.attr_value_trimmed(
                event,
                Some(EPUB_NS.as_bytes()),
                TYPE.as_bytes(),
            )),
        }
    }

    fn read_text_content(&mut self, end: &[u8]) -> Result<String> {
        let mut buf = Vec::new();
        let mut output = String::new();
        loop {
            match self.read_event_into(&mut buf)? {
                Event::Text(event) => {
                    output.push_str(&text_content(&event)?);
                }
                Event::CData(event) => {
                    output.push_str(&cdata_content(&event)?);
                }
                Event::GeneralRef(reference) => {
                    self.push_general_ref(&mut output, &reference)?;
                }
                Event::End(end_event) if self.is_smil_end(&end_event, end) => break,
                Event::Eof => return Err(unexpected_eof(end)),
                _ => {}
            }
            buf.clear();
        }
        Ok(output)
    }
}

fn append_sequence_child(
    roots: &mut Vec<SmilSequenceChild>,
    sequences: &mut [SmilSeq],
    child: SmilSequenceChild,
) {
    if let Some(parent) = sequences.last_mut() {
        parent.children.push(child);
    } else {
        roots.push(child);
    }
}

fn unexpected_eof(expected: &[u8]) -> SmilError {
    SmilError::UnexpectedEof {
        expected: String::from_utf8_lossy(expected).into_owned(),
    }
}

fn push_escaped_attribute(node: &mut BytesStart<'_>, name: &str, value: &str) {
    node.push_attribute((name.as_bytes(), escape(value).as_bytes()));
}

fn write_head(writer: &mut Writer<Vec<u8>>, head: &SmilHead) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new(HEAD)))?;
    if !head.metadata.is_empty() {
        writer.write_event(Event::Start(BytesStart::new(METADATA)))?;
        head.metadata
            .iter()
            .try_for_each(|meta| write_meta(writer, meta))?;
        writer.write_event(Event::End(BytesEnd::new(METADATA)))?;
    }
    writer.write_event(Event::End(BytesEnd::new(HEAD)))?;
    Ok(())
}

fn write_meta(writer: &mut Writer<Vec<u8>>, meta: &SmilMeta) -> Result<()> {
    let mut node = BytesStart::new(META);
    if let Some(name) = meta.name.as_deref() {
        push_escaped_attribute(&mut node, NAME, name);
    }
    if let Some(content) = meta.content.as_deref() {
        push_escaped_attribute(&mut node, CONTENT, content);
    }
    if let Some(scheme) = meta.scheme.as_deref() {
        push_escaped_attribute(&mut node, SCHEME, scheme);
    }
    if let Some(xml_lang) = meta.xml_lang.as_deref() {
        push_escaped_attribute(&mut node, "xml:lang", xml_lang);
    }
    if let Some(dir) = meta.dir {
        push_escaped_attribute(&mut node, DIR, &dir.to_string());
    }
    if let Some(id) = meta.id.as_deref() {
        push_escaped_attribute(&mut node, ID, id);
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_body(writer: &mut Writer<Vec<u8>>, body: &SmilBody) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new(BODY)))?;
    for child in &body.children {
        write_sequence_child(writer, child)?;
    }
    writer.write_event(Event::End(BytesEnd::new(BODY)))?;
    Ok(())
}

fn write_sequence_child(writer: &mut Writer<Vec<u8>>, child: &SmilSequenceChild) -> Result<()> {
    match child {
        SmilSequenceChild::Seq(seq) => write_seq(writer, seq),
        SmilSequenceChild::Par(par) => write_par(writer, par),
    }
}

fn write_seq(writer: &mut Writer<Vec<u8>>, seq: &SmilSeq) -> Result<()> {
    let mut node = BytesStart::new(SEQ);
    if let Some(id) = seq.id.as_deref() {
        push_escaped_attribute(&mut node, ID, id);
    }
    if let Some(epub_type) = seq.epub_type.as_deref() {
        push_escaped_attribute(&mut node, "epub:type", epub_type);
    }
    if let Some(class) = seq.class.as_deref() {
        push_escaped_attribute(&mut node, CLASS, class);
    }
    if let Some(custom_test) = seq.custom_test.as_deref() {
        push_escaped_attribute(&mut node, CUSTOM_TEST, custom_test);
    }
    if let Some(textref) = seq.authored_textref.as_ref() {
        push_escaped_attribute(&mut node, "epub:textref", textref.as_str());
    }
    writer.write_event(Event::Start(node))?;
    for child in &seq.children {
        write_sequence_child(writer, child)?;
    }
    writer.write_event(Event::End(BytesEnd::new(SEQ)))?;
    Ok(())
}

fn write_par(writer: &mut Writer<Vec<u8>>, par: &SmilPar) -> Result<()> {
    let mut node = BytesStart::new(PAR);
    if let Some(id) = par.id.as_deref() {
        push_escaped_attribute(&mut node, ID, id);
    }
    if let Some(epub_type) = par.epub_type.as_deref() {
        push_escaped_attribute(&mut node, "epub:type", epub_type);
    }
    if let Some(class) = par.class.as_deref() {
        push_escaped_attribute(&mut node, CLASS, class);
    }
    if let Some(custom_test) = par.custom_test.as_deref() {
        push_escaped_attribute(&mut node, CUSTOM_TEST, custom_test);
    }
    if let Some(textref) = par.authored_textref.as_ref() {
        push_escaped_attribute(&mut node, "epub:textref", textref.as_str());
    }
    writer.write_event(Event::Start(node))?;
    for child in &par.children {
        match child {
            SmilParallelChild::Text(text) => write_text(writer, text)?,
            SmilParallelChild::Audio(audio) => write_audio(writer, audio)?,
        }
    }
    writer.write_event(Event::End(BytesEnd::new(PAR)))?;
    Ok(())
}

fn write_text(writer: &mut Writer<Vec<u8>>, text: &SmilText) -> Result<()> {
    let mut node = BytesStart::new(TEXT);
    if let Some(src) = text.authored_src.as_ref() {
        push_escaped_attribute(&mut node, SRC, src.as_str());
    }
    if let Some(id) = text.id.as_deref() {
        push_escaped_attribute(&mut node, ID, id);
    }
    if let Some(region) = text.region.as_deref() {
        push_escaped_attribute(&mut node, REGION, region);
    }
    if let Some(epub_type) = text.epub_type.as_deref() {
        push_escaped_attribute(&mut node, "epub:type", epub_type);
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_audio(writer: &mut Writer<Vec<u8>>, audio: &SmilAudio) -> Result<()> {
    let mut node = BytesStart::new(AUDIO);
    if let Some(src) = audio.authored_src.as_ref() {
        push_escaped_attribute(&mut node, SRC, src.as_str());
    }
    if let Some(clip_begin) = audio.clip_begin.as_deref() {
        push_escaped_attribute(&mut node, CLIP_BEGIN, clip_begin);
    }
    if let Some(clip_end) = audio.clip_end.as_deref() {
        push_escaped_attribute(&mut node, CLIP_END, clip_end);
    }
    if let Some(id) = audio.id.as_deref() {
        push_escaped_attribute(&mut node, ID, id);
    }
    if let Some(epub_type) = audio.epub_type.as_deref() {
        push_escaped_attribute(&mut node, "epub:type", epub_type);
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct SmilExtraction {
    pub(crate) facts: SmilFacts,
    pub(crate) references: Vec<SmilPendingReference>,
}

#[derive(Debug)]
pub(crate) struct SmilPendingReference {
    pub(crate) node: SmilNodeId,
    pub(crate) authored: AuthoredHref,
    pub(crate) kind: HrefRole,
    pub(crate) element: &'static str,
    pub(crate) attribute: &'static str,
}

pub(crate) fn extract_smil_facts_from_reader<R: BufRead>(input: R) -> Result<SmilExtraction> {
    parse_smil_with_limits(input, SmilParseLimits::unbounded())
        .map(SmilDocument::into_analysis_parts)
}

#[cfg(test)]
fn extract_smil_facts(xml: &str) -> Result<SmilExtraction> {
    extract_smil_facts_from_reader(xml.as_bytes())
}

impl SmilDocument {
    pub(crate) fn into_analysis_parts(self) -> SmilExtraction {
        let SmilDocument { head, body, .. } = self;
        let mut skippable = Vec::new();
        let mut escapable = Vec::new();
        for meta in head.metadata {
            collect_analysis_meta(
                meta.name.as_deref(),
                meta.content.as_deref(),
                &mut skippable,
                &mut escapable,
            );
        }

        let mut projection = SmilProjection::default();
        projection.push_sequence_children(body.children);
        SmilExtraction {
            facts: SmilFacts::new(projection.roots, projection.nodes, skippable, escapable),
            references: projection.references,
        }
    }
}

#[derive(Default)]
struct SmilProjection {
    roots: Vec<SmilNodeId>,
    nodes: Vec<SmilNodeFact>,
    references: Vec<SmilPendingReference>,
}

impl SmilProjection {
    fn push_sequence_children(&mut self, children: Vec<SmilSequenceChild>) {
        let mut pending = children
            .into_iter()
            .rev()
            .map(|child| (child, None))
            .collect::<Vec<_>>();
        while let Some((child, parent)) = pending.pop() {
            match child {
                SmilSequenceChild::Seq(seq) => {
                    let node = self.push_container(
                        SmilNodeFact::Sequence {
                            children: Vec::new(),
                            epub_types: epub_types(seq.epub_type),
                        },
                        parent,
                    );
                    self.push_reference(
                        node,
                        seq.authored_textref,
                        HrefRole::SmilText,
                        SEQ,
                        "epub:textref",
                    );
                    pending.extend(
                        seq.children
                            .into_iter()
                            .rev()
                            .map(|child| (child, Some(node))),
                    );
                }
                SmilSequenceChild::Par(par) => {
                    let node = self.push_container(
                        SmilNodeFact::Parallel {
                            children: Vec::new(),
                            epub_types: epub_types(par.epub_type),
                        },
                        parent,
                    );
                    self.push_reference(
                        node,
                        par.authored_textref,
                        HrefRole::SmilText,
                        PAR,
                        "epub:textref",
                    );
                    for child in par.children {
                        self.push_parallel_child(child, node);
                    }
                }
            }
        }
    }

    fn push_parallel_child(&mut self, child: SmilParallelChild, parent: SmilNodeId) {
        let (fact, authored, kind, element) = match child {
            SmilParallelChild::Text(text) => (
                SmilNodeFact::Text {
                    epub_types: epub_types(text.epub_type),
                },
                text.authored_src,
                HrefRole::SmilText,
                TEXT,
            ),
            SmilParallelChild::Audio(audio) => (
                SmilNodeFact::Audio {
                    clip_begin: analysis_time(audio.clip_begin),
                    clip_end: analysis_time(audio.clip_end),
                    epub_types: epub_types(audio.epub_type),
                },
                audio.authored_src,
                HrefRole::SmilAudio,
                AUDIO,
            ),
        };
        let node = SmilNodeId::new(self.nodes.len());
        self.nodes.push(fact);
        append_child(&mut self.nodes, parent, node);
        self.push_reference(node, authored, kind, element, SRC);
    }

    fn push_container(&mut self, fact: SmilNodeFact, parent: Option<SmilNodeId>) -> SmilNodeId {
        let node = SmilNodeId::new(self.nodes.len());
        self.nodes.push(fact);
        if let Some(parent) = parent {
            append_child(&mut self.nodes, parent, node);
        } else {
            self.roots.push(node);
        }
        node
    }

    fn push_reference(
        &mut self,
        node: SmilNodeId,
        authored: Option<AuthoredHref>,
        kind: HrefRole,
        element: &'static str,
        attribute: &'static str,
    ) {
        if let Some(authored) = authored {
            self.references.push(SmilPendingReference {
                node,
                authored,
                kind,
                element,
                attribute,
            });
        }
    }
}

fn append_child(nodes: &mut [SmilNodeFact], parent: SmilNodeId, child: SmilNodeId) {
    match &mut nodes[parent.slot()] {
        SmilNodeFact::Sequence { children, .. } | SmilNodeFact::Parallel { children, .. } => {
            children.push(child)
        }
        SmilNodeFact::Text { .. } | SmilNodeFact::Audio { .. } => unreachable!(),
    }
}

fn epub_types(value: Option<EpubString>) -> Vec<String> {
    value.as_deref().map(split_tokens).unwrap_or_default()
}

fn analysis_time(value: Option<String>) -> Option<SmilTime> {
    value.map(|value| {
        crate::media_overlay::parse_media_time(&value)
            .map(SmilTime::Parsed)
            .unwrap_or(SmilTime::Unrecognized)
    })
}

fn collect_analysis_meta(
    name: Option<&str>,
    content: Option<&str>,
    skippable: &mut Vec<String>,
    escapable: &mut Vec<String>,
) {
    let Some((name, content)) = name.zip(content) else {
        return;
    };
    if name.eq_ignore_ascii_case("skippable") {
        skippable.extend(split_tokens(content));
    } else if name.eq_ignore_ascii_case("escapable") {
        escapable.extend(split_tokens(content));
    }
}

fn split_tokens(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = include_str!("../tests/fixtures/smil_basic.smil");
    const NESTED: &str = include_str!("../tests/fixtures/smil_nested.smil");

    #[test]
    fn parse_basic() {
        let smil = SmilDocument::parse(BASIC).unwrap();
        assert_eq!(smil.version().map(EpubString::as_str), Some("3.0"));
        assert_eq!(smil.head().metadata().len(), 1);
        let children = smil.body().children();
        assert_eq!(children.len(), 1);
        match &children[0] {
            SmilSequenceChild::Seq(seq) => {
                assert_eq!(seq.children().len(), 1);
                match &seq.children()[0] {
                    SmilSequenceChild::Par(par) => {
                        assert!(par.text().is_some());
                        assert!(par.audio().is_some());
                    }
                    _ => panic!("expected par"),
                }
            }
            _ => panic!("expected seq"),
        }
    }

    #[test]
    fn audio_before_text_order_survives_parse_and_serialization() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><audio src="audio.mp3"/><text src="chapter.xhtml#p1"/></par></body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        let SmilSequenceChild::Par(par) = &document.body().children()[0] else {
            panic!("expected par");
        };

        assert!(matches!(par.children()[0], SmilParallelChild::Audio(_)));
        assert!(matches!(par.children()[1], SmilParallelChild::Text(_)));

        let serialized = document.to_string().unwrap();
        assert!(serialized.find("<audio").unwrap() < serialized.find("<text").unwrap());
    }

    #[test]
    fn unsupported_parallel_containers_do_not_enter_the_model() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><seq><par/></seq><par/></par></body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        let SmilSequenceChild::Par(par) = &document.body().children()[0] else {
            panic!("expected par");
        };

        assert!(par.children().is_empty());
    }

    #[test]
    fn document_projects_to_preorder_analysis_facts() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops">
<head><meta name="skippable" content="note sidebar">ignored</meta><meta name="ESCAPABLE">table list</meta></head>
<body><seq epub:type="chapter section" epub:textref="chapter.xhtml"><par epub:type="sentence" epub:textref="chapter.xhtml#p1"><audio epub:type="sound" src="audio.mp3" clipBegin="1.25s" clipEnd="bad"/><text epub:type="pagebreak" src="chapter.xhtml#p1"/></par></seq></body>
</smil>"#;
        let extraction = SmilDocument::parse(xml).unwrap().into_analysis_parts();
        let facts = &extraction.facts;

        assert_eq!(facts.roots(), &[SmilNodeId::new(0)]);
        assert_eq!(facts.skippable(), ["note", "sidebar"]);
        assert_eq!(facts.escapable(), ["table", "list"]);
        assert_eq!(facts.nodes().len(), 4);
        assert!(matches!(
            &facts.nodes()[0],
            SmilNodeFact::Sequence { children, epub_types, .. }
                if children == &[SmilNodeId::new(1)] && epub_types == &["chapter", "section"]
        ));
        assert!(matches!(
            &facts.nodes()[1],
            SmilNodeFact::Parallel { children, epub_types, .. }
                if children == &[SmilNodeId::new(2), SmilNodeId::new(3)]
                    && epub_types == &["sentence"]
        ));
        assert!(
            matches!(&facts.nodes()[2], SmilNodeFact::Audio { epub_types, .. } if epub_types == &["sound"])
        );
        assert_eq!(
            facts.nodes()[2]
                .clip_begin()
                .unwrap()
                .parsed()
                .unwrap()
                .milliseconds(),
            1_250
        );
        assert_eq!(facts.nodes()[2].clip_end(), Some(&SmilTime::Unrecognized));
        assert!(
            matches!(&facts.nodes()[3], SmilNodeFact::Text { epub_types, .. } if epub_types == &["pagebreak"])
        );
        assert_eq!(extraction.references.len(), 4);
        assert_eq!(extraction.references[0].node, SmilNodeId::new(0));
        assert_eq!(extraction.references[1].node, SmilNodeId::new(1));
        assert_eq!(extraction.references[2].node, SmilNodeId::new(2));
        assert_eq!(extraction.references[3].node, SmilNodeId::new(3));
    }

    #[test]
    fn parse_nested() {
        let smil = SmilDocument::parse(NESTED).unwrap();
        let children = smil.body().children();
        assert_eq!(children.len(), 1);
        match &children[0] {
            SmilSequenceChild::Seq(seq) => {
                assert_eq!(seq.children().len(), 2);
            }
            _ => panic!("expected seq"),
        }
    }

    #[test]
    fn parse_meta_text_entities() {
        let smil = r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL">
<head><metadata><meta name="title">A &amp; B</meta></metadata></head><body></body>
</smil>"#;

        let smil = SmilDocument::parse(smil).unwrap();

        assert_eq!(
            smil.head().metadata()[0].content().map(EpubString::as_str),
            Some("A & B")
        );
    }

    #[test]
    fn unknown_meta_entities_are_preserved() {
        let smil = r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL">
<head><metadata><meta name="title">A &unknown; B</meta></metadata></head><body></body>
</smil>"#;

        let document = parse_smil(smil.as_bytes()).unwrap();

        assert_eq!(
            document.head().metadata()[0]
                .content()
                .map(EpubString::as_str),
            Some("A &unknown; B")
        );
    }

    #[test]
    fn malformed_smil_attrs_are_omitted_while_valid_attrs_are_preserved() {
        let smil = r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL">
<head><metadata><meta bad="one" bad="two" name="title" content="A" /></metadata></head><body></body>
</smil>"#;

        let document = parse_smil(smil.as_bytes()).unwrap();

        let meta = &document.head().metadata()[0];
        assert_eq!(meta.name().map(EpubString::as_str), Some("title"));
        assert_eq!(meta.content().map(EpubString::as_str), Some("A"));
        let serialized = document.to_string().unwrap();
        assert!(!serialized.contains("bad="));
        assert!(serialized.contains("name=\"title\""));
    }

    #[test]
    fn smil_root_and_namespace_are_required() {
        assert!(matches!(
            SmilDocument::parse(""),
            Err(SmilError::MissingRoot)
        ));
        assert!(matches!(
            SmilDocument::parse("<body xmlns=\"http://www.w3.org/ns/SMIL\"/>"),
            Err(SmilError::WrongRoot { .. })
        ));
        assert!(matches!(
            SmilDocument::parse("<smil/>"),
            Err(SmilError::WrongNamespace { .. })
        ));
        assert!(
            SmilDocument::parse("<s:smil xmlns:s=\"http://www.w3.org/ns/SMIL\"><s:body/></s:smil>")
                .is_ok()
        );
    }

    #[test]
    fn duplicate_structural_children_are_focused_errors() {
        let duplicate_head = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><head/><head/></smil>"#;
        assert!(matches!(
            SmilDocument::parse(duplicate_head),
            Err(SmilError::DuplicateHead)
        ));
        let duplicate_body = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body/><body/></smil>"#;
        assert!(matches!(
            SmilDocument::parse(duplicate_body),
            Err(SmilError::DuplicateBody)
        ));

        let repeated_text = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><text/><text/></par></body></smil>"#;
        assert!(matches!(
            SmilDocument::parse(repeated_text),
            Err(SmilError::RepeatedText)
        ));
        let repeated_audio = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><audio/><audio/></par></body></smil>"#;
        assert!(matches!(
            SmilDocument::parse(repeated_audio),
            Err(SmilError::RepeatedAudio)
        ));
    }

    #[test]
    fn smil_requires_a_complete_single_document() {
        assert!(matches!(
            SmilDocument::parse(r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq>"#),
            Err(SmilError::UnexpectedEof { .. })
        ));
        assert!(matches!(
            SmilDocument::parse(
                r#"<smil xmlns="http://www.w3.org/ns/SMIL"/><smil xmlns="http://www.w3.org/ns/SMIL"/>"#
            ),
            Err(SmilError::TrailingContent)
        ));
    }

    #[test]
    fn smil_parser_enforces_node_and_nesting_limits() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><head/><body/></smil>"#;
        assert!(matches!(
            SmilDocument::parse_with_limits(xml.as_bytes(), SmilParseLimits::new(2, 8)),
            Err(SmilError::NodeLimitExceeded { limit: 2 })
        ));
        assert!(matches!(
            SmilDocument::parse_with_limits(xml.as_bytes(), SmilParseLimits::new(8, 1)),
            Err(SmilError::NestingLimitExceeded { limit: 1 })
        ));
    }

    #[test]
    fn analysis_projection_does_not_inherit_standalone_structural_limits() {
        let mut deep = String::from(r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body>"#);
        for _ in 0..257 {
            deep.push_str("<seq>");
        }
        deep.push_str("<par><text/></par>");
        for _ in 0..257 {
            deep.push_str("</seq>");
        }
        deep.push_str("</body></smil>");
        assert!(matches!(
            SmilDocument::parse(&deep),
            Err(SmilError::NestingLimitExceeded { .. })
        ));
        assert_eq!(extract_smil_facts(&deep).unwrap().facts.nodes().len(), 259);

        let mut broad = String::from(r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body>"#);
        for _ in 0..100_000 {
            broad.push_str("<par/>");
        }
        broad.push_str("</body></smil>");
        assert!(matches!(
            SmilDocument::parse(&broad),
            Err(SmilError::NodeLimitExceeded { .. })
        ));
        assert_eq!(
            extract_smil_facts(&broad).unwrap().facts.nodes().len(),
            100_000
        );
    }

    #[test]
    fn analysis_extraction_streams_utf16_smil() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><smil xmlns="http://www.w3.org/ns/SMIL"><body><par/></body></smil>"#;
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));

        let extraction = extract_smil_facts_from_reader(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(extraction.facts.roots().len(), 1);
    }

    #[test]
    fn timing_projection_retains_semantic_state_without_lexical_values() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><par><audio clipBegin=" 1.25s " clipEnd=" "/></par></body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        let SmilSequenceChild::Par(par) = &document.body().children()[0] else {
            panic!("expected par");
        };
        assert_eq!(par.audio().unwrap().clip_begin(), Some(" 1.25s "));
        assert_eq!(par.audio().unwrap().clip_end(), Some(" "));
        let serialized = document.to_string().unwrap();
        assert!(serialized.contains("clipBegin=\" 1.25s \""));
        assert!(serialized.contains("clipEnd=\" \""));

        let extraction = document.into_analysis_parts();
        let audio = &extraction.facts.nodes()[1];
        assert_eq!(
            audio.clip_begin().and_then(|value| value.parsed()),
            Some(crate::media_overlay::MediaTime::new(1_250))
        );
        assert_eq!(audio.clip_end(), Some(&SmilTime::Unrecognized));
    }

    #[test]
    fn authored_smil_references_survive_empty_and_whitespace_normalization() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops"><body><seq epub:textref="  chapter.xhtml#one  "><par epub:textref=""><text src=""/><audio src="  audio.mp3  "/></par></seq></body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        let SmilSequenceChild::Seq(seq) = &document.body().children()[0] else {
            panic!("expected seq");
        };
        assert_eq!(
            seq.authored_textref().map(AuthoredHref::as_str),
            Some("  chapter.xhtml#one  ")
        );
        let SmilSequenceChild::Par(par) = &seq.children()[0] else {
            panic!("expected par");
        };
        assert_eq!(par.authored_textref().map(AuthoredHref::as_str), Some(""));
        assert_eq!(
            par.text().unwrap().authored_src().map(AuthoredHref::as_str),
            Some("")
        );
        assert_eq!(
            par.audio()
                .unwrap()
                .authored_src()
                .map(AuthoredHref::as_str),
            Some("  audio.mp3  ")
        );
        assert!(matches!(
            par.audio().unwrap().parsed_src(),
            Some(ParsedHref::Invalid { .. })
        ));

        let serialized = document.to_string().unwrap();
        assert!(serialized.contains("epub:textref=\"\""));
        assert!(serialized.contains("src=\"\""));
        assert!(serialized.contains("src=\"  audio.mp3  \""));
    }

    fn assert_reference_state(
        authored: Option<&AuthoredHref>,
        parsed: Option<ParsedHref>,
        raw: Option<&str>,
        expected: &str,
    ) {
        assert_eq!(authored.map(AuthoredHref::as_str), raw);
        match expected {
            "missing" => assert!(parsed.is_none()),
            "empty" => assert!(matches!(parsed, Some(ParsedHref::Empty { .. }))),
            "whitespace" | "invalid" => {
                assert!(matches!(parsed, Some(ParsedHref::Invalid { .. })))
            }
            "local" | "malformed" => {
                assert!(matches!(parsed, Some(ParsedHref::Local { .. })))
            }
            "external" => assert!(matches!(parsed, Some(ParsedHref::Remote { .. }))),
            _ => unreachable!(),
        }
    }

    #[test]
    fn smil_references_preserve_raw_and_canonical_interpretation_matrix() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<seq><par><text/><audio/></par></seq>
<seq epub:textref=""><par epub:textref=""><text src=""/><audio src=""/></par></seq>
<seq epub:textref="   "><par epub:textref="   "><text src="   "/><audio src="   "/></par></seq>
<seq epub:textref="chapter.xhtml#one"><par epub:textref="chapter.xhtml#one"><text src="chapter.xhtml#one"/><audio src="audio.mp3"/></par></seq>
<seq epub:textref="https://example.com/chapter"><par epub:textref="https://example.com/chapter"><text src="https://example.com/chapter"/><audio src="https://example.com/audio.mp3"/></par></seq>
<seq epub:textref="bad&#0;target"><par epub:textref="bad&#0;target"><text src="bad&#0;target"/><audio src="bad&#0;target"/></par></seq>
</body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        let cases = [
            (None, "missing"),
            (Some(""), "empty"),
            (Some("   "), "whitespace"),
            (Some("chapter.xhtml#one"), "local"),
            (Some("https://example.com/chapter"), "external"),
            (Some("bad&#0;target"), "malformed"),
        ];

        for (child, (raw, expected)) in document.body().children().iter().zip(cases) {
            let SmilSequenceChild::Seq(seq) = child else {
                panic!("expected seq");
            };
            assert_reference_state(seq.authored_textref(), seq.parsed_textref(), raw, expected);
            let SmilSequenceChild::Par(par) = &seq.children()[0] else {
                panic!("expected par");
            };
            assert_reference_state(par.authored_textref(), par.parsed_textref(), raw, expected);
            assert_reference_state(
                par.text().unwrap().authored_src(),
                par.text().unwrap().parsed_src(),
                raw,
                expected,
            );
            let audio_raw = match expected {
                "local" => Some("audio.mp3"),
                "external" => Some("https://example.com/audio.mp3"),
                _ => raw,
            };
            assert_reference_state(
                par.audio().unwrap().authored_src(),
                par.audio().unwrap().parsed_src(),
                audio_raw,
                expected,
            );
        }
    }

    #[test]
    fn foreign_smil_children_and_attributes_do_not_masquerade_as_smil() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops" xmlns:f="urn:foreign">
<f:head/><head><f:meta name="foreign"/><meta f:name="wrong" name="native"/></head>
<f:body/><body><f:seq/><seq f:textref="wrong" epub:textref="right.xhtml"><f:par/><par><f:text src="wrong"/><text f:src="wrong" src="right.xhtml"/><f:audio src="wrong"/><audio src="right.mp3"/></par></seq></body>
</smil>"#;
        let document = SmilDocument::parse(xml).unwrap();

        assert_eq!(document.head().metadata().len(), 1);
        assert_eq!(
            document.head().metadata()[0].name().map(EpubString::as_str),
            Some("native")
        );
        let SmilSequenceChild::Seq(seq) = &document.body().children()[0] else {
            panic!("expected seq");
        };
        assert_eq!(seq.children().len(), 1);
        assert_eq!(
            seq.authored_textref().map(AuthoredHref::as_str),
            Some("right.xhtml")
        );
        let SmilSequenceChild::Par(par) = &seq.children()[0] else {
            panic!("expected par");
        };
        assert_eq!(
            par.text().unwrap().authored_src().map(AuthoredHref::as_str),
            Some("right.xhtml")
        );
        assert_eq!(
            par.audio()
                .unwrap()
                .authored_src()
                .map(AuthoredHref::as_str),
            Some("right.mp3")
        );
    }

    #[test]
    fn meta_cdata_and_nested_text_have_an_explicit_private_contract() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL"><head><meta name="title"> A <![CDATA[& B]]><span> C </span></meta></head></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();
        assert_eq!(
            document.head().metadata()[0]
                .content()
                .map(EpubString::as_str),
            Some("A & B C")
        );
    }

    #[test]
    fn invalid_meta_direction_is_private_normalized_loss() {
        let document = SmilDocument::parse(
            r#"<smil xmlns="http://www.w3.org/ns/SMIL"><head><meta name="title" content="T" dir="sideways"/></head><body/></smil>"#,
        )
        .unwrap();

        assert_eq!(document.head().metadata()[0].dir(), None);
        assert!(!document.to_string().unwrap().contains("sideways"));
    }

    #[test]
    fn serialization_escapes_attributes_and_preserves_malformed_null_references() {
        let xml = r#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops"><head><meta name="A &amp; &quot;B&quot;"/></head><body><par epub:textref="chapter.xhtml?a=1&amp;b=2"><audio src="bad&#0;target"/></par></body></smil>"#;
        let document = SmilDocument::parse(xml).unwrap();

        let serialized = document.to_string().unwrap();
        assert!(serialized.contains(r#"name="A &amp; &quot;B&quot;""#));
        assert!(serialized.contains(r#"epub:textref="chapter.xhtml?a=1&amp;b=2""#));
        assert!(serialized.contains(r#"src="bad&amp;#0;target""#));
        assert!(!serialized.contains('\0'));

        let reparsed = SmilDocument::parse(&serialized).unwrap();
        assert_eq!(document, reparsed);
    }

    #[test]
    fn roundtrip() {
        let smil = SmilDocument::parse(BASIC).unwrap();
        let xml = smil.to_string().unwrap();
        let parsed = SmilDocument::parse(&xml).unwrap();
        assert_eq!(smil, parsed);
    }
}
