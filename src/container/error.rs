use std::path::PathBuf;

use crate::{
    resource::EpubPath,
    resource::provider::{ProviderReadError, ResourceProviderIndexError},
};

/// An error decoding XML bytes used by an OCF container document.
///
/// This re-export keeps container decoding failures available without exposing the
/// crate's internal XML module through the container API.
pub use crate::xml::XmlDecodeError as ContainerXmlDecodeError;

#[derive(Debug, Clone, thiserror::Error)]
/// Failure while discovering or parsing `META-INF/container.xml`.
///
/// The error owns any message or source needed to retain the discovery failure after
/// the archive lock has been released.
pub enum ContainerDocumentError {
    /// The decoded document did not have the required OCF structure.
    #[error("Malformed container document: {message}")]
    Malformed {
        /// A description of the structural problem.
        message: String,
    },
    /// The container bytes could not be decoded according to their XML encoding.
    #[error("Could not decode container XML: {source}")]
    XmlDecode {
        /// The XML encoding failure.
        source: ContainerXmlDecodeError,
    },
    /// The decoded XML was not well formed.
    #[error("Could not parse container XML: {source}")]
    Xml {
        /// The XML parser failure.
        source: quick_xml::Error,
    },
    /// Reading the container entry failed.
    #[error("Could not read container document ({kind:?}): {message}")]
    Read {
        /// The portable category of the I/O failure.
        kind: std::io::ErrorKind,
        /// The owned original error message.
        message: String,
    },
    /// Accessing the container entry through the ZIP archive failed.
    #[error("Could not access container document in the ZIP archive: {message}")]
    Archive {
        /// The owned archive error message.
        message: String,
    },
}

impl ContainerDocumentError {
    pub(super) fn from_container_error(error: ContainerError) -> Self {
        match error {
            ContainerError::MalformedContainer { message } => Self::Malformed { message },
            ContainerError::XmlDecode { source } => Self::XmlDecode { source },
            ContainerError::Xml { source } => Self::Xml { source },
            ContainerError::Io { source } => Self::Read {
                kind: source.kind(),
                message: source.to_string(),
            },
            ContainerError::IoPath { path, source } => Self::Read {
                kind: source.kind(),
                message: format!("{}: {source}", path.display()),
            },
            ContainerError::Zip { source } => Self::Archive {
                message: source.to_string(),
            },
            ContainerError::ZipLockPoisoned => Self::Archive {
                message: "ZIP archive lock poisoned".to_string(),
            },
            ContainerError::ContainerDocument { source } => source,
            error => Self::Malformed {
                message: error.to_string(),
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure reading, selecting, or rewriting an OCF container.
pub enum ContainerError {
    /// The requested logical ZIP entry does not exist.
    #[error("ZIP entry not found: {path}")]
    MissingEntry {
        /// The requested archive-relative path.
        path: PathBuf,
    },
    /// The container has no rootfile from which to select a rendition.
    #[error("Missing rootfiles in container")]
    MissingRootfiles,
    /// The archive has no logical `META-INF/container.xml` entry.
    #[error("META-INF/container.xml is missing")]
    MissingContainer,
    /// The container document violates required OCF document structure.
    #[error("Malformed META-INF/container.xml: {message}")]
    MalformedContainer {
        /// A description of the structural problem.
        message: String,
    },
    /// Automatic container discovery retained a malformed-document failure.
    #[error("Could not discover META-INF/container.xml: {source}")]
    ContainerDocument {
        /// The retained discovery or parsing failure.
        source: ContainerDocumentError,
    },
    /// A programmatically supplied required field was empty or whitespace-only.
    #[error("OCF container field is empty: {field}")]
    EmptyField {
        /// The name of the rejected field.
        field: &'static str,
    },
    /// A parsed rootfile has no usable `full-path` attribute.
    #[error("Rootfile missing full-path")]
    MissingRootfilePath,
    /// The requested rendition index is outside the rootfile list.
    #[error("Rootfile index {index} is out of bounds for {len} renditions")]
    RootfileIndexOutOfBounds {
        /// The requested zero-based index.
        index: usize,
        /// The number of available rootfiles.
        len: usize,
    },
    /// An unassociated I/O operation failed.
    #[error("IO error: {source}")]
    Io {
        #[from]
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// Opening or reading a named filesystem path failed.
    #[error("IO error for {path}: {source}")]
    IoPath {
        /// The underlying I/O failure.
        source: std::io::Error,
        /// The owned filesystem path associated with the failure.
        path: PathBuf,
    },
    /// Reading a logical resource through the provider view failed.
    #[error("Could not read the logical container resource: {source}")]
    ProviderRead {
        /// The provider failure.
        source: ProviderReadError,
    },
    /// Opening or accessing the ZIP archive failed.
    #[error("ZIP error: {source}")]
    Zip {
        #[from]
        /// The underlying ZIP failure.
        source: zip::result::ZipError,
    },
    /// Another thread panicked while holding the archive mutex.
    #[error("ZIP archive lock poisoned")]
    ZipLockPoisoned,
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
        source: ContainerXmlDecodeError,
    },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Failure while writing a normalized EPUB ZIP from a resource provider.
pub enum ExportError {
    /// Resource enumeration failed before export.
    #[error("Could not index resources for export: {source}")]
    ProviderIndex {
        /// The provider indexing failure.
        source: ResourceProviderIndexError,
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
