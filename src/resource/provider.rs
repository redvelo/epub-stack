//! Connect EPUB publications to application-owned resource storage.
//!
//! Implement [`crate::ResourceProvider`] for databases, network snapshots, virtual filesystems,
//! or other storage. A provider must expose one stable set of paths and bytes for its lifetime.

use crate::publication::EpubOpenLimits;
use crate::resource::{EpubPath, EpubPathError};

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

/// A boxed provider backend failure.
pub type BackendError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
/// Failure to open or consume one provider resource.
pub enum ProviderReadError {
    /// The canonical path is absent from the provider's stable view.
    #[error("resource not found: {path}")]
    Missing {
        /// Canonical path that was requested.
        path: EpubPath,
    },
    /// Filesystem-style I/O failed for a resource path.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Canonical path that was requested.
        path: EpubPath,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// A non-I/O provider backend failed while reading a resource.
    #[error("provider backend failed while reading {path}: {source}")]
    Backend {
        /// Canonical path that was requested.
        path: EpubPath,
        /// Backend-specific failure.
        source: BackendError,
    },
}

impl ProviderReadError {
    /// Wraps a backend-specific read failure for `path`.
    pub fn backend(path: EpubPath, source: impl Into<BackendError>) -> Self {
        Self::Backend {
            path,
            source: source.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure to enumerate a complete, deterministic provider view.
pub enum ProviderIndexError {
    /// Enumeration contained more entries than permitted.
    #[error("provider entry count limit exceeded: {limit}")]
    EntryLimit {
        /// Applied maximum entry count.
        limit: usize,
    },
    /// The sum of canonical path lengths exceeded the byte budget.
    #[error("provider path bytes limit exceeded: {limit}")]
    PathBytesLimit {
        /// Applied maximum total path bytes.
        limit: usize,
    },
    /// The provider holds an entry whose name is not a canonical [`EpubPath`].
    #[error("provider entry name is not a canonical EPUB path")]
    InvalidPath,
    /// Enumeration contained the same canonical path more than once.
    #[error("duplicate provider path: {0}")]
    DuplicatePath(
        /// Canonical path that occurred more than once.
        EpubPath,
    ),
    /// The backend failed while enumerating its complete view.
    #[error("provider enumeration failed: {0}")]
    Backend(
        /// Backend-specific enumeration failure.
        BackendError,
    ),
}

impl ProviderIndexError {
    /// Wraps a backend-specific enumeration failure.
    pub fn backend(source: impl Into<BackendError>) -> Self {
        Self::Backend(source.into())
    }
}

/// Where a publication's bytes come from: a ZIP, a directory, a database, anything you can read
/// by path.
///
/// Implement this to open a book from storage this crate knows nothing about.
///
/// One rule: while the provider is alive, the same path must keep returning the same bytes. If
/// your storage can change underneath you, take a snapshot, transaction or ETag first and serve
/// from that. Editing a publication never writes back through here.
pub trait ResourceProvider {
    /// Opens one file and hands a reader to the callback.
    ///
    /// Call the callback at most once. Whatever it returns comes back untouched, so a callback
    /// that itself fails can report that in `T`.
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> Result<T, ProviderReadError>;

    /// Lists every path you can serve, with byte lengths where you know them.
    ///
    /// Yield them in any order. Sorting, duplicate rejection and limits are handled for you.
    fn entries(&self) -> Result<impl Iterator<Item = (EpubPath, Option<u64>)>, ProviderIndexError>;
}

#[derive(Debug, Default, Clone)]
/// A publication held in memory, for tests, freshly created books, or bytes you already have.
///
/// Cloning copies the bytes, so each clone is independent.
pub struct MemoryResourceProvider {
    entries: BTreeMap<EpubPath, Vec<u8>>,
}

impl MemoryResourceProvider {
    /// Creates an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider from path text and byte pairs.
    ///
    /// Later duplicate paths replace earlier bytes.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (impl AsRef<str>, Vec<u8>)>,
    ) -> Result<Self, EpubPathError> {
        let mut provider = Self::new();
        for (path, bytes) in entries {
            provider.insert(EpubPath::new(path)?, bytes);
        }
        Ok(provider)
    }

    /// Inserts or replaces bytes, returning the previous bytes.
    pub fn insert(&mut self, path: EpubPath, bytes: Vec<u8>) -> Option<Vec<u8>> {
        self.entries.insert(path, bytes)
    }

    /// Removes and returns the bytes at an exact canonical path.
    pub fn remove(&mut self, path: &EpubPath) -> Option<Vec<u8>> {
        self.entries.remove(path)
    }

    /// Borrows the bytes at an exact canonical path.
    pub fn get(&self, path: &EpubPath) -> Option<&[u8]> {
        self.entries.get(path).map(Vec::as_slice)
    }

    /// Iterates paths and bytes in canonical path order.
    pub fn iter(&self) -> impl Iterator<Item = (&EpubPath, &[u8])> {
        self.entries
            .iter()
            .map(|(path, bytes)| (path, bytes.as_slice()))
    }

    /// Consumes the provider and returns its path-ordered storage.
    pub fn into_inner(self) -> BTreeMap<EpubPath, Vec<u8>> {
        self.entries
    }
}

impl ResourceProvider for MemoryResourceProvider {
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> Result<T, ProviderReadError> {
        let bytes = self
            .entries
            .get(path)
            .ok_or_else(|| ProviderReadError::Missing { path: path.clone() })?;
        Ok(read(&mut Cursor::new(bytes.as_slice())))
    }

    fn entries(&self) -> Result<impl Iterator<Item = (EpubPath, Option<u64>)>, ProviderIndexError> {
        Ok(self
            .entries
            .iter()
            .map(|(path, bytes)| (path.clone(), Some(bytes.len() as u64))))
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct ProviderEntry {
    pub(crate) path: EpubPath,
    pub(crate) size_bytes: Option<u64>,
}

/// A complete, sorted, bounded snapshot of provider inventory.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub(crate) struct ProviderIndex {
    entries: Vec<ProviderEntry>,
}

impl ProviderIndex {
    pub(crate) fn of<R: ResourceProvider>(
        provider: &R,
        limits: &EpubOpenLimits,
    ) -> Result<Self, ProviderIndexError> {
        Self::build(provider.entries()?, limits)
    }

    pub(crate) fn build(
        entries: impl IntoIterator<Item = (EpubPath, Option<u64>)>,
        limits: &EpubOpenLimits,
    ) -> Result<Self, ProviderIndexError> {
        let max_entries = limits.max_provider_entries.get();
        let max_path_bytes = limits.max_provider_path_bytes.get();
        let mut checked = Vec::new();
        let mut total_path_bytes = 0usize;
        for (path, size_bytes) in entries {
            if checked.len() == max_entries {
                return Err(ProviderIndexError::EntryLimit { limit: max_entries });
            }
            total_path_bytes = total_path_bytes
                .checked_add(path.as_str().len())
                .filter(|total| *total <= max_path_bytes)
                .ok_or(ProviderIndexError::PathBytesLimit {
                    limit: max_path_bytes,
                })?;
            checked.push(ProviderEntry { path, size_bytes });
        }
        checked.sort_by(|left, right| left.path.cmp(&right.path));
        if let Some(pair) = checked.windows(2).find(|pair| pair[0].path == pair[1].path) {
            return Err(ProviderIndexError::DuplicatePath(pair[0].path.clone()));
        }
        Ok(Self { entries: checked })
    }

    pub(crate) fn entries(&self) -> &[ProviderEntry] {
        &self.entries
    }

    pub(crate) fn get(&self, path: &EpubPath) -> Option<&ProviderEntry> {
        self.entries
            .binary_search_by(|entry| entry.path.cmp(path))
            .ok()
            .map(|index| &self.entries[index])
    }

    pub(crate) fn publication_entries(&self) -> impl Iterator<Item = &ProviderEntry> {
        self.entries
            .iter()
            .filter(|entry| !entry.path.is_ocf_control())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;

    fn entry(path: &str, size: u64) -> (EpubPath, Option<u64>) {
        (EpubPath::new(path).unwrap(), Some(size))
    }

    fn limits(entries: usize, path_bytes: usize) -> EpubOpenLimits {
        EpubOpenLimits {
            max_provider_entries: NonZeroUsize::new(entries).unwrap(),
            max_provider_path_bytes: NonZeroUsize::new(path_bytes).unwrap(),
            ..EpubOpenLimits::default()
        }
    }

    #[test]
    fn provider_index_enforces_exact_and_plus_one_limits() {
        let entries = [entry("EPUB/a", 1), entry("EPUB/b", 1)];
        assert!(ProviderIndex::build(entries.clone(), &limits(2, 12)).is_ok());
        assert!(matches!(
            ProviderIndex::build(entries.clone(), &limits(1, 12)),
            Err(ProviderIndexError::EntryLimit { limit: 1 })
        ));
        assert!(matches!(
            ProviderIndex::build(entries, &limits(2, 11)),
            Err(ProviderIndexError::PathBytesLimit { limit: 11 })
        ));
    }

    #[test]
    fn provider_index_rejects_duplicate_canonical_paths() {
        let path = EpubPath::new("EPUB/chapter.xhtml").unwrap();
        let err = ProviderIndex::build(
            [(path.clone(), None), (path.clone(), Some(10))],
            &EpubOpenLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ProviderIndexError::DuplicatePath(error_path) if error_path == path));
    }

    #[test]
    fn provider_index_orders_entries_and_filters_ocf_control_entries() {
        let index = ProviderIndex::build(
            [
                entry("EPUB/z.xhtml", 1),
                entry("META-INF/container.xml", 2),
                entry("mimetype", 3),
                entry("EPUB/a.xhtml", 4),
            ],
            &EpubOpenLimits::default(),
        )
        .unwrap();

        assert_eq!(
            index
                .entries()
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            [
                "EPUB/a.xhtml",
                "EPUB/z.xhtml",
                "META-INF/container.xml",
                "mimetype"
            ]
        );
        assert_eq!(
            index
                .publication_entries()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["EPUB/a.xhtml", "EPUB/z.xhtml"]
        );
        assert_eq!(
            index
                .get(&EpubPath::new("EPUB/z.xhtml").unwrap())
                .unwrap()
                .size_bytes,
            Some(1)
        );
    }

    #[test]
    fn memory_resource_provider_propagates_canonical_path_errors() {
        assert!(matches!(
            MemoryResourceProvider::from_entries([("../chapter.xhtml", Vec::new())]).unwrap_err(),
            EpubPathError::DotSegment
        ));
    }
}
