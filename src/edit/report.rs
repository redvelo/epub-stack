//! Inspect resource changes installed by an in-memory edit commit.

use crate::EpubPath;

/// Lists the final per-path effects of an in-memory commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditReport {
    pub(crate) changes: Vec<EditChange>,
}

impl EditReport {
    /// Returns changes in canonical path order, with at most one record per path.
    pub fn changes(&self) -> &[EditChange] {
        &self.changes
    }
}

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
        kind: StructuralEditKind,
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

/// Identifies the structural document rewritten by an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralEditKind {
    /// The package document was rewritten.
    Package,
    /// The selected EPUB navigation document was rewritten.
    Navigation,
}
