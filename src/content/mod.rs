//! Source text, fragments, structure, media, forms, and scripts extracted from content.
//!
//! [`XhtmlFacts`] provides source-order text plus authored viewport metadata, headings, page
//! breaks, figures, tables, media, forms, and scripts. [`SvgFacts`] provides standalone SVG text,
//! fragment targets, scripts, and nested XHTML facts from `foreignObject`, while
//! [`crate::media_overlay::SmilFacts`] provides synchronized playback nodes.
//! Structural semantics use [`crate::semantics::SemanticToken`].
//! Use [`XhtmlFacts::text_stream`] for document text, overlapping [spans](text::TextSpanRef),
//! and separate [attribute text](text::SupplementaryText). Applications choose how to index
//! these observations for search or narration.
//!
//! These models describe source content, not browser-rendered text. Check
//! [`crate::analysis::coverage::Coverage::content`] for extraction coverage, or use
//! [`crate::Epub::bytes`] to read the original source.

mod documents;
pub(crate) mod facts;
pub mod text;

pub(crate) mod extraction;

pub use documents::{
    ContentFacts, DocumentActivities, DocumentActivity, SvgFacts, SvgForeignObjectFact,
    SvgTextFact, XhtmlFacts,
};
pub use facts::{
    FormFact, FragmentAttribute, FragmentFact, MediaFact, MediaSourceContext, ScriptFact,
    StructureFact, StructureRole, TrackKind, ViewportDirective, ViewportDirectiveFact,
    ViewportFact,
};

pub(crate) use extraction::{
    XhtmlExtraction, XhtmlLinkAssociations, parse_xhtml_document_from_reader_counted,
};
pub(crate) use facts::{LinkFact, NavigationLinkKind};
