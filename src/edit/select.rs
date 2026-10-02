//! Select package metadata, manifest items, spine entries, and navigation points for staged
//! edits.
//!
//! Selection occurs when an edit operation consumes a selector and always uses the staged state,
//! so earlier operations in the same transaction can shift ordinals and indexes. Use normalized
//! variants for semantic matching and `AuthoredHref` variants when exact source text matters.
//! Navigation edits affect EPUB NAV, not NCX.

use crate::{
    package::metadata::DcElement,
    resource::{AuthoredHref, EpubHref, ManifestOrdinal, ReadingOrderOrdinal},
    string::EpubString,
};

/// Selects one manifest item from the staged package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestItemSelector {
    /// Match the normalized manifest `id` value.
    Id(EpubString),
    /// Select by position in the staged manifest.
    Ordinal(ManifestOrdinal),
    /// Match the parsed normalized href value.
    Href(EpubHref),
    /// Match exact authored href text.
    AuthoredHref(AuthoredHref),
}

impl From<EpubString> for ManifestItemSelector {
    fn from(value: EpubString) -> Self {
        Self::Id(value)
    }
}

impl From<ManifestOrdinal> for ManifestItemSelector {
    fn from(value: ManifestOrdinal) -> Self {
        Self::Ordinal(value)
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
    /// Select by position in the staged spine.
    Ordinal(ReadingOrderOrdinal),
}

impl From<EpubString> for SpineItemRefSelector {
    fn from(value: EpubString) -> Self {
        Self::Idref(value)
    }
}

impl From<ReadingOrderOrdinal> for SpineItemRefSelector {
    fn from(value: ReadingOrderOrdinal) -> Self {
        Self::Ordinal(value)
    }
}

/// Selects one Dublin Core metadata element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataElementSelector {
    /// The Dublin Core element kind.
    pub element: DcElement,
    /// The node within that kind.
    pub node: MetadataNodeSelector,
}

/// Selects a metadata node by authored identity or kind-local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataNodeSelector {
    /// Match an authored metadata `id`.
    Id(EpubString),
    /// Select by zero-based order within one metadata element kind.
    Index(usize),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Selects a principal navigation list or a list by document order.
pub enum ListSelector {
    /// The list normalized as the table of contents.
    Toc,
    /// The list normalized as the page list.
    PageList,
    /// The list normalized as landmarks.
    Landmarks,
    /// The list at a zero-based document-order index.
    Index(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Matches a point within a selected navigation list.
///
/// Paths are exact; href and label matches must be unique within the selected staged tree.
pub enum PointMatch {
    /// A zero-based child-index path from the selected list.
    Path(Vec<usize>),
    /// A point whose href has the same normalized EPUB href value.
    Href(EpubHref),
    /// A point whose retained authored href evidence is exactly equal.
    AuthoredHref(AuthoredHref),
    /// A point whose normalized nonempty label is equal.
    Label(EpubString),
}

impl From<Vec<usize>> for PointMatch {
    fn from(value: Vec<usize>) -> Self {
        Self::Path(value)
    }
}

impl From<EpubHref> for PointMatch {
    fn from(value: EpubHref) -> Self {
        Self::Href(value)
    }
}

impl From<AuthoredHref> for PointMatch {
    fn from(value: AuthoredHref) -> Self {
        Self::AuthoredHref(value)
    }
}

impl From<EpubString> for PointMatch {
    fn from(value: EpubString) -> Self {
        Self::Label(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Selects one navigation point within one list.
pub struct PointSelector {
    /// The list containing the point.
    pub list: ListSelector,
    /// The point within that list.
    pub point: PointMatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Selects the list or parent point under which a point is inserted.
pub enum InsertionTarget {
    /// Insert among a selected list's top-level points.
    List(ListSelector),
    /// Insert among the children of one selected point.
    ChildOf(PointSelector),
}

/// The selector that failed to identify exactly one staged value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionTarget {
    /// A manifest item selector.
    ManifestItem(ManifestItemSelector),
    /// A spine itemref selector.
    SpineItemRef(SpineItemRefSelector),
    /// A Dublin Core element selector.
    MetadataElement(MetadataElementSelector),
    /// An EPUB 3 `meta` selector.
    Meta(MetaSelector),
    /// A metadata `link` selector.
    MetadataLink(MetadataLinkSelector),
    /// A navigation list selector.
    NavigationList(ListSelector),
    /// A navigation point selector.
    NavigationPoint(PointSelector),
}
