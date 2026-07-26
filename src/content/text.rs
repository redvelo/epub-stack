//! Extracted source text, chunks, and Unicode code-point ranges.
//!
//! Text chunks give search and TTS applications body text, headings, page labels, captions, and
//! image alternatives with available language, direction, and fragment context. Streams are
//! normalized source text rather than browser-rendered text; they do not model layout,
//! CSS-generated content, or grapheme segmentation.

use crate::semantics::{HeadingLevel, TextDirection};

/// One searchable or speakable text unit from an XHTML resource.
///
/// A chunk either ranges over its containing [`TextStream`] or owns derived text such as
/// an image alternative. Passing a stream from another resource can return an error or
/// unrelated text; chunks and streams must be used from the same [`super::XhtmlFacts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub(crate) id: String,
    pub(crate) kind: TextChunkKind,
    pub(crate) content: TextChunkContent,
    pub(crate) fragment: Option<String>,
    pub(crate) lang: Option<String>,
    pub(crate) dir: Option<TextDirection>,
}

/// The source meaning of an extracted text chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextChunkKind {
    /// Text from ordinary body flow.
    Body,
    /// Text from a heading at the stated level.
    Heading {
        /// The constrained HTML heading level.
        level: HeadingLevel,
    },
    /// An authored page-break label.
    PagebreakLabel,
    /// A figure caption.
    FigureCaption,
    /// A table caption.
    TableCaption,
    /// Alternative text owned by the chunk rather than present in the text stream.
    AltText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextChunkContent {
    Stream(TextRange),
    Owned(String),
}

impl TextChunkContent {
    fn text<'a>(&'a self, stream: &'a TextStream) -> Result<&'a str, TextRangeError> {
        match self {
            Self::Stream(range) => stream.text_for_range(*range),
            Self::Owned(text) => Ok(text),
        }
    }

    fn stream_range(&self) -> Option<TextRange> {
        match self {
            Self::Stream(range) => Some(*range),
            Self::Owned(_) => None,
        }
    }
}

/// A half-open Unicode code-point range in a [`TextStream`].
///
/// Offsets count Unicode scalar values, not UTF-8 bytes, UTF-16 code units, grapheme
/// clusters, or rendered characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl TextRange {
    /// Creates `[start, end)`, returning `None` when `start > end`.
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    /// Returns the inclusive code-point offset.
    pub fn start(self) -> u64 {
        self.start
    }

    /// Returns the exclusive code-point offset.
    pub fn end(self) -> u64 {
        self.end
    }
}

/// Extracted source-order text for one XHTML resource.
///
/// This is not browser-rendered text and does not model layout, CSS-generated content,
/// grapheme segmentation, or DOM text-node identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextStream {
    pub(crate) text: String,
}

impl TextStream {
    /// Returns the complete extracted UTF-8 text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the number of Unicode scalar values in the stream.
    pub fn code_point_len(&self) -> u64 {
        self.text.chars().count() as u64
    }

    /// Returns text for a half-open code-point range.
    pub fn text_for_range(&self, range: TextRange) -> Result<&str, TextRangeError> {
        let start = byte_index_for_code_point_offset(&self.text, range.start)?;
        let end = byte_index_for_code_point_offset(&self.text, range.end)?;
        self.text.get(start..end).ok_or(TextRangeError::OutOfBounds)
    }
}

fn byte_index_for_code_point_offset(text: &str, offset: u64) -> Result<usize, TextRangeError> {
    let offset = usize::try_from(offset).map_err(|_| TextRangeError::OutOfBounds)?;
    text.char_indices()
        .map(|(byte_index, _)| byte_index)
        .chain(std::iter::once(text.len()))
        .nth(offset)
        .ok_or(TextRangeError::OutOfBounds)
}

/// Failure to apply a code-point range to a text stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextRangeError {
    /// A start or end offset exceeds the stream's code-point length.
    #[error("text range is outside the resource text stream")]
    OutOfBounds,
}

impl TextChunk {
    /// Returns the analysis-local chunk identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the chunk's source meaning.
    pub fn kind(&self) -> TextChunkKind {
        self.kind
    }

    /// Borrows this chunk's text, using its containing stream for ranged content.
    pub fn text<'a>(&'a self, stream: &'a TextStream) -> Result<&'a str, TextRangeError> {
        self.content.text(stream)
    }

    /// Returns its range when the text is borrowed from the containing stream.
    pub fn stream_range(&self) -> Option<TextRange> {
        self.content.stream_range()
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Returns the effective authored language when recorded.
    pub fn lang(&self) -> Option<&str> {
        self.lang.as_deref()
    }

    /// Returns the effective authored text direction when recorded.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }

    /// Returns the heading level for heading chunks.
    pub fn heading_level(&self) -> Option<HeadingLevel> {
        match self.kind {
            TextChunkKind::Heading { level } => Some(level),
            _ => None,
        }
    }
}
