# epub-stack

[![Crates.io](https://img.shields.io/crates/v/epub-stack.svg)](https://crates.io/crates/epub-stack)
[![Documentation](https://img.shields.io/docsrs/epub-stack)](https://docs.rs/epub-stack)
[![CI](https://github.com/redvelo/epub-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/redvelo/epub-stack/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/epub-stack.svg)](https://github.com/redvelo/epub-stack#license)

`epub-stack` is a Rust toolkit for the next generation of EPUB applications. Build reading
systems, ebook management tools, annotation sync servers and editors on typed models.

## Installation

```toml
[dependencies]
epub-stack = "0.2"
```

## Why `epub-stack`

- EPUB as typed models: metadata, navigation, resources, media overlays, CFI and annotations modeled on EPUB 3.4
- Analyze content: extract text for search and narration, inspect images and media, and link SMIL narration to its text.
- Follow references and edit: find broken links, trace resource dependencies and preview changes before committing.

## Run The Examples

### List publication resources

```sh
cargo run --example inventory -- fixtures/real/alice-in-wonderland.epub
```

### Inspect content and relationships

```sh
cargo run --example analysis -- fixtures/real/alice-in-wonderland.epub
```

### Edit a chapter and export a new EPUB

```sh
cargo run --example edit -- fixtures/real/alice-in-wonderland.epub target/alice-edited.epub epub/text/chapter-1.xhtml
```

The examples also accept any EPUB file.

## Read Chapters

Read documents in spine order, or open one by its path.

```rust,no_run
use epub_stack::{EpubPath, EpubZip};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = EpubZip::open("book.epub")?.default_rendition()?;

    for entry in book.resources().reading_order() {
        let Some(path) = entry.resource().and_then(|resource| resource.local_path()) else {
            continue;
        };
        println!("{}", book.utf8_text(path)?);
    }

    let chapter = EpubPath::new("EPUB/text/chapter.xhtml")?;
    println!("{}", book.utf8_text(&chapter)?);
    Ok(())
}
```

Use `bytes()` for binary resources such as media and fonts.
Use `read_with()` for streaming reads.

## Find Broken Links

Find links to missing resources or fragments.

```rust,no_run
use epub_stack::{EpubZip, analysis::reference::AuthoredReference};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = EpubZip::open("book.epub")?.default_rendition()?;
    let analysis = book.analyze();

    for reference in analysis.broken_references() {
        if let AuthoredReference::Href(reference) = reference
            && let Some(source) = analysis.resources().resource(reference.source())
        {
            println!("{} → {}", source.address().display_value(), reference.declared());
        }
    }

    Ok(())
}
```

Analysis can be incomplete when resources are missing or remote, formats are unsupported, or byte
limits are reached. `coverage()` reports why individual results are partial or unavailable.

## Edit And Save

Replace a resource, review the changes, and export the book.

```rust,no_run
use epub_stack::{EpubPath, EpubZip};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut book = EpubZip::open("book.epub")?.default_rendition()?;
    let chapter = EpubPath::new("EPUB/text/chapter.xhtml")?;

    let preview = book
        .edit()
        .upsert_resource(
            chapter,
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Updated</body></html>"
                .to_vec(),
        )?
        .preview()?;

    println!("{:#?}", preview.changes());
    let applied = preview.commit();
    println!("committed {} changes", applied.len());

    book.export_to_path("updated.epub")?;
    Ok(())
}
```

Export preserves unchanged resource payloads but normalizes the archive and regenerated
XML with best effort preservation.

## Other Capabilities

Read [metadata](https://docs.rs/epub-stack/latest/epub_stack/package/metadata/),
[navigation](https://docs.rs/epub-stack/latest/epub_stack/navigation/),
[document text](https://docs.rs/epub-stack/latest/epub_stack/content/text/) and
[image and media details](https://docs.rs/epub-stack/latest/epub_stack/analysis/inspection/);
each module page opens with a worked example.

`Epub::create` initializes an in-memory EPUB 3 publication. The editing API adds its
resources, manifest declarations, reading order, and navigation before export.

`Cfi` parses EPUB CFI points and ranges and resolves locations against the current
publication. Web Annotation types support parsing, serialization, and targets that can
be resolved without browser layout.

## Provide A Publication

`ResourceProvider` lets you open books from your own storage, including databases,
browser caches, and unpacked directories. Use `MemoryResourceProvider` for in-memory
files, or implement the trait for another backend.

Open the publication with `Epub::from_provider`, supplying its OPF package path.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT), at your option.
