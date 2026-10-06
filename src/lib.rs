//! A Rust toolkit for the next generation of EPUB applications, with typed models oriented to EPUB 3.4.
//!
//! # Quick start
//!
//! Open a book and read a chapter.
//!
//! ```
//! use epub_stack::{EpubPath, EpubZip};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//!
//! let chapter = EpubPath::new("epub/text/chapter-1.xhtml")?;
//! let xhtml = book.utf8_text(&chapter)?;
//!
//! assert!(xhtml.contains("Down the Rabbit-Hole"));
//! # Ok(())
//! # }
//! ```
//!
//! Walk the reading order, in the order a reader would meet each document.
//!
//! ```
//! use epub_stack::EpubZip;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//!
//! for entry in book.resources().reading_order() {
//!     let Some(path) = entry.resource().and_then(|resource| resource.local_path()) else {
//!         continue;
//!     };
//!     println!("{path}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Analyze the book and check whether anything it links to is missing.
//!
//! ```
//! use epub_stack::EpubZip;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//! let analysis = book.analyze();
//!
//! let broken = analysis.broken_references().count();
//! assert_eq!(broken, 0);
//!
//! // A negative answer is only as good as the analysis behind it.
//! assert!(analysis.coverage().is_complete());
//! # Ok(())
//! # }
//! ```
//!
//! Replace a chapter and write the result out as a new EPUB.
//!
//! ```
//! use epub_stack::{EpubPath, EpubZip};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//!
//! let chapter = EpubPath::new("epub/text/chapter-1.xhtml")?;
//! let replacement =
//!     br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Hi.</p></body></html>"#;
//!
//! let changes = book
//!     .edit()
//!     .upsert_resource(chapter.clone(), replacement.to_vec())?
//!     .preview()?
//!     .commit();
//!
//! assert_eq!(changes.len(), 1);
//! assert!(book.utf8_text(&chapter)?.contains("Hi."));
//!
//! let mut epub = std::io::Cursor::new(Vec::new());
//! book.export(&mut epub)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Reading and analysis
//!
//! - [`EpubZip`] opens an EPUB file and selects a rendition.
//! - [`Epub::package`] exposes [metadata](package::metadata::Metadata), manifest, and spine.
//! - [`Epub::navigation`] provides the selected [navigation document](navigation::NavigationDocument)
//!   with its table of contents, page list, and landmarks.
//! - [`ResourceIndex`] locates publication resources and walks reading order.
//! - [`Epub::bytes`], [`Epub::utf8_text`], and [`Epub::read_with`] read current resource bytes.
//! - [`Epub::analyze`] connects extracted content, [byte inspection](analysis::inspection),
//!   [references](analysis::reference), and [edit impact](analysis::impact).
//! - [`content::text`] provides source-order text, semantic spans, and supplementary text.
//! - [`ResourceProvider`] supplies publication bytes when a filesystem EPUB is not available.
//!
//! Analysis results describe the version that was analyzed; rerun [`Epub::analyze`] after edits.
//! For package and navigation structure without content analysis, read [`Epub::package`],
//! [`Epub::navigation`] and [`Epub::resources`] directly; each is serializable on its own.
//!
//! # Edit and export
//!
//! 1. Start with [`Epub::edit`] and stage changes.
//! 2. Call [`EpubEdit::preview`] and inspect the proposed resources and changes.
//! 3. Call [`commit`](edit::EpubEditPreview::commit) to update the open publication and list the
//!    applied [changes](edit::EditChange).
//! 4. Call [`Epub::export`] or [`Epub::export_to_path`] to write the committed publication.
//!
//! Preview and commit do not write an EPUB file. Export writes a normalized EPUB ZIP: it preserves
//! unchanged publication resource payloads, except for the canonical `mimetype` entry, but not ZIP
//! byte identity.
//!
//! # More tasks
//!
//! - Create a minimal publication with [`Epub::create`].
//! - Parse locations with [`Cfi`], then resolve them with [`Epub::resolve_cfi`].
//! - Read and exchange annotations with [`Annotation`], [`AnnotationSet`](annotation::AnnotationSet),
//!   and [`AnnotationBundle`](annotation::AnnotationBundle).
//!
//! # Platform
//!
//! Parsing, modeling, analysis, CFI, annotations, and editing are portable, with no filesystem,
//! threads, or async runtime, and `wasm32-unknown-unknown` is a supported target.
//! [`EpubZip::open`] and [`Epub::export_to_path`] use the native filesystem; without one, read a
//! container with [`EpubZip::from_reader`], open a publication with [`Epub::from_provider`], and
//! export through any [`Write`](std::io::Write) and [`Seek`](std::io::Seek) sink with
//! [`Epub::export`].

#![deny(missing_docs)]

pub mod accessibility;
pub mod analysis;
pub mod annotation;
pub mod cfi;
pub mod container;
pub mod content;
pub mod css;
pub mod edit;
pub mod media_overlay;
mod media_type;
pub mod navigation;
pub mod package;
mod publication;
pub mod resource;
pub mod semantics;
mod string;
pub mod vocab;
mod xml;

pub use analysis::PublicationAnalysis;
pub use annotation::Annotation;
pub use cfi::Cfi;
pub use container::EpubZip;
pub use edit::EpubEdit;
pub use publication::{Epub, EpubCreateError, EpubOpenError, EpubOpenFailure, EpubOpenLimits};
pub use resource::provider::{MemoryResourceProvider, ResourceProvider};
pub use resource::{EpubPath, ResourceIndex};
pub use string::{EpubString, EpubStringEmpty};
pub use xml::XmlDecodeError;

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeExamples;
