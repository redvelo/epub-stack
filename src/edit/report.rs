//! Inspect resource changes installed by an in-memory edit commit.

use crate::{EpubPath, edit::StructuralResourceKind};

/// Describes the final effect of a commit on one resource path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditChange {
    /// Adds or replaces ordinary resource bytes.
    UpsertResource {
        /// Canonical provider path.
        path: EpubPath,
        /// Final staged byte length.
        size_bytes: usize,
    },
    /// Removes a resource from the effective publication view.
    RemoveResource {
        /// Canonical provider path.
        path: EpubPath,
    },
    /// Rewrites source bytes together with their staged semantic model.
    RewriteStructuralResource {
        /// Canonical provider path.
        path: EpubPath,
        /// Kind of structural document represented by the rewritten bytes.
        kind: StructuralResourceKind,
        /// Final staged byte length.
        size_bytes: usize,
    },
}

impl EditChange {
    pub(crate) fn path(&self) -> &EpubPath {
        match self {
            Self::UpsertResource { path, .. }
            | Self::RemoveResource { path }
            | Self::RewriteStructuralResource { path, .. } => path,
        }
    }
}
