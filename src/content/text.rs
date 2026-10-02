//! Source-order text, overlapping element spans, and attribute-derived text.
//!
//! Obtain a [`TextStream`] from [`super::XhtmlFacts::text_stream`]. Its [`text()`](TextStream::text)
//! includes text outside recognized semantic blocks. [`spans()`](TextStream::spans) adds
//! element roles and inherited metadata; spans overlap and should not be concatenated into
//! a reading sequence. [`supplementary()`](TextStream::supplementary) holds image alternatives
//! and attribute-derived page labels separately.
//!
//! Extract document text and its semantic spans.
//!
//! ```
//! use epub_stack::{EpubZip, content::{ContentFacts, text::TextRole}};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//! let analysis = book.analyze();
//!
//! for resource in analysis.analyzed_resources() {
//!     if let Some(xhtml) = resource.content().value().and_then(ContentFacts::as_xhtml) {
//!         let text = xhtml.text_stream();
//!         println!("{}", text.text());
//!
//!         for span in text.spans() {
//!             if let TextRole::Heading { level } = span.role() {
//!                 println!("h{}: {}", level.get(), span.text());
//!             }
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use super::FragmentFact;
use crate::semantics::{HeadingLevel, TextDirection};
use std::ops::Range;

/// A half-open Unicode scalar-value range in one extracted text stream.
///
/// Offsets count Rust `char` values and carry no resource identity. They are not browser positions.
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

/// Source element and inherited metadata for an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextOrigin {
    pub(crate) element: String,
    pub(crate) element_ordinal: usize,
    pub(crate) fragment: Option<FragmentFact>,
    pub(crate) lang: Option<String>,
    pub(crate) dir: Option<TextDirection>,
}

impl TextOrigin {
    /// Returns the source element name.
    pub fn element(&self) -> &str {
        &self.element
    }
    /// Returns the element's extraction-local position, not a persistent identifier.
    pub fn element_ordinal(&self) -> usize {
        self.element_ordinal
    }
    /// Returns the fragment declared by this element or its nearest ancestor.
    ///
    /// This is authored context, not a guarantee of a unique or exact text destination.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        self.fragment.as_ref()
    }
    /// Returns the effective authored language at the element.
    pub fn lang(&self) -> Option<&str> {
        self.lang.as_deref()
    }
    /// Returns the effective authored text direction at the element.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }
}

/// An element's semantic role over the extracted stream. Spans may overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    /// Element context without a more specific text role.
    Element,
    /// Ordinary paragraph-like content.
    Body,
    /// Heading content.
    Heading {
        /// The authored heading level.
        level: HeadingLevel,
    },
    /// Source text of an authored page-break element.
    Pagebreak,
    /// Figure caption content.
    FigureCaption,
    /// Table caption content.
    TableCaption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSpan {
    pub(crate) range: TextRange,
    pub(crate) bytes: Range<usize>,
    pub(crate) role: TextRole,
    pub(crate) origin: TextOrigin,
}

/// A semantic span borrowed together with its owning text stream.
#[derive(Debug, Clone, Copy)]
pub struct TextSpanRef<'a> {
    stream: &'a TextStream,
    span: &'a TextSpan,
}

impl<'a> TextSpanRef<'a> {
    /// Borrows the span's text in constant time.
    pub fn text(self) -> &'a str {
        &self.stream.text[self.span.bytes.clone()]
    }
    /// Returns representation-local code-point offsets, not DOM or annotation positions.
    pub fn range(self) -> TextRange {
        self.span.range
    }
    /// Returns the span's semantic role.
    pub fn role(self) -> TextRole {
        self.span.role
    }
    /// Returns the source context at this element; nested spans retain metadata overrides.
    pub fn origin(self) -> &'a TextOrigin {
        &self.span.origin
    }
}

/// The authored attribute supplying supplementary text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplementaryTextSource {
    /// An image alternative supplied by `alt`.
    Alternative,
    /// A page label supplied by `aria-label`.
    PagebreakAriaLabel,
    /// A page label supplied by `title`.
    PagebreakTitle,
}

/// Attribute-derived text with source element context, not a range of document text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupplementaryText {
    pub(crate) text: String,
    pub(crate) source: SupplementaryTextSource,
    pub(crate) origin: TextOrigin,
}

impl SupplementaryText {
    /// Returns the extracted attribute text.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Returns the attribute that supplied the text.
    pub fn source(&self) -> SupplementaryTextSource {
        self.source
    }
    /// Returns its source element context.
    ///
    /// This records provenance, not an exact browser locator.
    pub fn origin(&self) -> &TextOrigin {
        &self.origin
    }
}

/// Normalized source-order document text.
///
/// Character references are decoded. HTML whitespace is collapsed, including inside `pre`,
/// and source block boundaries and `br` elements become newlines. Head content, scripts,
/// styles, and templates are excluded. CSS layout, visibility, whitespace rules, and generated
/// content are not evaluated. Semantic spans overlap and do not partition this stream;
/// supplementary attribute text is stored separately.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextStream {
    pub(crate) text: String,
    pub(crate) code_point_len: u64,
    pub(crate) spans: Vec<TextSpan>,
    pub(crate) supplementary: Vec<SupplementaryText>,
}

impl TextStream {
    /// Returns all extracted document text, including text outside semantic blocks.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Returns the number of Unicode scalar values.
    pub fn code_point_len(&self) -> u64 {
        self.code_point_len
    }
    /// Iterates overlapping spans in source element order, borrowing their owning stream.
    pub fn spans(&self) -> impl ExactSizeIterator<Item = TextSpanRef<'_>> {
        self.spans
            .iter()
            .map(|span| TextSpanRef { stream: self, span })
    }
    /// Returns attribute-derived text in source element order.
    pub fn supplementary(&self) -> &[SupplementaryText] {
        &self.supplementary
    }
    /// Resolves arbitrary representation-local code-point offsets by scanning the stream.
    ///
    /// Prefer [`TextSpanRef::text`] when reading an existing span; it does not scan preceding text.
    ///
    /// # Errors
    ///
    /// Returns [`TextRangeError::OutOfBounds`] if either endpoint exceeds the stream length.
    /// A range from another stream is not detected if its offsets fit.
    pub fn text_for_range(&self, range: TextRange) -> Result<&str, TextRangeError> {
        let mut start = None;
        for (offset, byte) in self
            .text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(self.text.len()))
            .enumerate()
        {
            if offset as u64 == range.start {
                start = Some(byte);
            }
            if offset as u64 == range.end {
                return start
                    .and_then(|start| self.text.get(start..byte))
                    .ok_or(TextRangeError::OutOfBounds);
            }
        }
        Err(TextRangeError::OutOfBounds)
    }
}

/// Failure to apply offsets to a text stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextRangeError {
    /// The requested range lies outside the stream.
    #[error("text range is outside the resource text stream")]
    OutOfBounds,
}
