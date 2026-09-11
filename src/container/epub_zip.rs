use crate::{
    Epub, EpubOpenError, EpubOpenFailure, EpubOpenLimits,
    resource::EpubPath,
    resource::provider::{
        ProviderReadError, ResourceProvider, ResourceProviderEntry, ResourceProviderIndex,
        ResourceProviderIndexError, ResourceProviderIndexLimits,
    },
    xml::decode_xml,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::ops::Range;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use zip::ZipArchive;

#[cfg(test)]
use super::RenditionAccessMode;
use super::{
    ContainerDocumentError, ContainerError, Rootfile,
    document::{parse_rootfiles, serialize_rootfiles},
};

#[cfg(test)]
const MIMETYPE: &str = "mimetype";
pub(crate) const CONTAINER_PATH: &str = "META-INF/container.xml";
const MAX_AUTOMATIC_CONTAINER_BYTES: u64 = 4 * 1024 * 1024;

type Result<T> = std::result::Result<T, ContainerError>;
type SourceEntryIndex = BTreeMap<Vec<u8>, usize>;
type SourceDirectory = (SourceEntryIndex, Vec<Vec<u8>>);

/// Physical layout of one source ZIP entry in the reader supplied to [`EpubZip`].
///
/// This describes source archive bytes, not portable publication identity. Pending logical
/// changes that shadow an entry have no source layout through [`EpubZip::source_entry_layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpubZipEntryLayout {
    data_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    crc32: u32,
    stored: bool,
    encrypted: bool,
}

impl EpubZipEntryLayout {
    /// Returns the absolute byte offset at which the entry's source data begins.
    pub fn data_offset(&self) -> u64 {
        self.data_offset
    }

    /// Returns the source entry's compressed byte length.
    pub fn compressed_size(&self) -> u64 {
        self.compressed_size
    }

    /// Returns the source entry's uncompressed byte length.
    pub fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }

    /// Returns the source entry's declared CRC-32 value.
    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    /// Returns whether the source entry uses the ZIP stored compression method.
    pub fn is_stored(&self) -> bool {
        self.stored
    }

    /// Returns whether the source entry is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// Returns a candidate source data range for a directly stored logical resource.
    ///
    /// This checks method, encryption, length equality, and arithmetic only. Callers must bind the
    /// layout to the same immutable source and recheck physical bounds before reading the range.
    pub fn stored_data_range(&self) -> Option<Range<u64>> {
        if !self.stored || self.encrypted || self.compressed_size != self.uncompressed_size {
            return None;
        }
        self.data_offset
            .checked_add(self.compressed_size)
            .map(|end| self.data_offset..end)
    }
}

/// An EPUB ZIP container that can be read, repaired, and opened as a publication.
///
/// Entry changes remain in this value until it is exported; they do not mutate the source ZIP.
/// Duplicate physical entries are collapsed by resource path and are not forensically preserved.
#[derive(Debug)]
pub struct EpubZip<R: Read + Seek> {
    zip: Mutex<ZipArchive<R>>,
    source_entries: SourceEntryIndex,
    source_names: Vec<Vec<u8>>,
    container_state: ContainerState,
    container_dirty: bool,
    repairs: BTreeMap<EpubPath, Option<Vec<u8>>>,
}

#[derive(Debug)]
enum ContainerState {
    Missing,
    Malformed(ContainerDocumentError),
    Parsed(Vec<Rootfile>),
}

impl EpubZip<BufReader<File>> {
    /// Opens the EPUB file at `path` and discovers its container state.
    ///
    /// The returned value owns the file handle. A missing or malformed container document
    /// does not prevent opening a readable ZIP; that state is reported by [`Self::rootfiles`].
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::IoPath`] if the file cannot be opened, or a ZIP error if
    /// the archive itself cannot be read.
    pub fn open(path: impl AsRef<Path>) -> Result<EpubZip<BufReader<File>>> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|err| ContainerError::IoPath {
            source: err,
            path: path.to_path_buf(),
        })?;
        EpubZip::from_reader(BufReader::new(file))
    }
}
impl<R: Read + Seek> ResourceProvider for EpubZip<R> {
    fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> crate::resource::provider::ReadResult<T> {
        if path.as_str() == CONTAINER_PATH && self.container_dirty {
            let bytes = Self::to_string(
                self.rootfiles()
                    .map_err(|source| ProviderReadError::backend(path.clone(), source))?,
            )
            .map_err(|source| ProviderReadError::backend(path.clone(), source))?
            .into_bytes();
            let mut cursor = Cursor::new(bytes);
            return Ok(read(&mut cursor));
        }
        if let Some(repair) = self.repairs.get(path) {
            return match repair {
                Some(bytes) => {
                    let mut cursor = Cursor::new(bytes.as_slice());
                    Ok(read(&mut cursor))
                }
                None => Err(ProviderReadError::MissingResource { path: path.clone() }),
            };
        }
        let mut zip = self
            .zip()
            .map_err(|source| ProviderReadError::backend(path.clone(), source))?;
        if let Some(index) = self.source_index(path) {
            let mut zipfile = zip
                .by_index(index)
                .map_err(ContainerError::from)
                .map_err(|source| ProviderReadError::backend(path.clone(), source))?;
            Ok(read(&mut zipfile))
        } else {
            Err(ProviderReadError::MissingResource { path: path.clone() })
        }
    }
    fn index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
        self.build_container_index(limits)
    }
}

impl<R: Read + Seek> EpubZip<R> {
    /// Opens an EPUB from an owned seekable reader.
    ///
    /// Automatic `META-INF/container.xml` discovery accepts at most 4 MiB and reads up to one
    /// additional byte to detect an over-limit document. A missing or malformed container is
    /// retained as state rather than rejecting an otherwise readable ZIP.
    ///
    /// # Errors
    ///
    /// Returns an error if `reader` is not a readable ZIP archive.
    pub fn from_reader(reader: R) -> Result<Self> {
        let zip = ZipArchive::new(reader)?;
        let central_directory_start = zip.central_directory_start();
        let mut reader = zip.into_inner();
        let (source_entries, source_names) =
            Self::read_source_directory(&mut reader, central_directory_start)?;
        let zip = ZipArchive::new(reader)?;
        if zip.len() != source_names.len() {
            return Err(zip::result::ZipError::InvalidArchive(
                "Source ZIP entry selection did not match the central directory".into(),
            )
            .into());
        }
        let container = EpubZip {
            zip: Mutex::new(zip),
            source_entries,
            source_names,
            container_state: ContainerState::Missing,
            container_dirty: false,
            repairs: BTreeMap::new(),
        };
        Ok(container.with_discovered_container_state())
    }

    fn read_source_directory(reader: &mut R, offset: u64) -> Result<SourceDirectory> {
        use std::io::SeekFrom;

        reader.seek(SeekFrom::Start(offset))?;
        let mut selected_by_zip_name = BTreeMap::<Vec<u8>, usize>::new();
        let mut source_names = Vec::<Vec<u8>>::new();
        let mut selected_ordinals = Vec::<usize>::new();
        let mut all_source_names = BTreeSet::new();
        let mut ordinal = 0usize;
        loop {
            let mut signature = [0u8; 4];
            reader.read_exact(&mut signature)?;
            if signature != [0x50, 0x4b, 0x01, 0x02] {
                break;
            }
            let mut header = [0u8; 42];
            reader.read_exact(&mut header)?;
            let name_len = usize::from(u16::from_le_bytes([header[24], header[25]]));
            let extra_len = usize::from(u16::from_le_bytes([header[26], header[27]]));
            let comment_len = i64::from(u16::from_le_bytes([header[28], header[29]]));
            let mut source_name = vec![0; name_len];
            reader.read_exact(&mut source_name)?;
            let mut extra = vec![0; extra_len];
            reader.read_exact(&mut extra)?;
            reader.seek(SeekFrom::Current(comment_len))?;

            all_source_names.insert(source_name.clone());
            let zip_name = Self::unicode_path_name(&source_name, &extra)
                .unwrap_or_else(|| source_name.clone());
            if let Some(index) = selected_by_zip_name.get(&zip_name).copied() {
                if source_names[index] != source_name {
                    return Err(zip::result::ZipError::InvalidArchive(
                        "Unicode path aliases collide across distinct raw ZIP names".into(),
                    )
                    .into());
                }
                source_names[index] = source_name;
                selected_ordinals[index] = ordinal;
            } else {
                let index = source_names.len();
                selected_by_zip_name.insert(zip_name, index);
                source_names.push(source_name);
                selected_ordinals.push(ordinal);
            }
            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                zip::result::ZipError::InvalidArchive("Too many central directory entries".into())
            })?;
        }

        let selected_source_names = source_names.iter().cloned().collect::<BTreeSet<_>>();
        if selected_source_names != all_source_names {
            return Err(zip::result::ZipError::InvalidArchive(
                "Unicode path aliases collapse distinct raw ZIP names".into(),
            )
            .into());
        }
        let mut by_source_order = selected_ordinals
            .iter()
            .copied()
            .enumerate()
            .collect::<Vec<_>>();
        by_source_order.sort_by_key(|(_, ordinal)| *ordinal);
        let mut source_entries = BTreeMap::new();
        for (index, _) in by_source_order {
            source_entries.insert(source_names[index].clone(), index);
        }
        Ok((source_entries, source_names))
    }

    fn unicode_path_name(source_name: &[u8], extra: &[u8]) -> Option<Vec<u8>> {
        let mut remaining = extra;
        let mut selected = source_name.to_vec();
        let mut changed = false;
        while remaining.len() >= 4 {
            let header_id = u16::from_le_bytes([remaining[0], remaining[1]]);
            let len = usize::from(u16::from_le_bytes([remaining[2], remaining[3]]));
            remaining = &remaining[4..];
            if remaining.len() < len {
                break;
            }
            let field = &remaining[..len];
            if header_id == 0x7075
                && field.len() >= 5
                && u32::from_le_bytes(field[1..5].try_into().ok()?) == Self::zip_crc32(&selected)
            {
                selected = field[5..].to_vec();
                changed = true;
            }
            remaining = &remaining[len..];
        }
        changed.then_some(selected)
    }

    fn zip_crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    fn source_index(&self, path: &EpubPath) -> Option<usize> {
        self.source_entries.get(path.as_str().as_bytes()).copied()
    }

    fn with_discovered_container_state(mut self) -> Self {
        self.container_state =
            match self.with_entry_reader(CONTAINER_PATH, Self::parse_rootfiles_automatically) {
                Ok(rootfiles) => ContainerState::Parsed(rootfiles),
                Err(ContainerError::MissingEntry { .. }) => ContainerState::Missing,
                Err(error) => {
                    ContainerState::Malformed(ContainerDocumentError::from_container_error(error))
                }
            };
        self
    }

    fn parse_rootfiles_automatically(reader: &mut dyn Read) -> Result<Vec<Rootfile>> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_AUTOMATIC_CONTAINER_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_AUTOMATIC_CONTAINER_BYTES {
            return Err(ContainerError::MalformedContainer {
                message: format!(
                    "container document exceeds the automatic parsing limit of {MAX_AUTOMATIC_CONTAINER_BYTES} bytes"
                ),
            });
        }
        Self::parse_rootfiles(Cursor::new(bytes))
    }

    fn zip(&self) -> Result<MutexGuard<'_, ZipArchive<R>>> {
        self.zip.lock().map_err(|_| ContainerError::ZipLockPoisoned)
    }

    fn with_entry_reader<T>(
        &self,
        name: &str,
        read: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<T> {
        let mut zip = self.zip()?;
        if let Some(index) = self.source_entries.get(name.as_bytes()).copied() {
            let mut zipfile = zip.by_index(index)?;
            read(&mut zipfile)
        } else {
            Err(ContainerError::MissingEntry {
                path: Path::new(name).to_path_buf(),
            })
        }
    }

    /// Returns the selected source ZIP entry's physical layout when source bytes back the path.
    ///
    /// Lookup uses the same exact raw UTF-8 name selected by logical provider reads. Missing
    /// entries and entries shadowed by a pending replacement, removal, or generated container
    /// document return `None`. The offsets are absolute to the exact reader supplied to this
    /// value and must not be applied to another archive. Layout metadata does not certify source
    /// bounds or payload integrity; hosts must validate the immutable source before direct reads.
    ///
    /// # Errors
    ///
    /// Returns a ZIP error when the selected entry's local header is malformed or unreadable, or
    /// [`ContainerError::ZipLockPoisoned`] when archive access is unavailable.
    pub fn source_entry_layout(&self, path: &EpubPath) -> Result<Option<EpubZipEntryLayout>> {
        if (path.as_str() == CONTAINER_PATH && self.container_dirty)
            || self.repairs.contains_key(path)
        {
            return Ok(None);
        }

        let mut zip = self.zip()?;
        let Some(index) = self.source_index(path) else {
            return Ok(None);
        };
        let file = zip.by_index_raw(index)?;
        let data_offset = file.data_start().ok_or_else(|| {
            zip::result::ZipError::InvalidArchive("ZIP entry data offset was not resolved".into())
        })?;
        Ok(Some(EpubZipEntryLayout {
            data_offset,
            compressed_size: file.compressed_size(),
            uncompressed_size: file.size(),
            crc32: file.crc32(),
            stored: file.compression() == zip::CompressionMethod::Stored,
            encrypted: file.encrypted(),
        }))
    }

    /// Returns decoded text for the logical `META-INF/container.xml` entry.
    ///
    /// Pending rootfile edits are serialized to normalized OCF XML. Otherwise the source
    /// bytes, including an upserted replacement, are decoded and returned as an owned string.
    ///
    /// # Errors
    ///
    /// Returns an error when the logical entry is missing, cannot be read, cannot be
    /// generated, or has an unsupported or malformed XML encoding.
    pub fn container(&self) -> Result<String> {
        let bytes = self.container_bytes()?;
        decode_xml(&bytes)
            .map(|xml| xml.into_owned())
            .map_err(ContainerError::from)
    }

    /// Returns owned bytes for the logical `META-INF/container.xml` entry.
    ///
    /// Pending rootfile edits produce normalized UTF-8 XML instead of the original bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the logical resource is missing or cannot be read or generated.
    pub fn container_bytes(&self) -> Result<Vec<u8>> {
        let path = EpubPath::new(CONTAINER_PATH).expect("static container path is valid");
        self.read(&path)
            .map_err(|source| ContainerError::ProviderRead { source })
    }

    /// Reads the complete current bytes for an archive entry.
    ///
    /// Pending replacements and removals take precedence over source ZIP entries. This buffers
    /// the entire entry and has no byte limit.
    ///
    /// # Errors
    ///
    /// Returns a provider error if the entry is missing or cannot be read.
    pub fn entry_bytes(&self, path: &EpubPath) -> crate::resource::provider::ReadResult<Vec<u8>> {
        self.read(path)
    }

    /// Adds or replaces an entry for later reads and export.
    ///
    /// The source ZIP is unchanged. Replacing `META-INF/container.xml` reparses its rootfile
    /// semantics while retaining the supplied bytes for provider reads; malformed XML is
    /// retained as container state rather than returned as an error.
    pub fn upsert_entry(&mut self, path: EpubPath, bytes: Vec<u8>) {
        if path.as_str() == CONTAINER_PATH {
            self.container_state = Self::parse_rootfiles(Cursor::new(bytes.as_slice()))
                .map(ContainerState::Parsed)
                .unwrap_or_else(|error| {
                    ContainerState::Malformed(ContainerDocumentError::from_container_error(error))
                });
            self.container_dirty = false;
        }
        self.repairs.insert(path, Some(bytes));
    }

    /// Removes an entry from later reads and export without modifying the source ZIP.
    ///
    /// Removing `META-INF/container.xml` also changes the parsed container state to missing.
    pub fn remove_entry(&mut self, path: EpubPath) {
        if path.as_str() == CONTAINER_PATH {
            self.container_state = ContainerState::Missing;
            self.container_dirty = false;
        }
        self.repairs.insert(path, None);
    }
    /// Parses rootfile semantics from an OCF container document.
    ///
    /// The input is consumed into memory, decoded according to its XML declaration, and
    /// parsed namespace-aware. Unknown elements and attributes are ignored. Recognized
    /// rendition enum attributes retain their authored raw values, but general XML source,
    /// comments, and lexical formatting are not preserved.
    ///
    /// # Errors
    ///
    /// Returns an I/O, decoding, XML, or structural error. There is no input-size limit;
    /// callers handling untrusted data should bound the supplied reader.
    pub fn parse_rootfiles(input: impl std::io::BufRead) -> Result<Vec<Rootfile>> {
        parse_rootfiles(input)
    }

    /// Generates a `container.xml` document for a rootfile list.
    ///
    /// Output is normalized UTF-8 XML. Unknown XML and original formatting are omitted.
    ///
    /// # Errors
    ///
    /// Returns an XML writer error if generation fails.
    pub fn to_string<'a>(rootfiles: impl IntoIterator<Item = &'a Rootfile>) -> Result<String> {
        serialize_rootfiles(rootfiles)
    }
    /// Borrows the discovered or edited rootfiles in source order.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::MissingContainer`] or the retained malformed-container
    /// discovery error when no parsed rootfile list is available.
    pub fn rootfiles(&self) -> Result<&[Rootfile]> {
        match &self.container_state {
            ContainerState::Missing => Err(ContainerError::MissingContainer),
            ContainerState::Malformed(source) => Err(ContainerError::ContainerDocument {
                source: source.clone(),
            }),
            ContainerState::Parsed(rootfiles) => Ok(rootfiles),
        }
    }

    /// Borrows the first rootfile, which defines the default rendition.
    ///
    /// # Errors
    ///
    /// Returns the container-state error from [`Self::rootfiles`] or
    /// [`ContainerError::MissingRootfiles`] when the parsed list is empty.
    pub fn default_rootfile(&self) -> Result<&Rootfile> {
        self.rootfiles()?
            .first()
            .ok_or(ContainerError::MissingRootfiles)
    }

    /// Appends an owned rootfile and marks the container document for normalized generation.
    ///
    /// Missing or malformed discovered container state is replaced by an empty editable list.
    pub fn add_rootfile(&mut self, rootfile: Rootfile) {
        self.editable_rootfiles().push(rootfile);
    }

    /// Removes and returns the rootfile at `index` for later container generation.
    ///
    /// Missing or malformed container state first becomes an empty editable list. Returns
    /// `None` when `index` is out of bounds.
    pub fn remove_rootfile(&mut self, index: usize) -> Option<Rootfile> {
        let rootfiles = self.editable_rootfiles();
        if index < rootfiles.len() {
            Some(rootfiles.remove(index))
        } else {
            None
        }
    }

    /// Removes all rootfiles and marks the container document for normalized generation.
    ///
    /// Missing or malformed container state is replaced by an empty parsed list.
    pub fn clear_rootfiles(&mut self) {
        self.editable_rootfiles().clear();
    }

    fn editable_rootfiles(&mut self) -> &mut Vec<Rootfile> {
        self.container_dirty = true;
        if !matches!(self.container_state, ContainerState::Parsed(_)) {
            self.container_state = ContainerState::Parsed(Vec::new());
        }
        let ContainerState::Parsed(rootfiles) = &mut self.container_state else {
            unreachable!()
        };
        rootfiles
    }
    /// Consumes the container and opens its first rootfile with default limits.
    ///
    /// # Errors
    ///
    /// Returns an [`EpubOpenError`] containing ownership of this provider if container
    /// selection or publication opening fails.
    #[allow(clippy::result_large_err)]
    pub fn default_rendition(self) -> std::result::Result<Epub<Self>, EpubOpenError<Self>> {
        self.default_rendition_with_limits(EpubOpenLimits::default())
    }

    /// Consumes the container and opens its first rootfile with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an [`EpubOpenError`] containing ownership of this provider if selection or
    /// opening fails.
    #[allow(clippy::result_large_err)]
    pub fn default_rendition_with_limits(
        self,
        limits: EpubOpenLimits,
    ) -> std::result::Result<Epub<Self>, EpubOpenError<Self>> {
        self.rendition_with_limits(0, limits)
    }

    /// Consumes the container and opens rootfile `index` with default limits.
    ///
    /// # Errors
    ///
    /// Returns an [`EpubOpenError`] containing ownership of this provider when the index is
    /// unavailable or publication opening fails.
    #[allow(clippy::result_large_err)]
    pub fn rendition(self, index: usize) -> std::result::Result<Epub<Self>, EpubOpenError<Self>> {
        self.rendition_with_limits(index, EpubOpenLimits::default())
    }

    /// Consumes the container and opens rootfile `index` with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an [`EpubOpenError`] containing ownership of this provider when the container
    /// is unavailable, `index` is out of bounds, or publication opening fails.
    #[allow(clippy::result_large_err)]
    pub fn rendition_with_limits(
        self,
        index: usize,
        limits: EpubOpenLimits,
    ) -> std::result::Result<Epub<Self>, EpubOpenError<Self>> {
        let root = match self.rootfiles().and_then(|rootfiles| {
            if rootfiles.is_empty() {
                return Err(ContainerError::MissingRootfiles);
            }
            rootfiles
                .get(index)
                .ok_or(ContainerError::RootfileIndexOutOfBounds {
                    index,
                    len: rootfiles.len(),
                })?
                .package_path()
        }) {
            Ok(root) => root.to_owned(),
            Err(source) => {
                return Err(EpubOpenError::new(
                    EpubOpenFailure::ContainerSelection { source },
                    self,
                ));
            }
        };
        Epub::from_provider_with_limits(self, root, limits)
    }
    /// Lists all physical file and directory entry names in the source ZIP.
    ///
    /// This ignores pending entry changes. Use the [`ResourceProvider`] index for the current
    /// logical resource view.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::ZipLockPoisoned`] if archive access is unavailable.
    pub fn file_names(&self) -> Result<Vec<String>> {
        Ok(self.zip()?.file_names().map(str::to_string).collect())
    }

    fn build_container_index(
        &self,
        limits: &ResourceProviderIndexLimits,
    ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
        let mut zip = self
            .zip()
            .map_err(ResourceProviderIndexError::enumeration)?;
        let mut entries = Vec::new();
        for (name, idx) in &self.source_entries {
            let file = zip
                .by_index_raw(*idx)
                .map_err(ResourceProviderIndexError::enumeration)?;
            debug_assert_eq!(self.source_names.get(*idx), Some(name));
            let name =
                std::str::from_utf8(name).map_err(|_| ResourceProviderIndexError::InvalidPath)?;
            if !name.is_empty() && !name.ends_with('/') {
                let path =
                    EpubPath::new(name).map_err(|_| ResourceProviderIndexError::InvalidPath)?;
                if path.as_str() != name {
                    return Err(ResourceProviderIndexError::InvalidPath);
                }
                if (self.container_dirty && path.as_str() == CONTAINER_PATH)
                    || self.repairs.contains_key(&path)
                {
                    continue;
                }
                entries.push(ResourceProviderEntry::new(path, Some(file.size())));
            }
        }
        for (path, repair) in &self.repairs {
            if let Some(bytes) = repair {
                entries.push(ResourceProviderEntry::new(
                    path.clone(),
                    Some(bytes.len() as u64),
                ));
            }
        }
        if self.container_dirty {
            let path = EpubPath::new(CONTAINER_PATH).expect("static container path is valid");
            let rootfiles = self
                .rootfiles()
                .map_err(ResourceProviderIndexError::enumeration)?;
            let size = Self::to_string(rootfiles)
                .map_err(ResourceProviderIndexError::enumeration)?
                .len() as u64;
            entries.retain(|entry| entry.path() != &path);
            entries.push(ResourceProviderEntry::new(path, Some(size)));
        }
        ResourceProviderIndex::try_from_entries(entries, limits)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufReader;
    use std::io::{Cursor, Write};

    use super::{
        CONTAINER_PATH, ContainerDocumentError, ContainerError, EpubZip, EpubZipEntryLayout,
        MAX_AUTOMATIC_CONTAINER_BYTES, MIMETYPE, RenditionAccessMode, Rootfile,
    };
    use crate::{
        EpubOpenFailure, EpubPath, ResourceSelector,
        package::{EpubVersion, RenditionLayout},
        resource::{
            ResourceLookupError,
            provider::{ResourceProvider, ResourceProviderIndexLimits},
        },
        semantics::EpubString,
    };
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};
    const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
           xmlns:rendition="http://www.idpf.org/2013/rendition"
           version="1.0">
   <rootfiles>
       <rootfile full-path="EPUB/comic/package.opf"
           media-type="application/oebps-package+xml"
           rendition:accessMode="visual"/>
       <rootfile full-path="EPUB/novel/package.opf"
           media-type="application/oebps-package+xml"
           rendition:accessMode="textual"/>
   </rootfiles>
</container>"#;

    fn fixture_epub(include_ncx: bool) -> Cursor<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default();
            let entries = [
                (MIMETYPE, "application/epub+zip"),
                (
                    "META-INF/container.xml",
                    r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml" /></rootfiles></container>"#,
                ),
                (
                    "EPUB/package.opf",
                    r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" /><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml" /></manifest><spine toc="ncx"><itemref idref="chap" /></spine></package>"#,
                ),
                (
                    "EPUB/nav.xhtml",
                    r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
                ),
                ("EPUB/chapter.xhtml", "<html></html>"),
            ];
            for (name, contents) in entries {
                zip.start_file(name, options).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            if include_ncx {
                zip.start_file("EPUB/toc.ncx", options).unwrap();
                zip.write_all(br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap><navPoint id="chapter"><navLabel><text>Chapter</text></navLabel><content src="chapter.xhtml" /></navPoint></navMap></ncx>"#).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        cursor
    }

    fn fixture_archive(directories: &[&str], entries: &[(&str, &str)]) -> Cursor<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default();
            for directory in directories {
                zip.add_directory(*directory, options).unwrap();
            }
            for (name, contents) in entries {
                zip.start_file(*name, options).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        cursor
    }

    fn entry_archive(name: &str, contents: &[u8], options: SimpleFileOptions) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(name, options).unwrap();
            zip.write_all(contents).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn replace_all(bytes: &mut [u8], from: &[u8], to: &[u8]) -> usize {
        assert_eq!(from.len(), to.len());
        let mut replaced = 0;
        let mut start = 0;
        while let Some(offset) = bytes[start..]
            .windows(from.len())
            .position(|candidate| candidate == from)
        {
            let offset = start + offset;
            bytes[offset..offset + from.len()].copy_from_slice(to);
            replaced += 1;
            start = offset + from.len();
        }
        replaced
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    fn add_unicode_path_aliases(bytes: &mut Vec<u8>, aliases: &[&str]) {
        let mut central = bytes
            .windows(4)
            .position(|candidate| candidate == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        let mut added = 0u32;
        for alias in aliases {
            assert_eq!(&bytes[central..central + 4], &[0x50, 0x4b, 0x01, 0x02]);
            let name_len = usize::from(u16::from_le_bytes([
                bytes[central + 28],
                bytes[central + 29],
            ]));
            let extra_len = u16::from_le_bytes([bytes[central + 30], bytes[central + 31]]);
            let comment_len = usize::from(u16::from_le_bytes([
                bytes[central + 32],
                bytes[central + 33],
            ]));
            let source_name = &bytes[central + 46..central + 46 + name_len];
            let mut unicode_path = vec![1];
            unicode_path.extend_from_slice(&crc32(source_name).to_le_bytes());
            unicode_path.extend_from_slice(alias.as_bytes());
            let mut field = 0x7075u16.to_le_bytes().to_vec();
            field.extend_from_slice(&(unicode_path.len() as u16).to_le_bytes());
            field.extend_from_slice(&unicode_path);
            bytes[central + 30..central + 32]
                .copy_from_slice(&(extra_len + field.len() as u16).to_le_bytes());
            bytes.splice(
                central + 46 + name_len..central + 46 + name_len,
                field.iter().copied(),
            );
            added += field.len() as u32;
            central += 46 + name_len + usize::from(extra_len) + field.len() + comment_len;
        }
        let eocd = bytes
            .windows(4)
            .rposition(|candidate| candidate == [0x50, 0x4b, 0x05, 0x06])
            .unwrap();
        let central_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().unwrap());
        bytes[eocd + 12..eocd + 16].copy_from_slice(&(central_size + added).to_le_bytes());
    }

    fn multiple_rendition_epub() -> Cursor<Vec<u8>> {
        fixture_archive(
            &[],
            &[
                (MIMETYPE, "application/epub+zip"),
                ("META-INF/container.xml", CONTAINER_XML),
                (
                    "EPUB/comic/package.opf",
                    r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>Comic</dc:title><dc:identifier id="uid">comic</dc:identifier><dc:language>en</dc:language></metadata><manifest/><spine/></package>"#,
                ),
                (
                    "EPUB/novel/package.opf",
                    r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>Novel</dc:title><dc:identifier id="uid">novel</dc:identifier><dc:language>en</dc:language></metadata><manifest/><spine/></package>"#,
                ),
            ],
        )
    }

    fn fixture_ncx_only_epub() -> Cursor<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default();
            let entries = [
                (MIMETYPE, "application/epub+zip"),
                (
                    "META-INF/container.xml",
                    r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml" /></rootfiles></container>"#,
                ),
                (
                    "EPUB/package.opf",
                    r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid"><metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="ncx" href="nav/toc.ncx" media-type="application/x-dtbncx+xml" /><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml" /></manifest><spine toc="ncx"><itemref idref="chap" /></spine></package>"#,
                ),
                ("EPUB/chapter.xhtml", "<html></html>"),
                (
                    "EPUB/nav/toc.ncx",
                    r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap><navPoint id="chapter"><navLabel><text>Chapter</text></navLabel><content src="../chapter.xhtml" /></navPoint></navMap></ncx>"#,
                ),
            ];
            for (name, contents) in entries {
                zip.start_file(name, options).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        cursor
    }
    #[test]
    fn parse_rootfiles() {
        let rootfiles =
            EpubZip::<BufReader<File>>::parse_rootfiles(CONTAINER_XML.as_bytes()).unwrap();
        let visual_root = Rootfile::new("EPUB/comic/package.opf")
            .unwrap()
            .with_rendition_access_mode(RenditionAccessMode::Visual);
        let textual_root = Rootfile::new("EPUB/novel/package.opf")
            .unwrap()
            .with_rendition_access_mode(RenditionAccessMode::Textual);
        assert_eq!(rootfiles.as_slice(), &[visual_root, textual_root])
    }

    #[test]
    fn rootfile_optional_rendition_value_matrix_preserves_known_raw_state() {
        let xml = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:rendition="http://www.idpf.org/2013/rendition">
            <rootfiles>
                <rootfile full-path="missing.opf"/>
                <rootfile full-path="empty.opf" rendition:media="" rendition:language=""
                    rendition:accessMode="" rendition:layout="" rendition:label=""/>
                <rootfile full-path="whitespace.opf" rendition:media="   " rendition:language="   "
                    rendition:accessMode="   " rendition:layout="   " rendition:label="   "/>
                <rootfile full-path="invalid.opf" rendition:media="not a media query ???"
                    rendition:language="not_a_language" rendition:accessMode="speech"
                    rendition:layout="sideways" rendition:label="Unknown label"/>
                <rootfile full-path="valid.opf" rendition:media="(color)" rendition:language="fr"
                    rendition:accessMode="visual" rendition:layout="pre-paginated"
                    rendition:label="Illustrated"/>
                <rootfile full-path="roll.opf" rendition:layout="roll"/>
            </rootfiles>
        </container>"#;

        let rootfiles = EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(xml.as_bytes()).unwrap();
        let missing = &rootfiles[0];
        assert_eq!(missing.rendition_access_mode_raw(), None);
        assert_eq!(missing.rendition_layout_raw(), None);

        for (rootfile, raw) in [(&rootfiles[1], ""), (&rootfiles[2], "   ")] {
            assert_eq!(rootfile.rendition_access_mode_raw(), Some(raw));
            assert_eq!(rootfile.rendition_layout_raw(), Some(raw));
            assert_eq!(rootfile.rendition_media(), None);
            assert_eq!(rootfile.rendition_language(), None);
            assert_eq!(rootfile.rendition_access_mode(), None);
            assert_eq!(rootfile.rendition_layout(), None);
            assert_eq!(rootfile.rendition_label(), None);
        }

        let invalid = &rootfiles[3];
        assert_eq!(invalid.rendition_access_mode_raw(), Some("speech"));
        assert_eq!(invalid.rendition_layout_raw(), Some("sideways"));
        assert_eq!(invalid.rendition_access_mode(), None);
        assert_eq!(invalid.rendition_layout(), None);

        let valid = &rootfiles[4];
        assert_eq!(
            valid.rendition_media().map(EpubString::as_str),
            Some("(color)")
        );
        assert_eq!(
            valid.rendition_language().map(EpubString::as_str),
            Some("fr")
        );
        assert_eq!(
            valid.rendition_access_mode(),
            Some(RenditionAccessMode::Visual)
        );
        assert_eq!(
            valid.rendition_layout(),
            Some(RenditionLayout::PrePaginated)
        );
        assert_eq!(
            valid.rendition_label().map(EpubString::as_str),
            Some("Illustrated")
        );
        assert_eq!(rootfiles[5].rendition_layout(), Some(RenditionLayout::Roll));

        let generated = EpubZip::<Cursor<Vec<u8>>>::to_string(&rootfiles).unwrap();
        let reparsed = EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(generated.as_bytes()).unwrap();
        assert_eq!(reparsed, rootfiles);
    }

    #[test]
    fn parse_rootfiles_requires_ocf_root_and_ignores_foreign_subtrees() {
        let wrong_root = r#"<container><rootfiles/></container>"#;
        assert!(matches!(
            EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(wrong_root.as_bytes()),
            Err(super::ContainerError::MalformedContainer { .. })
        ));

        let xml = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:f="https://example.com/foreign"><f:extension><rootfiles><rootfile full-path="ignored.opf"/></rootfiles></f:extension><rootfiles><f:rootfile full-path="also-ignored.opf"/><rootfile full-path="EPUB/package.opf"/></rootfiles></container>"#;
        let rootfiles = EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(xml.as_bytes()).unwrap();
        assert_eq!(rootfiles.len(), 1);
        assert_eq!(rootfiles[0].package_path().unwrap(), "EPUB/package.opf");
    }

    #[test]
    fn parse_rootfiles_rejects_incomplete_or_extra_document_content() {
        let container = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"/>"#;
        for (case, xml) in [
            r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>"#,
            &format!("{container}{container}"),
            &format!("{container} trailing"),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(xml.as_bytes()).is_err(),
                "case {case} was accepted"
            );
        }
    }

    #[test]
    fn explicit_rendition_selection_opens_requested_package_and_returns_archive_on_failure() {
        let archive = EpubZip::from_reader(multiple_rendition_epub()).unwrap();
        let novel = archive.rendition(1).unwrap();
        assert_eq!(novel.package_path().as_str(), "EPUB/novel/package.opf");

        let archive = novel.into_base_provider();
        let error = archive.rendition(2).unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::ContainerSelection {
                source: ContainerError::RootfileIndexOutOfBounds { index: 2, len: 2 }
            }
        ));
        assert_eq!(error.into_provider().rootfiles().unwrap().len(), 2);
    }
    #[test]
    fn readable_zip_retains_typed_missing_and_malformed_container_states() {
        for container_xml in [None, Some(b"<broken".as_slice())] {
            let mut cursor = Cursor::new(Vec::new());
            {
                let mut zip = ZipWriter::new(&mut cursor);
                zip.start_file(MIMETYPE, SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(b"application/epub+zip").unwrap();
                if let Some(container_xml) = container_xml {
                    zip.start_file("META-INF/container.xml", SimpleFileOptions::default())
                        .unwrap();
                    zip.write_all(container_xml).unwrap();
                }
                zip.finish().unwrap();
            }
            cursor.set_position(0);

            let archive = EpubZip::from_reader(cursor).unwrap();
            match container_xml {
                Some(_) => assert!(matches!(
                    archive.rootfiles(),
                    Err(ContainerError::ContainerDocument { .. })
                )),
                None => assert!(matches!(
                    archive.rootfiles(),
                    Err(ContainerError::MissingContainer)
                )),
            }
        }
    }

    #[test]
    fn container_discovery_repeats_typed_xml_decode_errors_without_shared_error_storage() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(CONTAINER_PATH, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"<container>\xff</container>").unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let archive = EpubZip::from_reader(cursor).unwrap();

        for error in [
            archive.rootfiles().unwrap_err(),
            archive.rootfiles().unwrap_err(),
        ] {
            assert!(matches!(
                error,
                ContainerError::ContainerDocument {
                    source: ContainerDocumentError::XmlDecode { .. }
                }
            ));
        }
    }

    #[test]
    fn rootfile_edit_generates_the_logical_container_resource() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(MIMETYPE, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"application/epub+zip").unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let mut archive = EpubZip::from_reader(cursor).unwrap();
        archive.add_rootfile(Rootfile::new("EPUB/package.opf").unwrap());

        let container_path = EpubPath::new(CONTAINER_PATH).unwrap();
        assert!(archive.container_bytes().unwrap().starts_with(b"<?xml"));
        assert!(
            archive
                .index(&ResourceProviderIndexLimits::default())
                .unwrap()
                .get(&container_path)
                .is_some()
        );
        assert_eq!(
            EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(
                archive.container_bytes().unwrap().as_slice()
            )
            .unwrap()[0]
                .package_path()
                .unwrap(),
            "EPUB/package.opf"
        );
    }

    #[test]
    fn container_access_preserves_exact_bytes_and_decodes_strictly() {
        let path = EpubPath::new(CONTAINER_PATH).unwrap();
        let mut archive = EpubZip::from_reader(fixture_epub(false)).unwrap();
        let malformed = b"<container>\xff</container>".to_vec();

        archive.upsert_entry(path, malformed.clone());

        assert_eq!(archive.container_bytes().unwrap(), malformed);
        assert!(matches!(
            archive.container(),
            Err(ContainerError::XmlDecode { .. })
        ));
    }

    #[test]
    fn epub_zip_index_enforces_limits_and_rejects_noncanonical_paths() {
        let cont = EpubZip::from_reader(fixture_epub(false)).unwrap();
        let limits = ResourceProviderIndexLimits::new(1, usize::MAX).unwrap();
        assert!(matches!(
            cont.index(&limits),
            Err(
                crate::resource::provider::ResourceProviderIndexError::EntryCountLimitExceeded {
                    limit: 1
                }
            )
        ));

        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file("EPUB/text/../chapter.xhtml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"chapter").unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let archive = EpubZip::from_reader(cursor).unwrap();
        assert!(matches!(
            archive.index(&ResourceProviderIndexLimits::default()),
            Err(crate::resource::provider::ResourceProviderIndexError::InvalidPath)
        ));
    }

    #[test]
    fn automatic_container_discovery_enforces_its_public_size_limit() {
        let oversized = vec![b'x'; (MAX_AUTOMATIC_CONTAINER_BYTES + 1) as usize];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(CONTAINER_PATH, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&oversized).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let archive = EpubZip::from_reader(cursor).unwrap();
        assert!(matches!(
            archive.rootfiles(),
            Err(ContainerError::ContainerDocument { source })
                if matches!(source, ContainerDocumentError::Malformed { .. })
        ));
    }

    #[test]
    fn rootfile_package_path_requires_full_path() {
        let rootfile = Rootfile::new("EPUB/package.opf").unwrap();
        assert_eq!(rootfile.package_path().unwrap(), "EPUB/package.opf");

        let parsed = EpubZip::<Cursor<Vec<u8>>>::parse_rootfiles(
            br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile/></rootfiles></container>"#.as_slice(),
        )
        .unwrap();
        let missing = &parsed[0];
        assert!(matches!(
            missing.package_path().unwrap_err(),
            super::ContainerError::MissingRootfilePath
        ));
    }

    #[test]
    fn default_rendition_returns_archive_on_container_selection_failure() {
        let archive = EpubZip::from_reader(fixture_archive(
            &[],
            &[
                (MIMETYPE, "application/epub+zip"),
                (
                    CONTAINER_PATH,
                    r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles/></container>"#,
                ),
            ],
        ))
        .unwrap();

        let error = archive.default_rendition().unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::ContainerSelection {
                source: ContainerError::MissingRootfiles
            }
        ));
        let archive = error.into_provider();
        assert_eq!(
            archive
                .entry_bytes(&EpubPath::new(MIMETYPE).unwrap())
                .unwrap(),
            b"application/epub+zip"
        );
    }

    #[test]
    fn rendition_selects_epub_nav_without_loading_secondary_ncx() {
        for include_ncx in [false, true] {
            let epub = EpubZip::from_reader(fixture_epub(include_ncx))
                .unwrap()
                .default_rendition()
                .unwrap();

            assert!(epub.navigation().epub_nav().is_some());
            assert!(epub.navigation().ncx().is_none());
        }
    }

    #[test]
    fn normalized_export_preserves_ncx_only_publication_semantics() {
        let epub = EpubZip::from_reader(fixture_ncx_only_epub())
            .unwrap()
            .default_rendition()
            .unwrap();

        let output = epub.export(Cursor::new(Vec::new())).unwrap().into_inner();
        let reopened = EpubZip::from_reader(Cursor::new(output))
            .unwrap()
            .default_rendition()
            .unwrap();
        assert_eq!(reopened.package().version(), Some(EpubVersion::Two));
        assert!(reopened.navigation().epub_nav().is_none());
        assert!(reopened.navigation().ncx().is_some());
        assert!(matches!(
            reopened.resource(ResourceSelector::path("EPUB/nav.xhtml").unwrap()),
            Err(ResourceLookupError::NotFound(_))
        ));
    }

    #[test]
    fn stored_source_layout_identifies_exact_bytes_and_crc() {
        let contents = b"123456789";
        let bytes = entry_archive(
            "EPUB/audio.mp3",
            contents,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        let archive = EpubZip::from_reader(Cursor::new(bytes.clone())).unwrap();
        let layout = archive
            .source_entry_layout(&EpubPath::new("EPUB/audio.mp3").unwrap())
            .unwrap()
            .unwrap();

        assert_eq!(layout.compressed_size(), contents.len() as u64);
        assert_eq!(layout.uncompressed_size(), contents.len() as u64);
        assert_eq!(layout.crc32(), 0xcbf4_3926);
        assert!(layout.is_stored());
        assert!(!layout.is_encrypted());
        let range = layout.stored_data_range().unwrap();
        assert_eq!(&bytes[range.start as usize..range.end as usize], contents);
    }

    #[test]
    fn compressed_and_encrypted_source_layouts_are_not_direct_ranges() {
        let compressed = entry_archive(
            "EPUB/audio.mp3",
            b"compressible audio bytes compressible audio bytes",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        );
        let archive = EpubZip::from_reader(Cursor::new(compressed)).unwrap();
        let layout = archive
            .source_entry_layout(&EpubPath::new("EPUB/audio.mp3").unwrap())
            .unwrap()
            .unwrap();
        assert!(!layout.is_stored());
        assert!(!layout.is_encrypted());
        assert_eq!(layout.stored_data_range(), None);

        let mut encrypted = entry_archive(
            "EPUB/audio.mp3",
            b"encrypted marker fixture",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        for (signature, flag_offset) in [
            (&[0x50, 0x4b, 0x03, 0x04][..], 6),
            (&[0x50, 0x4b, 0x01, 0x02][..], 8),
        ] {
            let header = encrypted
                .windows(signature.len())
                .position(|candidate| candidate == signature)
                .unwrap();
            encrypted[header + flag_offset] |= 1;
        }
        let archive = EpubZip::from_reader(Cursor::new(encrypted)).unwrap();
        let layout = archive
            .source_entry_layout(&EpubPath::new("EPUB/audio.mp3").unwrap())
            .unwrap()
            .unwrap();
        assert!(layout.is_stored());
        assert!(layout.is_encrypted());
        assert_eq!(layout.stored_data_range(), None);
    }

    #[test]
    fn source_layout_is_absent_when_logical_bytes_do_not_use_the_source() {
        let audio = EpubPath::new("EPUB/audio.mp3").unwrap();
        let missing = EpubPath::new("EPUB/missing.mp3").unwrap();
        let mut archive = EpubZip::from_reader(fixture_archive(
            &[],
            &[(CONTAINER_PATH, CONTAINER_XML), (audio.as_str(), "source")],
        ))
        .unwrap();

        assert_eq!(archive.source_entry_layout(&missing).unwrap(), None);
        archive.upsert_entry(audio.clone(), b"replacement".to_vec());
        assert_eq!(archive.source_entry_layout(&audio).unwrap(), None);

        let mut archive = EpubZip::from_reader(fixture_archive(
            &[],
            &[(CONTAINER_PATH, CONTAINER_XML), (audio.as_str(), "source")],
        ))
        .unwrap();
        archive.remove_entry(audio.clone());
        assert_eq!(archive.source_entry_layout(&audio).unwrap(), None);

        let container = EpubPath::new(CONTAINER_PATH).unwrap();
        let mut archive =
            EpubZip::from_reader(fixture_archive(&[], &[(CONTAINER_PATH, CONTAINER_XML)])).unwrap();
        archive.clear_rootfiles();
        assert_eq!(archive.source_entry_layout(&container).unwrap(), None);
    }

    #[test]
    fn source_layout_and_logical_reads_select_the_same_duplicate() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file("EPUB/first.mp3", options).unwrap();
            zip.write_all(b"first").unwrap();
            zip.start_file("EPUB/later.mp3", options).unwrap();
            zip.write_all(b"second").unwrap();
            zip.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        assert_eq!(
            replace_all(&mut bytes, b"EPUB/later.mp3", b"EPUB/first.mp3"),
            2
        );
        let archive = EpubZip::from_reader(Cursor::new(bytes.clone())).unwrap();
        let path = EpubPath::new("EPUB/first.mp3").unwrap();
        assert_eq!(archive.entry_bytes(&path).unwrap(), b"second");
        let range = archive
            .source_entry_layout(&path)
            .unwrap()
            .unwrap()
            .stored_data_range()
            .unwrap();
        assert_eq!(&bytes[range.start as usize..range.end as usize], b"second");
    }

    #[test]
    fn provider_index_rejects_non_utf8_raw_names_without_aliasing_reads() {
        let mut bytes = entry_archive(
            "EPUB/x.mp3",
            b"audio",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        assert_eq!(replace_all(&mut bytes, b"EPUB/x.mp3", b"EPUB/\xff.mp3"), 2);
        let archive = EpubZip::from_reader(Cursor::new(bytes)).unwrap();
        assert!(matches!(
            archive.index(&ResourceProviderIndexLimits::default()),
            Err(crate::resource::provider::ResourceProviderIndexError::InvalidPath)
        ));
        let path = EpubPath::new("EPUB/x.mp3").unwrap();
        assert!(archive.entry_bytes(&path).is_err());
        assert_eq!(archive.source_entry_layout(&path).unwrap(), None);
    }

    #[test]
    fn unicode_path_extra_fields_do_not_override_raw_utf8_identity() {
        let raw_name = b"EPUB/raw.mp3";
        let alias = "EPUB/alias.mp3";
        let mut bytes = entry_archive(
            std::str::from_utf8(raw_name).unwrap(),
            b"audio",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        add_unicode_path_aliases(&mut bytes, &[alias]);

        let archive = EpubZip::from_reader(Cursor::new(bytes)).unwrap();
        let raw_path = EpubPath::new(std::str::from_utf8(raw_name).unwrap()).unwrap();
        let alias_path = EpubPath::new(alias).unwrap();
        assert_eq!(archive.entry_bytes(&raw_path).unwrap(), b"audio");
        assert!(archive.source_entry_layout(&raw_path).unwrap().is_some());
        assert!(archive.entry_bytes(&alias_path).is_err());
        assert_eq!(archive.source_entry_layout(&alias_path).unwrap(), None);
        archive
            .index(&ResourceProviderIndexLimits::default())
            .unwrap();
    }

    #[test]
    fn unicode_alias_collisions_are_rejected_instead_of_dropping_raw_names() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            writer
                .start_file("first", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"first").unwrap();
            writer
                .start_file("later", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"later").unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        add_unicode_path_aliases(&mut bytes, &["alias", "alias"]);
        assert!(matches!(
            EpubZip::from_reader(Cursor::new(bytes)),
            Err(ContainerError::Zip { .. })
        ));
    }

    #[test]
    fn malformed_local_header_keeps_raw_identity_despite_unicode_alias() {
        let mut bytes = entry_archive(
            "raw-name",
            b"audio",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        add_unicode_path_aliases(&mut bytes, &["alias"]);
        bytes[0..4].copy_from_slice(b"nope");
        let archive = EpubZip::from_reader(Cursor::new(bytes)).unwrap();
        assert!(matches!(
            archive.source_entry_layout(&EpubPath::new("raw-name").unwrap()),
            Err(ContainerError::Zip { .. })
        ));
        assert_eq!(
            archive
                .source_entry_layout(&EpubPath::new("alias").unwrap())
                .unwrap(),
            None
        );
    }

    #[test]
    fn empty_zip64_descriptor_and_prepended_entries_report_absolute_ranges() {
        let cases = [
            entry_archive(
                "empty",
                b"",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            ),
            entry_archive(
                "zip64",
                b"zip64",
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .large_file(true),
            ),
            {
                let mut writer = ZipWriter::new_stream(Vec::new());
                writer
                    .start_file(
                        "descriptor",
                        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                    )
                    .unwrap();
                writer.write_all(b"descriptor").unwrap();
                writer.finish().unwrap().into_inner()
            },
            {
                let mut cursor = Cursor::new(b"prepended bytes".to_vec());
                cursor.set_position(cursor.get_ref().len() as u64);
                {
                    let mut writer = ZipWriter::new(&mut cursor);
                    writer
                        .start_file(
                            "prepended",
                            SimpleFileOptions::default()
                                .compression_method(CompressionMethod::Stored),
                        )
                        .unwrap();
                    writer.write_all(b"payload").unwrap();
                    writer.finish().unwrap();
                }
                cursor.into_inner()
            },
        ];
        let expected = [
            ("empty", &b""[..]),
            ("zip64", &b"zip64"[..]),
            ("descriptor", &b"descriptor"[..]),
            ("prepended", &b"payload"[..]),
        ];

        for (bytes, (name, contents)) in cases.into_iter().zip(expected) {
            let archive = EpubZip::from_reader(Cursor::new(bytes.clone())).unwrap();
            let range = archive
                .source_entry_layout(&EpubPath::new(name).unwrap())
                .unwrap()
                .unwrap()
                .stored_data_range()
                .unwrap();
            assert_eq!(&bytes[range.start as usize..range.end as usize], contents);
            if name == "prepended" {
                assert!(range.start >= b"prepended bytes".len() as u64);
            }
        }
    }

    #[test]
    fn corrupt_local_header_returns_a_typed_zip_error() {
        let mut bytes = entry_archive(
            "EPUB/audio.mp3",
            b"audio",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        bytes[0..4].copy_from_slice(b"nope");
        let archive = EpubZip::from_reader(Cursor::new(bytes)).unwrap();
        assert!(matches!(
            archive.source_entry_layout(&EpubPath::new("EPUB/audio.mp3").unwrap()),
            Err(ContainerError::Zip { .. })
        ));
    }

    #[test]
    fn stored_data_range_checks_overflow_and_eligibility() {
        let base = EpubZipEntryLayout {
            data_offset: u64::MAX,
            compressed_size: 1,
            uncompressed_size: 1,
            crc32: 0,
            stored: true,
            encrypted: false,
        };
        assert_eq!(base.stored_data_range(), None);
        assert_eq!(
            EpubZipEntryLayout {
                compressed_size: 0,
                uncompressed_size: 0,
                ..base
            }
            .stored_data_range(),
            Some(u64::MAX..u64::MAX)
        );
    }
}
