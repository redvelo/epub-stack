//! Inspect EPUB 2 metadata and guide references.
//!
//! [`Opf2Meta`](crate::package::legacy::Opf2Meta) represents legacy `meta` name/content pairs.
//! [`Guide`] and [`Reference`](crate::package::legacy::Reference) expose EPUB 2 guide entries,
//! including their authored href evidence and recognized
//! [`ReferenceType`](crate::package::legacy::ReferenceType).

use crate::resource::{AuthoredHref, EpubHref};
use crate::string::EpubString;

#[derive(Debug, PartialEq, Eq, Clone, Hash, bon::Builder)]
/// An owned EPUB 2 `meta` name/content pair.
///
/// Parsed instances may represent a missing side of the pair; [`Self::new`] requires both.
pub struct Opf2Meta {
    #[builder(required, with = Some)]
    name: Option<EpubString>,
    #[builder(required, with = Some)]
    content: Option<EpubString>,
}

impl Opf2Meta {
    pub(super) fn from_parsed(name: Option<EpubString>, content: Option<EpubString>) -> Self {
        Self { name, content }
    }

    /// Creates a complete EPUB 2 metadata pair from owned non-empty strings.
    pub fn new(name: EpubString, content: EpubString) -> Self {
        Self {
            name: Some(name),
            content: Some(content),
        }
    }

    /// Borrows the authored `name`, if modeled.
    pub fn name(&self) -> Option<&EpubString> {
        self.name.as_ref()
    }
    /// Borrows the authored `content`, if modeled.
    pub fn content(&self) -> Option<&EpubString> {
        self.content.as_ref()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
/// An owned EPUB 2 guide preserving reference order.
///
/// The semantic model does not retain unknown guide XML or lexical formatting.
pub struct Guide {
    references: Vec<Reference>,
}

impl Guide {
    pub(super) fn empty() -> Self {
        Self {
            references: Vec::new(),
        }
    }

    /// Borrows guide references in authored order.
    pub fn references(&self) -> &[Reference] {
        self.references.as_slice()
    }

    pub(super) fn from_parsed(references: Vec<Reference>) -> Self {
        Self { references }
    }

    pub(super) fn append(&mut self, mut other: Self) {
        self.references.append(&mut other.references);
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
/// A modeled EPUB 2 guide reference.
///
/// The authored href spelling is retained separately from its usable [`EpubHref`] projection.
pub struct Reference {
    reference_type: Option<ReferenceType>,
    title: Option<EpubString>,
    href: Option<AuthoredHref>,
}

impl Reference {
    pub(super) fn from_parsed(
        reference_type: Option<ReferenceType>,
        title: Option<EpubString>,
        href: Option<AuthoredHref>,
    ) -> Self {
        Self {
            reference_type,
            title,
            href,
        }
    }

    /// Borrows the optional authored title.
    pub fn title(&self) -> Option<&EpubString> {
        self.title.as_ref()
    }
    /// Returns an owned usable EPUB href projection.
    ///
    /// Returns `None` for a missing, empty, or whitespace-only authored href. The authored
    /// spelling remains available through [`Self::authored_href`].
    pub fn href(&self) -> Option<EpubHref> {
        self.href.as_ref().and_then(AuthoredHref::to_epub_href)
    }
    /// Borrows the source-preserving authored href, including empty or malformed text.
    pub fn authored_href(&self) -> Option<&AuthoredHref> {
        self.href.as_ref()
    }
    /// Returns a clone of the recognized reference type.
    ///
    /// Cloning allocates only for [`ReferenceType::Other`].
    pub fn reference_type(&self) -> Option<ReferenceType> {
        self.reference_type.clone()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, strum_macros::Display, strum_macros::EnumString, Hash)]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// A recognized EPUB 2 guide reference type, or an owned unknown token.
pub enum ReferenceType {
    /// The publication cover.
    Cover,
    /// The title page.
    TitlePage,
    /// The table of contents.
    Toc,
    /// An index.
    Index,
    /// A glossary.
    Glossary,
    /// Acknowledgements.
    Acknowledgements,
    /// A bibliography.
    Bibliography,
    /// A colophon.
    Colophon,
    /// A copyright page.
    CopyrightPage,
    /// A dedication.
    Dedication,
    /// An epigraph.
    Epigraph,
    /// A foreword.
    Foreword,
    /// A list of illustrations.
    Loi,
    /// A list of tables.
    Lot,
    /// Notes.
    Notes,
    /// A preface.
    Preface,
    /// The publication's main text.
    Text,
    #[strum(default)]
    /// An unrecognized authored type token, preserved as owned text.
    Other(String),
}
