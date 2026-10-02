//! Private committed-session resource overlay.

use crate::publication::EpubOpenLimits;
use crate::resource::{
    EpubPath,
    provider::{ProviderIndex, ProviderIndexError},
};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ResourceChanges {
    entries: BTreeMap<EpubPath, ResourceChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceChange {
    Upsert(Vec<u8>),
    Remove,
}

impl ResourceChanges {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn upsert(&mut self, path: EpubPath, data: Vec<u8>) {
        self.entries.insert(path, ResourceChange::Upsert(data));
    }

    pub(crate) fn remove(&mut self, path: EpubPath) {
        self.entries.insert(path, ResourceChange::Remove);
    }

    pub(crate) fn entries(&self) -> &BTreeMap<EpubPath, ResourceChange> {
        &self.entries
    }

    pub(crate) fn entry(&self, path: &EpubPath) -> Option<Option<&[u8]>> {
        match self.entries.get(path) {
            Some(ResourceChange::Upsert(data)) => Some(Some(data.as_slice())),
            Some(ResourceChange::Remove) => Some(None),
            None => None,
        }
    }

    pub(crate) fn apply_to_index(
        &self,
        base: &ProviderIndex,
        limits: &EpubOpenLimits,
    ) -> Result<ProviderIndex, ProviderIndexError> {
        let mut entries = base
            .entries()
            .iter()
            .map(|entry| (entry.path.clone(), entry.size_bytes))
            .collect::<BTreeMap<_, _>>();
        for (path, change) in &self.entries {
            match change {
                ResourceChange::Upsert(data) => {
                    entries.insert(path.clone(), Some(data.len() as u64));
                }
                ResourceChange::Remove => {
                    entries.remove(path);
                }
            }
        }
        ProviderIndex::build(entries, limits)
    }
}

impl crate::container::ExportOverlay for ResourceChanges {
    fn entry(&self, path: &EpubPath) -> Option<Option<&[u8]>> {
        self.entry(path)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Epub, container::EpubZip, resource::EpubPath};
    use std::io::{Cursor, Read, Write};
    use zip::write::SimpleFileOptions;
    use zip::{ZipArchive, ZipWriter};

    fn ncx_only_edit_epub() -> Epub<EpubZip<Cursor<Vec<u8>>>> {
        let mut data = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut data);
            let options = SimpleFileOptions::default();
            write_zip_entry(&mut zip, "mimetype", "application/epub+zip", options);
            write_zip_entry(
                &mut zip,
                "META-INF/container.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/package.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/toc.ncx",
                r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap></ncx>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/text/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Text</p></body></html>"#,
                options,
            );
            zip.finish().unwrap();
        }
        data.set_position(0);
        EpubZip::from_reader(data)
            .unwrap()
            .default_rendition()
            .unwrap()
    }

    fn write_zip_entry(
        zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
        path: &str,
        contents: &str,
        options: SimpleFileOptions,
    ) {
        zip.start_file(path, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }

    fn zip_entry(data: &[u8], path: &str) -> Option<Vec<u8>> {
        let mut archive = ZipArchive::new(Cursor::new(data)).unwrap();
        let mut file = archive.by_name(path).ok()?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        Some(contents)
    }

    #[test]
    fn no_op_ncx_only_export_preserves_opf_and_ncx_bytes_without_nav() {
        let epub = ncx_only_edit_epub();
        let package_source = epub
            .bytes(&EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap();
        let ncx_source = epub.bytes(&EpubPath::new("EPUB/toc.ncx").unwrap()).unwrap();

        let exported = epub.export(Cursor::new(Vec::new())).unwrap().into_inner();

        assert_eq!(
            zip_entry(&exported, "EPUB/package.opf").unwrap(),
            package_source
        );
        assert_eq!(zip_entry(&exported, "EPUB/toc.ncx").unwrap(), ncx_source);
        assert!(zip_entry(&exported, "EPUB/nav.xhtml").is_none());
    }
}
