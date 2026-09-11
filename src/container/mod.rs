//! Open EPUB ZIP containers, inspect or repair their entries, and select a rendition.
//!
//! Use [`EpubZip`] for ordinary EPUB files and seekable ZIP readers. Repairs affect subsequent
//! reads and export but do not modify the source archive. Duplicate physical ZIP entries are
//! exposed as one logical resource and cannot be preserved for forensic round trips.

mod document;
mod epub_zip;
mod error;
mod export;

pub use document::{RenditionAccessMode, Rootfile};
pub use epub_zip::{EpubZip, EpubZipEntryLayout};
pub use error::{ContainerDocumentError, ContainerError, ContainerXmlDecodeError, ExportError};
pub(crate) use export::{ExportOverlay, export_provider};
