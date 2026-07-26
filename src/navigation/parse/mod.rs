//! Parse EPUB NAV XHTML and EPUB 2 NCX documents.
//!
//! Call [`epub_nav`] for EPUB 3 navigation XHTML or [`ncx`] for an EPUB 2 NCX. Each function
//! returns a [`super::NavigationDocument`] and retains the supplied
//! [`EpubPath`](crate::resource::EpubPath) as both document identity and the base for later href
//! interpretation. Parsing does not resolve href targets against a publication.
//!
//! Point trees are limited to 128 navigation levels. Results retain modeled labels, hrefs, and
//! semantic evidence, not arbitrary source markup or byte layout.

pub(crate) mod common;
mod epub_nav;
mod ncx;

pub use common::NavigationParseError;
pub use epub_nav::epub_nav;
pub use ncx::ncx;

pub(crate) use ncx::ncx_reader;
