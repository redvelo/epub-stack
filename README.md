# epub-stack

[![Crates.io](https://img.shields.io/crates/v/epub-stack.svg)](https://crates.io/crates/epub-stack)
[![Documentation](https://img.shields.io/docsrs/epub-stack)](https://docs.rs/epub-stack)
[![CI](https://github.com/redvelo/epub-stack/actions/workflows/ci.yml/badge.svg)](https://github.com/redvelo/epub-stack/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/epub-stack.svg)](https://github.com/redvelo/epub-stack#license)

`epub-stack` is a toolkit for the next generation of EPUB applications. It supports
parsing, analysis, inspection, editing, and other application-oriented workflows.

## Installation

```toml
[dependencies]
epub-stack = "0.1"
```

## Why `epub-stack`

`epub-stack` is more than a package and metadata parser. It provides typed EPUB
models, exact resource access, policy-free analysis, source-aware editing, and
relationship queries for application workflows such as:

- Find dependencies, broken references, and structures affected by an edit.
- Extract text, media, overlays, accessibility evidence, and byte-level details.
- Distinguish complete results from partial or unavailable analysis.

Built for EPUB 3.4, it also accepts older EPUB content, including EPUB 2 and
NCX, and preserves unknown EPUB and XML data where practical.

## Run The Examples

This repository includes an Alice's Adventures in Wonderland EPUB for reproducible
example and integration-test runs:

```text
cargo run --example inventory -- fixtures/real/alice-in-wonderland.epub
cargo run --example analysis -- fixtures/real/alice-in-wonderland.epub
cargo run --example edit -- fixtures/real/alice-in-wonderland.epub target/alice-edited.epub epub/text/chapter-1.xhtml
```

The example programs also accept other EPUB files. The Alice publication has
[separate rights terms](https://github.com/redvelo/epub-stack/blob/main/fixtures/real/alice-in-wonderland.NOTICE.md)
and is excluded from the crate published to crates.io.

## Read Resources

```rust
use epub_stack::{EpubZip, ResourceSelector};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = EpubZip::open("book.epub")?.default_rendition()?;

    for entry in book.reading_order() {
        println!("{:?}", entry.target());
    }

    let selector = ResourceSelector::path("EPUB/text/chapter.xhtml")
        .ok_or("invalid EPUB resource path")?;
    let chapter = book.resource(selector)?;

    println!("{} bytes", chapter.bytes()?.len());
    println!("{}", chapter.utf8_text()?);
    Ok(())
}
```

`bytes` and `utf8_text` buffer the complete resource. Use `Resource::read_with` for
callback-scoped streaming.

## Understand A Publication

Opening answers what the package declares. `Epub::analyze` reads eligible resources
and builds a detached `PublicationAnalysis` snapshot.

```rust
use epub_stack::EpubZip;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let book = EpubZip::open("book.epub")?.default_rendition()?;
    let analysis = book.analyze();

    println!("broken references: {}", analysis.broken_references().count());
    println!("searchable chunks: {}", analysis.search_entries().count());
    println!("analysis complete: {}", analysis.coverage().content().is_complete());

    Ok(())
}
```

Snapshots connect search text, authored content, byte inspection, references,
dependencies, fingerprints, media overlays, and accessibility evidence to physical
resources and their publication occurrences. Content facts describe authored uses;
inspection describes the underlying bytes.

For an application-oriented report on another publication, run:

```text
cargo run --example analysis -- book.epub
```

Use `AnalysisLimits` to bound the work. Coverage distinguishes complete, partial, and
unavailable results; check it before treating an empty query as conclusive. Snapshots
do not change after edits, so rerun `analyze` for current results.

## Edit And Save

Stage changes with `edit`, inspect them with `preview`, and apply them to the in-memory
publication with `commit`. Use `export_to_path` or `export` to save the result.

```rust
use epub_stack::{EpubZip, ResourceSelector};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut book = EpubZip::open("book.epub")?.default_rendition()?;
    let chapter = ResourceSelector::path("EPUB/text/chapter.xhtml")
        .ok_or("invalid EPUB resource path")?;

    let preview = book
        .edit()
        .replace_resource(
            chapter,
            b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>Updated</body></html>"
                .to_vec(),
        )?
        .preview()?;

    println!("changes ready to commit: {}", preview.changes().len());
    let report = preview.commit();
    println!("committed changes: {}", report.changes().len());

    book.export_to_path("updated.epub")?;
    Ok(())
}
```

Export preserves unchanged resource payloads but normalizes the archive and regenerated
XML. It is not a byte-for-byte round trip.

## Other Capabilities

`Epub::create` initializes an in-memory EPUB 3 publication. The editing API adds its
resources, manifest declarations, reading order, and navigation before export.

`Cfi` and `CfiRange` parse EPUB CFI syntax and resolve locations against the current
publication. Web Annotation types support parsing, serialization, and targets that can
be resolved without browser layout.

## Provide A Publication

`EpubZip` is one way to supply a publication. `ResourceProvider` supports databases,
object stores, browser caches, unpacked directories, generated publications, and other
application-controlled storage. `MemoryResourceProvider` handles in-memory inputs.

`Epub::from_provider` opens a known OPF package path. If opening fails,
`EpubOpenError` returns ownership of the provider through
`into_provider` or `into_parts`, allowing applications to repair resources and retry.

## Non-Goals

`epub-stack` does not provide:

- HTML/CSS rendering, browser layout, pagination, or measurement.
- a DOM, visual highlights, or a reading-system user interface.
- EPUB validation policy or an EPUBCheck replacement.
- byte-for-byte ZIP round trips.

`epub-navigator` will be the canonical web-based reading runtime built on this crate.

See `examples/` for additional workflows.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT), at your option.
