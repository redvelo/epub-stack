use epub_stack::EpubZip;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: inventory <book.epub>")?;
    let zip = EpubZip::open(path)?;
    let book = zip.default_rendition()?;

    for resource in book.resources().resources() {
        println!("{:?} {:?}", resource.address(), resource.presence());
    }
    Ok(())
}
