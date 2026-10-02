//! Stage, preview, and commit publication edits in memory.
//!
//! Start with [`Epub::edit`](crate::Epub::edit), chain operations on [`EpubEdit`], then call
//! [`EpubEdit::preview`] to inspect the proposed state. [`EpubEditPreview::commit`] installs it
//! in the open [`Epub`](crate::Epub); dropping either value leaves the publication unchanged.
//!
//! Committing does not write an EPUB. Export the committed in-memory state separately with
//! [`Epub::export`](crate::Epub::export). Changed XML may be normalized. Preview does not rerun
//! content analysis.

pub use error::{
    EditError, GuideHrefFailure, SelectionFailure, StructuralResourceKind,
    StructuralVerificationFailure,
};
pub use report::EditChange;
pub use transaction::{EpubEdit, EpubEditPreview};

mod error;
mod report;
pub mod select;
mod transaction;

/// Chooses whether an embedded annotation change also removes resources the previous
/// annotation set referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedAnnotationResourceRemoval {
    /// Change only `META-INF/annotations.json`, leaving previously referenced resources in
    /// place.
    SetOnly,
    /// Also remove previously referenced resources that the new state no longer references.
    SetAndReferencedResources,
}
