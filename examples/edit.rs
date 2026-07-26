use epub_stack::{EpubZip, ResourceSelector};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: edit <book.epub> <output.epub> <resource-path>")?;
    let output = args
        .next()
        .ok_or("usage: edit <book.epub> <output.epub> <resource-path>")?;
    let resource_path = args
        .next()
        .ok_or("usage: edit <book.epub> <output.epub> <resource-path>")?;
    let zip = EpubZip::open(input)?;
    let mut book = zip.default_rendition()?;

    let preview = book
        .edit()
        .replace_resource(
            ResourceSelector::path(resource_path).ok_or("invalid EPUB resource path")?,
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Updated</body></html>".to_vec(),
        )?
        .preview()?;
    println!("staged resources: {}", preview.resources().len());
    println!("staged changes: {}", preview.changes().len());
    preview.commit();
    book.export_to_path(output)?;
    Ok(())
}
