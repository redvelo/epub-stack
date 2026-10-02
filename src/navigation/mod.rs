//! Parse, inspect, and generate EPUB navigation.
//!
//! Use [`parse::epub_nav`] or [`parse::ncx`] to read a navigation document. Inspect its lists
//! through [`NavigationDocument`], build points with [`NavigationPoint::builder`], and use
//! [`NavigationDocument::to_normalized_xhtml`] to generate EPUB navigation XHTML.
//! [`crate::Epub::navigation`] returns the document selected when a publication is opened.
//!
//! Labels and headings are trimmed. Original hrefs and semantic tokens are retained;
//! arbitrary markup and source formatting are not.
//!
//! Read the table of contents, including nested sections.
//!
//! ```
//! use epub_stack::{EpubZip, navigation::NavigationPoint};
//!
//! fn print_toc(points: &[NavigationPoint], depth: usize) {
//!     for point in points {
//!         if let Some(label) = point.label() {
//!             println!("{:indent$}{label}", "", indent = depth * 2);
//!         }
//!         print_toc(point.children(), depth + 1);
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//!
//! if let Some(toc) = book.navigation().and_then(|navigation| navigation.toc()) {
//!     print_toc(toc.points(), 0);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Use [`NavigationDocument::page_list`] or [`NavigationDocument::landmarks`] for other lists.

pub mod parse;

pub(crate) mod generate;

pub use generate::NavigationGenerateError;

pub mod facts;

use crate::{
    resource::{AuthoredHref, EpubHref, EpubPath},
    semantics::{EpubStructuralSemantic, HeadingLevel, SemanticToken},
    string::EpubString,
};

const MAX_NAV_DEPTH: usize = 128;

/// Failure to construct navigation whose point tree exceeds the supported depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Navigation nesting exceeds the supported depth")]
pub struct NavigationDepthError;

/// A heading associated with a navigation list.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Heading {
    level: HeadingLevel,
    text: EpubString,
}

impl Heading {
    pub(crate) fn new(level: HeadingLevel, text: EpubString) -> Self {
        Self { level, text }
    }

    /// The authored heading level.
    pub fn level(&self) -> HeadingLevel {
        self.level
    }

    /// The heading text, trimmed when it became an [`EpubString`].
    pub fn text(&self) -> &EpubString {
        &self.text
    }

    pub(crate) fn h2(text: EpubString) -> Self {
        Self::new(
            HeadingLevel::new(2).expect("2 is a valid heading level"),
            text,
        )
    }
}

/// A book's table of contents, page list and landmarks, whether they came from an EPUB
/// navigation document or a legacy NCX.
///
/// Hrefs are kept exactly as written. Resolving one means accounting for both [`Self::path`]
/// and [`Self::authored_base`], so prefer
/// [`Epub::navigation_targets`](crate::Epub::navigation_targets).
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NavigationDocument {
    source: NavigationSource,
    path: EpubPath,
    authored_base: Option<AuthoredHref>,
    lists: Vec<NavigationList>,
}

#[bon::bon]
impl NavigationDocument {
    #[builder]
    pub(crate) fn new(
        path: EpubPath,
        #[builder(default)] lists: Vec<NavigationList>,
    ) -> Result<Self, NavigationDepthError> {
        ensure_navigation_depth(&lists)?;
        Ok(Self {
            source: NavigationSource::EpubNav,
            path,
            authored_base: None,
            lists,
        })
    }

    pub(crate) fn from_parsed(
        source: NavigationSource,
        path: EpubPath,
        lists: Vec<NavigationList>,
    ) -> Self {
        Self {
            source,
            path,
            authored_base: None,
            lists,
        }
    }

    /// The source document syntax.
    pub fn source(&self) -> NavigationSource {
        self.source
    }

    /// Reports whether the document was parsed from an EPUB navigation document.
    pub fn is_epub_nav(&self) -> bool {
        self.source == NavigationSource::EpubNav
    }

    /// Reports whether the document was parsed from an NCX document.
    pub fn is_ncx(&self) -> bool {
        self.source == NavigationSource::Ncx
    }

    /// Serializes this document as normalized EPUB navigation XHTML.
    ///
    /// Accepts EPUB NAV or NCX input. Hrefs retain their authored spelling and base location;
    /// unmodeled markup is omitted. The publication is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationGenerateError`] when generation or XHTML encoding fails.
    pub fn to_normalized_xhtml(
        &self,
        title: &EpubString,
    ) -> Result<String, NavigationGenerateError> {
        self.generate_epub_nav_xhtml(self.path(), title)
    }

    /// The document's canonical publication path.
    pub fn path(&self) -> &EpubPath {
        &self.path
    }

    /// Returns the first authored XHTML head base href, including unusable source text.
    pub fn authored_base(&self) -> Option<&AuthoredHref> {
        self.authored_base.as_ref()
    }

    /// All navigation lists in document order.
    pub fn lists(&self) -> &[NavigationList] {
        self.lists.as_slice()
    }

    /// The first list classified as a table of contents.
    pub fn toc(&self) -> Option<&NavigationList> {
        self.lists
            .iter()
            .find(|list| list.semantic() == Some(EpubStructuralSemantic::Toc))
    }

    /// The first list classified as a page list.
    pub fn page_list(&self) -> Option<&NavigationList> {
        self.lists
            .iter()
            .find(|list| list.semantic() == Some(EpubStructuralSemantic::PageList))
    }

    /// The first list classified as landmarks.
    pub fn landmarks(&self) -> Option<&NavigationList> {
        self.lists
            .iter()
            .find(|list| list.semantic() == Some(EpubStructuralSemantic::Landmarks))
    }

    /// Iterates lists not normalized as TOC, page list, or landmarks.
    pub fn auxiliary_lists(&self) -> impl Iterator<Item = &NavigationList> {
        self.lists
            .iter()
            .filter(|list| !is_principal_list_semantic(list.semantic()))
    }
}

/// The syntax of a selected navigation document.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NavigationSource {
    /// EPUB 3 navigation XHTML.
    EpubNav,
    /// EPUB 2 Navigation Center eXtended.
    Ncx,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// One navigation list: a table of contents, a page list, a set of landmarks.
pub struct NavigationList {
    semantic: Option<EpubStructuralSemantic>,
    heading: Option<Heading>,
    hidden: bool,
    points: Vec<NavigationPoint>,
    authored_semantic_tokens: Vec<SemanticToken>,
}

#[bon::bon]
impl NavigationList {
    #[builder]
    pub(crate) fn new(
        semantic: Option<EpubStructuralSemantic>,
        heading: Option<Heading>,
        #[builder(default)] hidden: bool,
        #[builder(default)] points: Vec<NavigationPoint>,
    ) -> Result<Self, NavigationDepthError> {
        ensure_points_depth(&points)?;
        Ok(Self::from_authored(
            semantic,
            heading,
            hidden,
            points,
            Vec::new(),
        ))
    }

    pub(crate) fn from_semantics(
        authored_semantic_tokens: Vec<SemanticToken>,
        heading: Option<Heading>,
        hidden: bool,
        points: Vec<NavigationPoint>,
    ) -> Self {
        let principal = first_navigation_list_semantic(&authored_semantic_tokens);
        let semantic = principal.or_else(|| first_semantics(&authored_semantic_tokens));
        Self::from_authored(semantic, heading, hidden, points, authored_semantic_tokens)
    }

    pub(crate) fn from_authored(
        semantic: Option<EpubStructuralSemantic>,
        heading: Option<Heading>,
        hidden: bool,
        points: Vec<NavigationPoint>,
        authored_semantic_tokens: Vec<SemanticToken>,
    ) -> Self {
        Self {
            semantic,
            heading,
            hidden,
            points,
            authored_semantic_tokens,
        }
    }

    /// The semantic used to classify the list.
    ///
    /// This is distinct from [`Self::authored_semantic_tokens`]; NCX list structure can
    /// establish normalized meaning without asserting an EPUB token.
    pub fn semantic(&self) -> Option<EpubStructuralSemantic> {
        self.semantic
    }

    /// The recovered list heading.
    pub fn heading(&self) -> Option<&Heading> {
        self.heading.as_ref()
    }

    /// Whether the source marked the list hidden.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Top-level points in source order.
    pub fn points(&self) -> &[NavigationPoint] {
        self.points.as_slice()
    }

    /// Retained authored `epub:type`, class, and role evidence.
    pub fn authored_semantic_tokens(&self) -> &[SemanticToken] {
        self.authored_semantic_tokens.as_slice()
    }

    pub(crate) fn epub_type_token(&self) -> Option<String> {
        self.semantic().map(|semantic| semantic.to_string())
    }
}

fn is_principal_list_semantic(semantic: Option<EpubStructuralSemantic>) -> bool {
    matches!(
        semantic,
        Some(
            EpubStructuralSemantic::Toc
                | EpubStructuralSemantic::PageList
                | EpubStructuralSemantic::Landmarks
        )
    )
}

pub(crate) fn first_semantics(tokens: &[SemanticToken]) -> Option<EpubStructuralSemantic> {
    tokens.iter().find_map(SemanticToken::epub_semantic)
}

fn first_navigation_list_semantic(tokens: &[SemanticToken]) -> Option<EpubStructuralSemantic> {
    tokens
        .iter()
        .filter_map(SemanticToken::epub_semantic)
        .find(|semantic| {
            matches!(
                semantic,
                EpubStructuralSemantic::Toc
                    | EpubStructuralSemantic::PageList
                    | EpubStructuralSemantic::Landmarks
            )
        })
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// One entry in a navigation list, with its label and where it points.
///
/// Building one requires a label. Parsing does not: a book with an unlabelled entry still
/// produces one here.
///
/// ```compile_fail
/// use epub_stack::navigation::NavigationPoint;
///
/// // Bon's typestate rejects construction without the required label.
/// let _ = NavigationPoint::builder().build();
/// ```
pub struct NavigationPoint {
    label: Option<EpubString>,
    authored_href: Option<AuthoredHref>,
    children: Vec<NavigationPoint>,
    hidden: bool,
    semantic: Option<EpubStructuralSemantic>,
    authored_semantic_tokens: Vec<SemanticToken>,
}

#[bon::bon]
impl NavigationPoint {
    /// Builds a navigation point from a label, optional href, children, and semantic meaning.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationDepthError`] when the resulting tree exceeds the nesting limit.
    #[builder]
    pub fn new(
        label: EpubString,
        href: Option<EpubHref>,
        #[builder(default)] children: Vec<NavigationPoint>,
        #[builder(default)] hidden: bool,
        semantic: Option<EpubStructuralSemantic>,
    ) -> Result<Self, NavigationDepthError> {
        ensure_point_depth(&children)?;
        let authored_href = href.as_ref().map(AuthoredHref::from);
        Ok(Self {
            label: Some(label),
            authored_href,
            children,
            hidden,
            semantic,
            authored_semantic_tokens: Vec::new(),
        })
    }

    pub(crate) fn from_authored(
        label: Option<EpubString>,
        authored_href: Option<AuthoredHref>,
        children: Vec<NavigationPoint>,
        hidden: bool,
        semantic: Option<EpubStructuralSemantic>,
        authored_semantic_tokens: Vec<SemanticToken>,
    ) -> Self {
        Self {
            label,
            authored_href,
            children,
            hidden,
            semantic,
            authored_semantic_tokens,
        }
    }

    /// The point label, trimmed when it became an [`EpubString`].
    pub fn label(&self) -> Option<&EpubString> {
        self.label.as_ref()
    }

    /// Returns the point's href when its original text has valid EPUB href syntax.
    ///
    /// Use [`Self::authored_href`] to inspect malformed or otherwise unparsed evidence.
    pub fn href(&self) -> Option<EpubHref> {
        self.authored_href
            .as_ref()
            .and_then(AuthoredHref::to_epub_href)
    }

    /// Exact authored href evidence, including values that do not parse.
    pub fn authored_href(&self) -> Option<&AuthoredHref> {
        self.authored_href.as_ref()
    }

    /// Child points in source order.
    pub fn children(&self) -> &[NavigationPoint] {
        self.children.as_slice()
    }

    /// Whether the source marked the point hidden.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// The point's structural meaning.
    pub fn semantic(&self) -> Option<EpubStructuralSemantic> {
        self.semantic
    }

    /// Retained authored semantic evidence.
    pub fn authored_semantic_tokens(&self) -> &[SemanticToken] {
        self.authored_semantic_tokens.as_slice()
    }
}

fn ensure_point_depth(children: &[NavigationPoint]) -> Result<(), NavigationDepthError> {
    let child_depth = children
        .iter()
        .map(navigation_point_depth)
        .max()
        .unwrap_or(0);
    if child_depth >= MAX_NAV_DEPTH {
        return Err(NavigationDepthError);
    }
    Ok(())
}

fn ensure_navigation_depth(lists: &[NavigationList]) -> Result<(), NavigationDepthError> {
    for list in lists {
        ensure_points_depth(list.points())?;
    }
    Ok(())
}

fn ensure_points_depth(points: &[NavigationPoint]) -> Result<(), NavigationDepthError> {
    if points
        .iter()
        .any(|point| navigation_point_depth(point) > MAX_NAV_DEPTH)
    {
        return Err(NavigationDepthError);
    }
    Ok(())
}

pub(crate) fn navigation_point_depth(point: &NavigationPoint) -> usize {
    1 + point
        .children()
        .iter()
        .map(navigation_point_depth)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epub_string_rejects_empty_navigation_text() {
        assert!(EpubString::new("").is_none());
        assert!(EpubString::new("   ").is_none());
        assert_eq!(EpubString::new(" Chapter ").unwrap().as_str(), "Chapter");
        assert!(EpubString::try_new("").is_err());
        assert!(EpubString::try_new("   ").is_err());
    }

    #[test]
    fn public_navigation_construction_enforces_the_parser_depth_limit() {
        let mut point = NavigationPoint::builder()
            .label(EpubString::new("Level 1").unwrap())
            .build()
            .unwrap();
        for level in 2..=MAX_NAV_DEPTH {
            point = NavigationPoint::builder()
                .label(EpubString::new(format!("Level {level}")).unwrap())
                .children(vec![point])
                .build()
                .unwrap();
        }
        assert_eq!(navigation_point_depth(&point), MAX_NAV_DEPTH);
        assert_eq!(
            NavigationPoint::builder()
                .label(EpubString::new("Too deep").unwrap())
                .children(vec![point])
                .build(),
            Err(NavigationDepthError)
        );
    }
}
