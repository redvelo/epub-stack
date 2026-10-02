//! Open EPUB ZIP containers, inspect or repair their entries, and select a rendition.
//!
//! Use [`EpubZip`] for ordinary EPUB files and seekable ZIP readers. Repairs affect subsequent
//! reads and export but do not modify the source archive.

mod document;
mod epub_zip;
mod error;
mod export;

pub use document::{
    RenditionAccessMode, RenditionAccessModeToken, RenditionLayoutToken, Rootfile, RootfilePath,
};
pub use epub_zip::{EpubZip, EpubZipEntryLayout};
pub use error::{
    ContainerDocumentError, ContainerError, ContainerStructure, EpubZipError, ExportError,
};
pub(crate) use export::{ExportOverlay, export_provider};

/// Parses rootfile declarations from an OCF container document.
///
/// Buffers the input and decodes its XML encoding. Unknown XML and source formatting are
/// discarded; modeled rootfile attributes retain their authored values.
///
/// # Errors
///
/// Returns [`ContainerDocumentError`] for read, decoding, XML, or structural failures.
pub fn parse_rootfiles(
    input: impl std::io::BufRead,
) -> Result<Vec<Rootfile>, ContainerDocumentError> {
    document::parse_rootfiles(input)
}

/// Generates a normalized `container.xml` document for a rootfile list.
///
/// Unknown XML and original formatting are omitted.
pub fn rootfiles_to_xml<'a>(rootfiles: impl IntoIterator<Item = &'a Rootfile>) -> String {
    document::serialize_rootfiles(rootfiles)
}
