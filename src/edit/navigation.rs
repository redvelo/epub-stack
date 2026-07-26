//! Select navigation lists, points, and insertion locations for staged edits.
//!
//! Use principal-list variants for the table of contents, page list, or landmarks, and use an
//! index for document order. Point paths are exact; href and label matches must be unique within
//! the selected staged tree. `AuthoredHref` matches exact source text. Navigation edits serialize
//! the changed XML and can normalize lexical details.

use crate::{
    resource::{AuthoredHref, EpubHref},
    semantics::EpubString,
};
use std::fmt;

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

#[derive(Debug, Clone, PartialEq, Eq)]
/// Selects one navigation point within one list.
pub struct PointSelector {
    list: ListSelector,
    point_match: PointMatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Selects the list or parent point under which a point is inserted.
pub enum InsertionTarget {
    /// Insert among a selected list's top-level points.
    List(ListSelector),
    /// Insert among the children of one selected point.
    ChildOf(PointSelector),
}

impl ListSelector {
    /// Selects the list at `index` in document order.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

impl PointMatch {
    /// Matches the point at a zero-based child-index path.
    pub fn path(path: Vec<usize>) -> Self {
        Self::Path(path)
    }

    /// Matches a point by its normalized href.
    pub fn href(href: EpubHref) -> Self {
        Self::Href(href)
    }

    /// Matches a point by its exact authored href.
    pub fn authored_href(href: AuthoredHref) -> Self {
        Self::AuthoredHref(href)
    }

    /// Matches a point by its normalized label.
    pub fn label(label: EpubString) -> Self {
        Self::Label(label)
    }
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

impl PointSelector {
    /// Creates a selector from a list and point match.
    pub fn new(list: ListSelector, point_match: impl Into<PointMatch>) -> Self {
        Self {
            list,
            point_match: point_match.into(),
        }
    }

    /// Selects a point in the table of contents.
    pub fn toc(point_match: impl Into<PointMatch>) -> Self {
        Self::new(ListSelector::Toc, point_match)
    }

    /// Selects a point in the page list.
    pub fn page_list(point_match: impl Into<PointMatch>) -> Self {
        Self::new(ListSelector::PageList, point_match)
    }

    /// Selects a point in the landmarks list.
    pub fn landmarks(point_match: impl Into<PointMatch>) -> Self {
        Self::new(ListSelector::Landmarks, point_match)
    }

    /// Returns the selected list.
    pub fn list(&self) -> ListSelector {
        self.list
    }

    /// Returns the point match.
    pub fn point_match(&self) -> &PointMatch {
        &self.point_match
    }

    /// Splits the selector into its list and point match.
    pub fn into_parts(self) -> (ListSelector, PointMatch) {
        (self.list, self.point_match)
    }
}

impl InsertionTarget {
    /// Inserts directly into a list.
    pub fn list(list: ListSelector) -> Self {
        Self::List(list)
    }

    /// Inserts as a child of a selected point.
    pub fn child_of(selector: PointSelector) -> Self {
        Self::ChildOf(selector)
    }
}

impl fmt::Display for ListSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toc => formatter.write_str("toc"),
            Self::PageList => formatter.write_str("page-list"),
            Self::Landmarks => formatter.write_str("landmarks"),
            Self::Index(index) => write!(formatter, "index {index}"),
        }
    }
}

impl fmt::Display for PointMatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(formatter, "path {path:?}"),
            Self::Href(href) => write!(formatter, "href {href}"),
            Self::AuthoredHref(href) => write!(formatter, "authored href {href}"),
            Self::Label(label) => write!(formatter, "label {label}"),
        }
    }
}

impl fmt::Display for PointSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} list, {}", self.list, self.point_match)
    }
}
