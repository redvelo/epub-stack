//! Stage, preview, and commit publication edits in memory.
//!
//! Start with [`Epub::edit`](crate::Epub::edit), chain operations on [`EpubEdit`], then call
//! [`EpubEdit::preview`] to validate and inspect the proposed package, navigation, annotations,
//! resources, and change list. [`EpubEditPreview::commit`] installs that preview in the open
//! [`Epub`](crate::Epub); dropping either value leaves the publication unchanged.
//!
//! Committing does not write an EPUB. Export the committed in-memory state separately with
//! [`Epub::export`](crate::Epub::export). Untouched resources keep their bytes, while package and
//! navigation edits serialize changed XML and can normalize lexical details. Preview does not
//! rerun full content analysis.

pub use error::{
    EditError, NavigationGenerationError, SelectionFailure, StructuralResourceKind,
    StructuralXmlDecodeError, StructuralXmlOperationError,
};
pub use report::{EditChange, EditReport, StructuralEditKind};
pub use transaction::{EpubEdit, EpubEditPreview};

pub mod annotation;
mod error;
pub mod package;
mod report;
mod transaction;

/// Navigation list, point, and insertion selectors.
pub mod navigation;
