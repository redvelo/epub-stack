//! Parse EPUB NAV XHTML and EPUB 2 NCX documents.
//!
//! Call [`epub_nav`] for EPUB 3 navigation XHTML or [`ncx`] for an EPUB 2 NCX. Both take the
//! document's own [`EpubPath`](crate::resource::EpubPath), which becomes its identity and the base
//! for its hrefs, and fail with [`NavigationParseError`]. Parsing resolves nothing against a
//! publication; for that, pass the result to [`super::facts::navigation_targets`].
//!
//! Point trees are limited to 128 levels, and EPUB NAV label markup to 128 elements.

pub(crate) mod common;
mod epub_nav;
mod ncx;

pub use common::NavigationParseError;
pub use epub_nav::epub_nav;
pub use ncx::ncx;

pub(crate) use ncx::ncx_reader;
