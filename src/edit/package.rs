//! Select package metadata, manifest items, and spine entries for staged edits.
//!
//! Selection occurs when an edit operation consumes a selector. Use normalized variants for
//! semantic matching and `AuthoredHref` variants when exact source text matters. Package edits
//! serialize the changed XML and can normalize lexical details.

use crate::{
    resource::{AuthoredHref, EpubHref},
    semantics::EpubString,
};

/// Selects one manifest item from the staged package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestItemSelector {
    /// Match the normalized manifest `id` value.
    Id(EpubString),
    /// Match the parsed normalized href value.
    Href(EpubHref),
    /// Match exact authored href text.
    AuthoredHref(AuthoredHref),
}

impl ManifestItemSelector {
    /// Selects by manifest ID.
    pub fn id(id: EpubString) -> Self {
        Self::Id(id)
    }
    /// Selects by normalized href.
    pub fn href(href: EpubHref) -> Self {
        Self::Href(href)
    }
    /// Selects by exact authored href.
    pub fn authored_href(href: AuthoredHref) -> Self {
        Self::AuthoredHref(href)
    }
}

impl From<EpubString> for ManifestItemSelector {
    fn from(value: EpubString) -> Self {
        Self::Id(value)
    }
}

impl From<EpubHref> for ManifestItemSelector {
    fn from(value: EpubHref) -> Self {
        Self::Href(value)
    }
}

impl From<AuthoredHref> for ManifestItemSelector {
    fn from(value: AuthoredHref) -> Self {
        Self::AuthoredHref(value)
    }
}

/// Selects one spine item reference from the staged package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpineItemRefSelector {
    /// Match the itemref's manifest `idref`.
    Idref(EpubString),
    /// Select by zero-based spine order.
    Index(usize),
}

impl SpineItemRefSelector {
    /// Selects by manifest `idref`.
    pub fn idref(idref: EpubString) -> Self {
        Self::Idref(idref)
    }
    /// Selects by zero-based spine order.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

impl From<EpubString> for SpineItemRefSelector {
    fn from(value: EpubString) -> Self {
        Self::Idref(value)
    }
}

impl From<usize> for SpineItemRefSelector {
    fn from(value: usize) -> Self {
        Self::Index(value)
    }
}

/// Selects one Dublin Core metadata element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataElementSelector {
    /// Select an identifier element.
    Identifier(MetadataNodeSelector),
    /// Select a title element.
    Title(MetadataNodeSelector),
    /// Select a language element.
    Language(MetadataNodeSelector),
    /// Select a contributor element.
    Contributor(MetadataNodeSelector),
    /// Select a coverage element.
    Coverage(MetadataNodeSelector),
    /// Select a creator element.
    Creator(MetadataNodeSelector),
    /// Select a date element.
    Date(MetadataNodeSelector),
    /// Select a description element.
    Description(MetadataNodeSelector),
    /// Select a format element.
    Format(MetadataNodeSelector),
    /// Select a publisher element.
    Publisher(MetadataNodeSelector),
    /// Select a relation element.
    Relation(MetadataNodeSelector),
    /// Select a rights element.
    Rights(MetadataNodeSelector),
    /// Select a source element.
    Source(MetadataNodeSelector),
    /// Select a subject element.
    Subject(MetadataNodeSelector),
    /// Select a type element.
    Type(MetadataNodeSelector),
}

impl MetadataElementSelector {
    /// Selects a title using the supplied node match.
    pub fn title(selector: MetadataNodeSelector) -> Self {
        Self::Title(selector)
    }
    /// Selects an identifier using the supplied node match.
    pub fn identifier(selector: MetadataNodeSelector) -> Self {
        Self::Identifier(selector)
    }
    /// Selects a language using the supplied node match.
    pub fn language(selector: MetadataNodeSelector) -> Self {
        Self::Language(selector)
    }
}

/// Selects a metadata node by authored identity or semantic-list index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataNodeSelector {
    /// Match an authored metadata `id`.
    Id(EpubString),
    /// Select by zero-based order within one metadata element kind.
    Index(usize),
}

impl MetadataNodeSelector {
    /// Selects by authored ID.
    pub fn id(id: EpubString) -> Self {
        Self::Id(id)
    }
    /// Selects by zero-based kind-local order.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

/// Selects one EPUB 3 `meta` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaSelector {
    /// Match an authored `id`.
    Id(EpubString),
    /// Match a normalized property token.
    Property(EpubString),
    /// Match both property and `refines` values.
    PropertyRefines {
        /// Required property value.
        property: EpubString,
        /// Required `refines` value.
        refines: EpubString,
    },
    /// Select by zero-based EPUB 3 `meta` order.
    Index(usize),
}

impl MetaSelector {
    /// Selects by authored ID.
    pub fn id(id: EpubString) -> Self {
        Self::Id(id)
    }
    /// Selects by property.
    pub fn property(property: EpubString) -> Self {
        Self::Property(property)
    }
    /// Selects by property and `refines` together.
    pub fn property_refines(property: EpubString, refines: EpubString) -> Self {
        Self::PropertyRefines { property, refines }
    }
    /// Selects by zero-based EPUB 3 `meta` order.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

/// Selects one package metadata `link` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataLinkSelector {
    /// Match an authored `id`.
    Id(EpubString),
    /// Match a parsed normalized href.
    Href(EpubHref),
    /// Match exact authored href text.
    AuthoredHref(AuthoredHref),
    /// Select by zero-based metadata link order.
    Index(usize),
}

impl MetadataLinkSelector {
    /// Selects by authored ID.
    pub fn id(id: EpubString) -> Self {
        Self::Id(id)
    }
    /// Selects by normalized href.
    pub fn href(href: EpubHref) -> Self {
        Self::Href(href)
    }
    /// Selects by exact authored href.
    pub fn authored_href(href: AuthoredHref) -> Self {
        Self::AuthoredHref(href)
    }
    /// Selects by zero-based metadata link order.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}
