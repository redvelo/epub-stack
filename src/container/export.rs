use std::io::{Seek, Write};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::ExportError;
use crate::resource::{
    EpubPath,
    provider::{ProviderIndex, ResourceProvider},
};

const MIMETYPE: &str = "mimetype";

pub(crate) trait ExportOverlay {
    fn entry(&self, path: &EpubPath) -> Option<Option<&[u8]>>;
}

pub(crate) fn export_provider<R: ResourceProvider, O: ExportOverlay, W: Write + Seek>(
    provider: &R,
    index: &ProviderIndex,
    changes: &O,
    writer: W,
) -> std::result::Result<W, ExportError> {
    let mut zip = ZipWriter::new(writer);
    zip.start_file(
        MIMETYPE,
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )?;
    zip.write_all(b"application/epub+zip")
        .map_err(|source| ExportError::OutputStream { source })?;

    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for entry in index.entries() {
        let path = &entry.path;
        if path.as_str() == MIMETYPE {
            continue;
        }
        match changes.entry(path) {
            Some(Some(bytes)) => {
                zip.start_file(path.as_str(), options)?;
                zip.write_all(bytes)
                    .map_err(|source| ExportError::OutputStream { source })?;
            }
            Some(None) => continue,
            None => {
                zip.start_file(path.as_str(), options)?;
                provider
                    .read_with(path, |reader| {
                        let mut buffer = [0; 8 * 1024];
                        loop {
                            let read = reader.read(&mut buffer).map_err(|source| {
                                ExportError::ProviderStream {
                                    path: path.clone(),
                                    source,
                                }
                            })?;
                            if read == 0 {
                                return Ok::<(), ExportError>(());
                            }
                            zip.write_all(&buffer[..read])
                                .map_err(|source| ExportError::OutputStream { source })?;
                        }
                    })
                    .map_err(|source| ExportError::ProviderRead {
                        path: path.clone(),
                        source,
                    })??;
            }
        }
    }

    Ok(zip.finish()?)
}
