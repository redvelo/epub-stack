use crate::{
    Epub, EpubOpenError, EpubOpenFailure, EpubOpenLimits,
    resource::EpubPath,
    resource::provider::{
        ProviderReadError, ResourceProvider, ResourceProviderEntry, ResourceProviderIndex,
        ResourceProviderIndexError, ResourceProviderIndexLimits,
    },
    xml::decode_xml,
};

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
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

/// An EPUB ZIP container that can be read, repaired, and opened as a publication.
///
/// Entry changes remain in this value until it is exported; they do not mutate the source ZIP.
/// Duplicate physical entries are collapsed by resource path and are not forensically preserved.
#[derive(Debug)]
pub struct EpubZip<R: Read + Seek> {
    zip: Mutex<ZipArchive<R>>,
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
        if let Some(index) = zip.index_for_path(path.as_path()) {
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
        let container = EpubZip {
            zip: Mutex::new(zip),
            container_state: ContainerState::Missing,
            container_dirty: false,
            repairs: BTreeMap::new(),
        };
        Ok(container.with_discovered_container_state())
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
        name: impl AsRef<Path>,
        read: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<T> {
        let path = name.as_ref();
        let mut zip = self.zip()?;
        if let Some(index) = zip.index_for_path(path) {
            let mut zipfile = zip.by_index(index)?;
            read(&mut zipfile)
        } else {
            Err(ContainerError::MissingEntry {
                path: path.to_path_buf(),
            })
        }
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
        for idx in 0..zip.len() {
            let file = zip
                .by_index(idx)
                .map_err(ResourceProviderIndexError::enumeration)?;
            let name = file.name().to_string();
            if !name.is_empty() && !name.ends_with('/') {
                let path = EpubPath::new(name.as_str())
                    .map_err(|_| ResourceProviderIndexError::InvalidPath)?;
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
        CONTAINER_PATH, ContainerDocumentError, ContainerError, EpubZip,
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
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;
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
}
