//! Open, inspect, analyze, edit, and export EPUB publications with typed APIs oriented to EPUB 3.4.
//!
//! # Quick start
//!
//! This example opens an EPUB file from the local filesystem.
//!
//! ```no_run
//! use epub_stack::{EpubZip, ResourceSelector};
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("book.epub")?
//!     .default_rendition()?;
//! println!("package: {}", book.package_path());
//! println!("reading-order entries: {}", book.reading_order().count());
//! println!(
//!     "package source: {} bytes",
//!     book.resource(ResourceSelector::Package)?.bytes()?.len()
//! );
//! println!("{} resources analyzed", book.analyze().resource_facts().count());
//! # Ok(())
//! # }
//! ```
//!
//! # Central types
//!
//! - [`EpubZip`] opens an EPUB file and selects a rendition.
//! - [`Epub`] exposes package metadata, navigation, reading order, resources, and editing.
//! - [`ResourceIndex`] and [`ResourceSelector`] locate publication resources.
//! - [`resource::Resource`] reads bytes, UTF-8 text, or a stream.
//! - [`PublicationAnalysis`] collects resource, relationship, and accessibility information.
//! - [`EpubEdit`] and [`EpubEditPreview`] stage and inspect changes.
//! - [`ResourceProvider`] supplies publication bytes when a filesystem EPUB is not available.
//!
//! An analysis is a snapshot. Run [`Epub::analyze`] again after committing edits when current
//! results are required.
//!
//! # Edit and export
//!
//! 1. Start with [`Epub::edit`] and stage changes.
//! 2. Call [`EpubEdit::preview`] and inspect the proposed resources and changes.
//! 3. Call [`EpubEditPreview::commit`] to update the open publication.
//! 4. Call [`Epub::export`] or [`Epub::export_to_path`] to write the committed publication.
//!
//! Preview and commit do not write an EPUB file. Export writes a normalized EPUB ZIP.
//!
//! # More tasks
//!
//! - Create a minimal publication with [`Epub::create`].
//! - Parse locations with [`Cfi`] and [`CfiRange`], then resolve them with
//!   [`Epub::resolve_cfi`] or [`Epub::resolve_cfi_range`].
//! - Read and exchange annotations with [`Annotation`], [`AnnotationSet`], and
//!   [`AnnotationBundle`].
//!
//! # Scope and platforms
//!
//! This crate does not decide whether an EPUB is valid, and it does not perform browser layout,
//! DOM measurement, scripting policy, or network acquisition. Parsing, modeling, analysis, and
//! editing support `wasm32-unknown-unknown`; [`EpubZip::open`] and [`Epub::export_to_path`] use the
//! native filesystem. Export preserves unchanged publication resource payloads, except for the
//! canonical `mimetype` entry, but not ZIP byte identity.

#![deny(missing_docs)]

pub mod accessibility;
pub mod analysis;
pub mod annotation;
pub mod cfi;
pub mod container;
pub mod content;
pub mod edit;
mod error;
pub mod media_overlay;
mod media_type;
pub mod navigation;
pub mod package;
mod publication;
pub mod resource;
pub mod semantics;
mod string;
mod xml;

pub use analysis::{AnalysisLimits, PublicationAnalysis};
pub use annotation::{Annotation, AnnotationBundle, AnnotationSet};
pub use cfi::{Cfi, CfiRange};
pub use container::EpubZip;
pub use edit::{EditReport, EpubEdit, EpubEditPreview};
pub use publication::{
    Epub, EpubCreateError, EpubOpenError, EpubOpenFailure, EpubOpenLimits, EpubOpenLimitsError,
    NavigationHrefTargetFacts, NavigationLoadingFacts, NavigationLoadingOutcome,
    NavigationTargetFacts, NavigationTargetOutcomeFacts, PublicationFacts, PublicationFactsError,
};
pub use resource::provider::{MemoryResourceProvider, ResourceProvider};
pub use resource::{EpubPath, ResourceIndex, ResourceSelector};
