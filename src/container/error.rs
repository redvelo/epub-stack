use std::path::PathBuf;

use crate::{
    resource::EpubPath,
    resource::provider::{ProviderIndexError, ProviderReadError},
    xml::XmlDecodeError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// The way a container document violates required OCF structure.
pub enum ContainerStructure {
    /// The document has no element content.
    #[error("missing document element")]
    MissingDocumentElement,
    /// The document element is not `{{urn:oasis:names:tc:opendocument:xmlns:container}}container`.
    #[error("expected an OCF container document element")]
    UnexpectedDocumentElement,
    /// More than one document element was present.
    #[error("multiple document elements")]
    MultipleDocumentElements,
    /// Markup or non-whitespace text appeared outside the document element.
    #[error("content outside the document element")]
    ContentOutsideDocumentElement,
    /// The document element was never closed.
    #[error("truncated container document")]
    Truncated,
}

#[derive(Debug, Clone, thiserror::Error)]
/// Failure while discovering or parsing `META-INF/container.xml`.
pub enum ContainerDocumentError {
    /// The archive has no logical `META-INF/container.xml` entry.
    #[error("META-INF/container.xml is missing")]
    Missing,
    /// The container document exceeded the automatic parsing byte limit.
    #[error("Container document exceeds the automatic parsing limit of {limit} bytes")]
    TooLarge {
        /// The applied maximum number of bytes.
        limit: u64,
    },
    /// The decoded document did not have the required OCF structure.
    #[error("Malformed container document: {structure}")]
    Structure {
        /// The structural problem.
        structure: ContainerStructure,
    },
    /// The container bytes could not be decoded according to their XML encoding.
    #[error("Could not decode container XML: {source}")]
    Decode {
        /// The XML encoding failure.
        source: XmlDecodeError,
    },
    /// The decoded XML was not well formed.
    #[error("Could not parse container XML: {source}")]
    Xml {
        /// The XML parser failure.
        source: quick_xml::Error,
    },
    /// Reading the container entry from the archive failed.
    #[error("Could not read the container document: {source}")]
    Read {
        /// The archive access failure.
        source: EpubZipError,
    },
}

#[derive(Debug, Clone, thiserror::Error)]
/// Failure accessing the source ZIP archive.
pub enum EpubZipError {
    /// An I/O operation failed.
    #[error("IO error{}: {message}", .path.as_ref().map(|path| format!(" for {}", path.display())).unwrap_or_default())]
    Io {
        /// The portable category of the I/O failure.
        kind: std::io::ErrorKind,
        /// The owned original error message.
        message: String,
        /// The filesystem path associated with the failure, when there is one.
        path: Option<PathBuf>,
    },
    /// Opening or accessing the ZIP archive failed.
    #[error("ZIP error: {message}")]
    Zip {
        /// The owned archive error message.
        message: String,
    },
    /// Another thread panicked while holding the archive mutex.
    #[error("ZIP archive lock poisoned")]
    LockPoisoned,
}

impl EpubZipError {
    pub(super) fn io(source: std::io::Error) -> Self {
        Self::Io {
            kind: source.kind(),
            message: source.to_string(),
            path: None,
        }
    }

    pub(super) fn io_path(source: std::io::Error, path: PathBuf) -> Self {
        Self::Io {
            kind: source.kind(),
            message: source.to_string(),
            path: Some(path),
        }
    }
}

impl From<std::io::Error> for EpubZipError {
    fn from(source: std::io::Error) -> Self {
        Self::io(source)
    }
}

impl From<zip::result::ZipError> for EpubZipError {
    fn from(source: zip::result::ZipError) -> Self {
        Self::Zip {
            message: source.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure reading, selecting, or rewriting an OCF container.
pub enum ContainerError {
    /// The requested logical ZIP entry does not exist.
    #[error("ZIP entry not found: {path}")]
    MissingEntry {
        /// The requested canonical path.
        path: EpubPath,
    },
    /// Discovering or parsing the container document failed.
    #[error("Could not use META-INF/container.xml: {source}")]
    ContainerDocument {
        #[from]
        /// The retained discovery or parsing failure.
        source: ContainerDocumentError,
    },
    /// Accessing the source archive failed.
    #[error("Could not access the ZIP archive: {source}")]
    Archive {
        #[from]
        /// The archive failure.
        source: EpubZipError,
    },
    /// Reading a logical resource through the provider view failed.
    #[error("Could not read the logical container resource: {source}")]
    ProviderRead {
        /// The provider failure.
        source: ProviderReadError,
    },
    /// Parsing or generating container XML failed.
    #[error("XML error: {source}")]
    Xml {
        #[from]
        /// The underlying XML failure.
        source: quick_xml::Error,
    },
    /// XML byte decoding failed.
    #[error("Could not decode container XML: {source}")]
    XmlDecode {
        #[from]
        /// The underlying XML encoding failure.
        source: XmlDecodeError,
    },
}

impl From<std::io::Error> for ContainerError {
    fn from(source: std::io::Error) -> Self {
        Self::Archive {
            source: EpubZipError::io(source),
        }
    }
}

impl From<zip::result::ZipError> for ContainerError {
    fn from(source: zip::result::ZipError) -> Self {
        Self::Archive {
            source: EpubZipError::from(source),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Failure while writing a normalized EPUB ZIP from a resource provider.
pub enum ExportError {
    /// Resource enumeration failed before export.
    #[error("Could not index resources for export: {source}")]
    ProviderIndex {
        /// The provider indexing failure.
        source: ProviderIndexError,
    },
    /// Opening a resource stream through the provider failed.
    #[error("Could not read resource {path} for export: {source}")]
    ProviderRead {
        /// The logical resource path being exported.
        path: EpubPath,
        /// The provider read failure.
        source: ProviderReadError,
    },
    /// Creating the destination file failed.
    #[error("Could not create export output at {path}: {source}")]
    OutputPath {
        /// The destination filesystem path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// Streaming bytes from an opened provider resource failed.
    #[error("Could not stream resource {path} from the provider: {source}")]
    ProviderStream {
        /// The logical resource path being streamed.
        path: EpubPath,
        /// The underlying stream failure.
        source: std::io::Error,
    },
    /// Writing bytes to the destination failed.
    #[error("Could not write export output: {source}")]
    OutputStream {
        /// The underlying output failure.
        source: std::io::Error,
    },
    /// Constructing or finalizing the output ZIP failed.
    #[error("ZIP export failed: {source}")]
    Zip {
        #[from]
        /// The underlying ZIP failure.
        source: zip::result::ZipError,
    },
}
