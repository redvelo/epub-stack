//! Connect EPUB publications to application-owned resource storage.
//!
//! Implement [`crate::ResourceProvider`] for databases, network snapshots, virtual filesystems,
//! or other storage. A provider must expose one stable set of paths and bytes for its lifetime.

use crate::resource::{EpubPath, EpubPathError};

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read};
use std::path::Path;

/// Result of reading from a [`ResourceProvider`].
pub type ReadResult<T> = std::result::Result<T, ProviderReadError>;

#[derive(Debug, thiserror::Error)]
/// Failure to open or consume one provider resource.
pub enum ProviderReadError {
    /// The canonical path is absent from the provider's stable view.
    #[error("Resource not found: {path}")]
    MissingResource {
        /// Canonical path that was requested.
        path: EpubPath,
    },
    /// Filesystem-style I/O failed for a resource path.
    #[error("IO error for {path}: {source}")]
    IoPath {
        /// Underlying I/O failure.
        source: std::io::Error,
        /// Canonical path that was requested.
        path: EpubPath,
    },
    /// A non-I/O provider backend failed while reading a resource.
    #[error("Provider backend failed while reading {path}: {source}")]
    Backend {
        /// Canonical path that was requested.
        path: EpubPath,
        /// Backend-specific failure.
        source: Box<dyn std::error::Error>,
    },
}

impl ProviderReadError {
    /// Wraps a backend-specific read failure for `path`.
    pub fn backend(path: EpubPath, source: impl std::error::Error + 'static) -> Self {
        Self::Backend {
            path,
            source: Box::new(source),
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure to produce a complete, deterministic provider index.
pub enum ResourceProviderIndexError {
    /// At least one configured limit was zero.
    #[error("resource provider index limits must be nonzero")]
    InvalidLimits,
    /// Enumeration contained more entries than permitted.
    #[error("resource provider index entry count limit exceeded: {limit}")]
    EntryCountLimitExceeded {
        /// Applied maximum entry count.
        limit: usize,
    },
    /// The sum of canonical path lengths exceeded the byte budget.
    #[error("resource provider index total path bytes limit exceeded: {limit}")]
    TotalPathBytesLimitExceeded {
        /// Applied maximum total path bytes.
        limit: usize,
    },
    /// An enumerated path was not a canonical [`EpubPath`].
    #[error("invalid resource provider index path")]
    InvalidPath,
    /// Enumeration contained the same canonical path more than once.
    #[error("duplicate resource provider index path: {0}")]
    DuplicatePath(
        /// Canonical path that occurred more than once.
        EpubPath,
    ),
    /// The backend failed while enumerating its complete view.
    #[error("resource provider index enumeration failed: {source}")]
    Enumeration {
        /// Backend-specific enumeration failure.
        source: Box<dyn std::error::Error>,
    },
}

impl ResourceProviderIndexError {
    /// Wraps a backend-specific enumeration failure.
    pub fn enumeration(source: impl std::error::Error + 'static) -> Self {
        Self::Enumeration {
            source: Box::new(source),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
/// Operational limits for complete provider enumeration.
///
/// Both limits are inclusive and must be nonzero. They bound index construction work;
/// they are not EPUB validity rules or per-resource read limits.
pub struct ResourceProviderIndexLimits {
    max_entries: usize,
    max_total_path_bytes: usize,
}

impl ResourceProviderIndexLimits {
    /// Creates nonzero provider index limits.
    pub fn new(
        max_entries: usize,
        max_total_path_bytes: usize,
    ) -> std::result::Result<Self, ResourceProviderIndexError> {
        if max_entries == 0 || max_total_path_bytes == 0 {
            return Err(ResourceProviderIndexError::InvalidLimits);
        }
        Ok(Self {
            max_entries,
            max_total_path_bytes,
        })
    }

    /// Returns the inclusive maximum number of enumerated entries.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Returns the inclusive maximum sum of canonical path byte lengths.
    pub fn max_total_path_bytes(&self) -> usize {
        self.max_total_path_bytes
    }
}

impl Default for ResourceProviderIndexLimits {
    fn default() -> Self {
        Self {
            max_entries: 65_536,
            max_total_path_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// One canonical entry in a complete provider index.
pub struct ResourceProviderEntry {
    path: EpubPath,
    is_special: bool,
    size_bytes: Option<u64>,
}

impl ResourceProviderEntry {
    /// Creates an entry and derives whether its path is an OCF control path.
    ///
    /// `size_bytes` is optional provider metadata and is not verified by this constructor.
    pub fn new(path: EpubPath, size_bytes: Option<u64>) -> Self {
        let is_special = ResourceProviderIndex::is_special_path(path.as_str());
        Self {
            path,
            is_special,
            size_bytes,
        }
    }

    /// Returns the canonical provider path.
    pub fn path(&self) -> &EpubPath {
        &self.path
    }

    /// Reports whether this is `mimetype` or lies below `META-INF/`.
    pub fn is_special(&self) -> bool {
        self.is_special
    }

    /// Returns the provider-reported logical byte length, when known.
    pub fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
/// A complete, canonical, deterministic snapshot of provider inventory.
///
/// Entries are sorted by [`EpubPath`], canonical paths are unique, and construction is
/// bounded by [`ResourceProviderIndexLimits`]. The index describes the same stable logical
/// provider view from which subsequent reads must be served.
pub struct ResourceProviderIndex {
    entries: Vec<ResourceProviderEntry>,
    by_path: HashMap<EpubPath, usize>,
}

impl ResourceProviderIndex {
    /// Validates, bounds, deduplicates, and sorts a complete entry enumeration.
    ///
    /// The entry-count and total-path-byte limits are inclusive. Path bytes are UTF-8 byte
    /// lengths, summed before sorting; resource sizes do not contribute to either limit.
    pub fn try_from_entries(
        entries: impl IntoIterator<Item = ResourceProviderEntry>,
        limits: &ResourceProviderIndexLimits,
    ) -> std::result::Result<Self, ResourceProviderIndexError> {
        let mut checked_entries = Vec::new();
        let mut total_path_bytes = 0usize;

        for entry in entries {
            if EpubPath::new(entry.path.as_path()).as_ref() != Ok(&entry.path) {
                return Err(ResourceProviderIndexError::InvalidPath);
            }
            if checked_entries.len() == limits.max_entries {
                return Err(ResourceProviderIndexError::EntryCountLimitExceeded {
                    limit: limits.max_entries,
                });
            }
            total_path_bytes = total_path_bytes
                .checked_add(entry.path.as_str().len())
                .filter(|total| *total <= limits.max_total_path_bytes)
                .ok_or(ResourceProviderIndexError::TotalPathBytesLimitExceeded {
                    limit: limits.max_total_path_bytes,
                })?;
            checked_entries.push(entry);
        }

        checked_entries.sort_by(|left, right| left.path.cmp(&right.path));
        let mut by_path = HashMap::with_capacity(checked_entries.len());
        for (idx, entry) in checked_entries.iter().enumerate() {
            if by_path.insert(entry.path.clone(), idx).is_some() {
                return Err(ResourceProviderIndexError::DuplicatePath(
                    entry.path.clone(),
                ));
            }
        }

        Ok(Self {
            entries: checked_entries,
            by_path,
        })
    }

    /// Returns every entry in canonical path order, including OCF control entries.
    pub fn entries(&self) -> &[ResourceProviderEntry] {
        self.entries.as_slice()
    }

    /// Looks up an entry by exact canonical path identity.
    pub fn get(&self, path: &EpubPath) -> Option<&ResourceProviderEntry> {
        self.by_path
            .get(path)
            .and_then(|idx| self.entries.get(*idx))
    }

    /// Iterates publication entries in canonical path order, excluding OCF control paths.
    pub fn publication_entries(&self) -> impl Iterator<Item = &ResourceProviderEntry> {
        self.entries.iter().filter(|entry| !entry.is_special)
    }

    /// Reports whether a path is the root `mimetype` entry or lies below `META-INF/`.
    pub fn is_special_path(path: &str) -> bool {
        path == "mimetype" || path.starts_with("META-INF/")
    }
}

/// Read-only application storage used to open and serve one publication.
///
/// A provider must pin any live release, transaction, generation, or ETag before
/// returning a successful index. Paths and the bytes they identify must then remain
/// semantically stable for the lifetime of the provider value. Implementations that
/// cannot establish that stable view return an index error from [`Self::index`].
/// Publication edits do not revise this provider view.
pub trait ResourceProvider {
    /// Opens `path` and lets the application consume it through a scoped reader.
    ///
    /// The callback is invoked at most once and can stream without buffering the whole resource.
    /// Implementations must serve bytes from the same stable view represented by [`Self::index`].
    /// Callback return values, including nested I/O results, are returned unchanged.
    fn read_with<T>(&self, path: &EpubPath, read: impl FnOnce(&mut dyn Read) -> T)
    -> ReadResult<T>;

    /// Reads an entire resource into memory.
    ///
    /// This convenience method is unbounded by provider index limits. Callers that require a
    /// byte budget should use [`Self::read_with`] with a bounded reader.
    fn read(&self, path: &EpubPath) -> ReadResult<Vec<u8>> {
        self.read_with(path, |reader| {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        })?
        .map_err(|source| ProviderReadError::IoPath {
            source,
            path: path.clone(),
        })
    }

    /// Returns a complete deterministic index for this provider's stable logical view.
    ///
    /// A provider must pin its release, transaction, generation, ETag, or equivalent before
    /// success. If it cannot guarantee that indexed paths and their bytes remain semantically
    /// stable for the provider value's lifetime, it must return an error.
    fn index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError>;
}

#[derive(Debug, Default, Clone)]
/// In-memory storage for created EPUBs, tests, or already-loaded resource sets.
///
/// Cloning creates an independent byte snapshot. Mutation requires exclusive access, so a
/// borrowed provider view cannot change during a read.
pub struct MemoryResourceProvider {
    entries: BTreeMap<EpubPath, Vec<u8>>,
}

impl MemoryResourceProvider {
    /// Creates an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a provider from path and byte pairs.
    ///
    /// Later duplicate paths replace earlier bytes after canonical path validation.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (impl AsRef<Path>, Vec<u8>)>,
    ) -> std::result::Result<Self, EpubPathError> {
        let mut provider = Self::new();
        for (path, bytes) in entries {
            provider.insert(path, bytes)?;
        }
        Ok(provider)
    }

    /// Inserts or replaces bytes at a canonicalized EPUB path.
    ///
    /// Returns a path error without modifying the provider when canonicalization fails.
    pub fn insert(
        &mut self,
        path: impl AsRef<Path>,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), EpubPathError> {
        let epub_path = EpubPath::new(path)?;
        self.entries.insert(epub_path, bytes);
        Ok(())
    }

    /// Removes and returns the bytes at an exact canonical path.
    pub fn remove(&mut self, path: &EpubPath) -> Option<Vec<u8>> {
        self.entries.remove(path)
    }

    /// Borrows the bytes at an exact canonical path.
    pub fn get(&self, path: &EpubPath) -> Option<&[u8]> {
        self.entries.get(path).map(Vec::as_slice)
    }

    /// Reports whether an exact canonical path is present.
    pub fn contains(&self, path: &EpubPath) -> bool {
        self.entries.contains_key(path)
    }

    /// Iterates paths and bytes in canonical path order.
    pub fn entries(&self) -> impl Iterator<Item = (&EpubPath, &[u8])> {
        self.entries
            .iter()
            .map(|(path, bytes)| (path, bytes.as_slice()))
    }

    /// Consumes the provider and returns its path-ordered storage.
    pub fn into_entries(self) -> BTreeMap<EpubPath, Vec<u8>> {
        self.entries
    }
}

impl ResourceProvider for MemoryResourceProvider {
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> ReadResult<T> {
        let bytes = self
            .entries
            .get(path)
            .ok_or_else(|| ProviderReadError::MissingResource { path: path.clone() })?;
        let mut cursor = Cursor::new(bytes.as_slice());
        Ok(read(&mut cursor))
    }

    fn index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
        ResourceProviderIndex::try_from_entries(
            self.entries.iter().map(|(path, bytes)| {
                ResourceProviderEntry::new(path.clone(), Some(bytes.len() as u64))
            }),
            limits,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> ResourceProviderEntry {
        ResourceProviderEntry::new(EpubPath::new(path).unwrap(), Some(size))
    }

    #[test]
    fn resource_provider_index_enforces_invalid_exact_and_plus_one_limits() {
        assert!(matches!(
            ResourceProviderIndexLimits::new(0, 1),
            Err(ResourceProviderIndexError::InvalidLimits)
        ));
        assert!(matches!(
            ResourceProviderIndexLimits::new(1, 0),
            Err(ResourceProviderIndexError::InvalidLimits)
        ));

        let entries = [entry("EPUB/a", 1), entry("EPUB/b", 1)];
        let exact = ResourceProviderIndexLimits::new(2, 12).unwrap();
        assert!(ResourceProviderIndex::try_from_entries(entries.clone(), &exact).is_ok());

        let count_limit = ResourceProviderIndexLimits::new(1, 12).unwrap();
        assert!(matches!(
            ResourceProviderIndex::try_from_entries(entries.clone(), &count_limit),
            Err(ResourceProviderIndexError::EntryCountLimitExceeded { limit: 1 })
        ));

        let path_limit = ResourceProviderIndexLimits::new(2, 11).unwrap();
        assert!(matches!(
            ResourceProviderIndex::try_from_entries(entries, &path_limit),
            Err(ResourceProviderIndexError::TotalPathBytesLimitExceeded { limit: 11 })
        ));
    }

    #[test]
    fn resource_provider_index_rejects_duplicate_canonical_paths() {
        let limits = ResourceProviderIndexLimits::default();
        let path = EpubPath::new("EPUB/chapter.xhtml").unwrap();
        let err = ResourceProviderIndex::try_from_entries(
            [
                ResourceProviderEntry::new(path.clone(), None),
                ResourceProviderEntry::new(path.clone(), Some(10)),
            ],
            &limits,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ResourceProviderIndexError::DuplicatePath(error_path) if error_path == path
        ));
    }

    #[test]
    fn resource_provider_index_orders_entries_and_filters_special_entries() {
        let index = ResourceProviderIndex::try_from_entries(
            [
                entry("EPUB/z.xhtml", 1),
                entry("META-INF/container.xml", 2),
                entry("mimetype", 3),
                entry("EPUB/a.xhtml", 4),
            ],
            &ResourceProviderIndexLimits::default(),
        )
        .unwrap();

        assert_eq!(
            index
                .entries()
                .iter()
                .map(|entry| entry.path().as_str())
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
                .map(|entry| entry.path().as_str())
                .collect::<Vec<_>>(),
            ["EPUB/a.xhtml", "EPUB/z.xhtml"]
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
