//! Searchable text, fragments, viewport metadata, structure, media, forms, and scripts extracted
//! from content.
//!
//! [`XhtmlFacts`] provides source-order text plus authored viewport metadata, headings, page
//! breaks, figures, tables, media, forms, and scripts. [`SvgFacts`] provides standalone SVG text,
//! fragment targets, scripts, and nested XHTML facts from `foreignObject`, while
//! [`crate::media_overlay::SmilFacts`] provides synchronized playback nodes.
//!
//! These values describe normalized source content, not browser-rendered layout or text. They
//! belong to an analysis snapshot and continue to describe that analyzed version after an edit.
//! Extraction coverage is available from [`crate::analysis::coverage::Coverage::content`]. The
//! models are not mutable or lossless XHTML, SVG, or CSS syntax trees. Use
//! [`crate::Epub::resource`] and [`crate::resource::Resource::bytes`] when exact source bytes are
//! required.

mod documents;
pub(crate) mod facts;
pub mod text;

pub(crate) mod extraction;

pub use documents::{ContentFacts, SvgFacts, SvgForeignObjectFact, SvgTextFact, XhtmlFacts};
pub use facts::{
    FormFact, FragmentAttribute, FragmentFact, HtmlStructuralElement, MediaFact,
    MediaSourceContext, ScriptFact, SemanticSource, SemanticToken, StructureFact,
    ViewportDirective, ViewportDirectiveFact, ViewportFact,
};

pub(crate) use extraction::{
    XhtmlExtraction, XhtmlLinkAssociations, parse_xhtml_document_from_reader_counted,
};
pub(crate) use facts::{LinkFact, NavigationLinkKind};
