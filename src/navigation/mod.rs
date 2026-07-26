//! Parse, inspect, and generate EPUB navigation.
//!
//! Use [`parse::epub_nav`] or [`parse::ncx`] to read a navigation document. Inspect its lists
//! through [`NavigationDocument`], build points with [`NavigationPoint::builder`], and use
//! [`Navigation::to_normalized_xhtml`] to generate EPUB navigation XHTML. [`Navigation`] holds
//! the source selected when an [`crate::Epub`] is opened; it does not merge EPUB NAV with a
//! fallback or secondary NCX document.
//!
//! Labels and headings use [`EpubString`], so leading and trailing Unicode whitespace is removed.
//! Point trees are limited to 128 navigation levels. The model retains original hrefs and
//! semantic tokens, but not arbitrary markup, attributes, comments, or byte layout. Generated
//! navigation is therefore not a byte-for-byte copy of the source.

pub mod parse;

mod generate;

pub(crate) use generate::NavigationGenerateError;

use crate::{
    resource::{AuthoredHref, EpubHref, EpubPath},
    semantics::{DpubAriaRole, EpubStructuralSemantic, HeadingLevel},
    string::EpubString,
};

const MAX_NAV_DEPTH: usize = 128;

/// Failure to construct navigation whose point tree exceeds the supported depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Navigation nesting exceeds the supported depth")]
pub struct NavigationDepthError;

/// Failure to serialize selected normalized navigation as EPUB navigation XHTML.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NavigationXhtmlError {
    /// No navigation document was selected.
    #[error("Cannot serialize navigation XHTML without a selected navigation document")]
    NoDocument,
    /// The normalized navigation document could not be encoded as XHTML.
    #[error("Could not serialize navigation XHTML: {message}")]
    Serialization {
        /// The underlying XML writer failure rendered for diagnostics.
        message: String,
    },
}

/// A heading associated with a navigation list.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
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

#[derive(Debug, PartialEq, Eq, Clone, Hash, Default)]
/// The selected navigation state for a publication.
///
/// Ordinary opening retains zero or one document: EPUB NAV when usable, otherwise the
/// usable NCX fallback. Declared secondary navigation resources remain outside this model.
pub struct Navigation {
    document: Option<NavigationDocument>,
}

impl Navigation {
    /// Creates navigation containing the selected document.
    pub fn new(document: NavigationDocument) -> Self {
        Self {
            document: Some(document),
        }
    }

    /// Creates navigation with no successfully loaded document.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether no navigation document was selected.
    pub fn is_empty(&self) -> bool {
        self.document.is_none()
    }

    pub(crate) fn replace_epub_nav(&mut self, document: NavigationDocument) {
        self.document = Some(document);
    }

    pub(crate) fn remove_source(&mut self, source: NavigationSource) {
        if self
            .document
            .as_ref()
            .is_some_and(|document| document.source() == source)
        {
            self.document = None;
        }
    }

    /// The selected document, if it came from EPUB NAV.
    pub fn epub_nav(&self) -> Option<&NavigationDocument> {
        self.document
            .as_ref()
            .filter(|document| document.source() == NavigationSource::EpubNav)
    }

    /// The selected document, if it came from NCX.
    pub fn ncx(&self) -> Option<&NavigationDocument> {
        self.document
            .as_ref()
            .filter(|document| document.source() == NavigationSource::Ncx)
    }

    /// The selected navigation document.
    pub fn document(&self) -> Option<&NavigationDocument> {
        self.document.as_ref()
    }

    /// The selected document's first table-of-contents list.
    pub fn toc(&self) -> Option<&NavigationList> {
        self.document().and_then(NavigationDocument::toc)
    }

    /// The selected document's first page list.
    pub fn page_list(&self) -> Option<&NavigationList> {
        self.document().and_then(NavigationDocument::page_list)
    }

    /// The selected document's first landmarks list.
    pub fn landmarks(&self) -> Option<&NavigationList> {
        self.document().and_then(NavigationDocument::landmarks)
    }

    /// Serializes the selected document as normalized EPUB navigation XHTML.
    ///
    /// EPUB NAV and NCX sources are projected through the same normalized list and point
    /// semantics. Href strings retain their authored spelling relative to the selected
    /// [`NavigationDocument::path`]; this operation does not relocate the document.
    ///
    /// This is a lossy semantic projection: publisher markup and source-only evidence not
    /// represented by the normalized model may be omitted. It does not mutate publication
    /// state or change ordinary no-op publication export behavior.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationXhtmlError::NoDocument`] when no document is selected, or
    /// [`NavigationXhtmlError::Serialization`] when XHTML encoding fails.
    pub fn to_normalized_xhtml(&self, title: &EpubString) -> Result<String, NavigationXhtmlError> {
        let document = self.document().ok_or(NavigationXhtmlError::NoDocument)?;
        document
            .generate_epub_nav_xhtml(document.path(), title)
            .map_err(|source| NavigationXhtmlError::Serialization {
                message: source.to_string(),
            })
    }
}

/// One parsed, normalized EPUB NAV or NCX document.
///
/// Hrefs remain relative authored references; resolve them against [`Self::path`] when mapping
/// navigation points to publication resources.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct NavigationDocument {
    source: NavigationSource,
    path: EpubPath,
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
            lists,
        }
    }

    /// The source document syntax.
    pub fn source(&self) -> NavigationSource {
        self.source
    }

    /// The document's canonical publication path.
    pub fn path(&self) -> &EpubPath {
        &self.path
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
pub enum NavigationSource {
    /// EPUB 3 navigation XHTML.
    EpubNav,
    /// EPUB 2 Navigation Center eXtended.
    Ncx,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
/// A normalized navigation list with separately retained authored semantics.
pub struct NavigationList {
    semantic: Option<EpubStructuralSemantic>,
    heading: Option<Heading>,
    hidden: bool,
    points: Vec<NavigationPoint>,
    authored_semantic_tokens: Vec<NavigationSemanticToken>,
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
        authored_semantic_tokens: Vec<NavigationSemanticToken>,
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
        authored_semantic_tokens: Vec<NavigationSemanticToken>,
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
    pub fn authored_semantic_tokens(&self) -> &[NavigationSemanticToken] {
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

/// The authored attribute that supplied navigation semantic evidence.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum NavigationSemanticSource {
    /// An EPUB `type` attribute token.
    EpubType,
    /// An NCX `class` attribute value.
    Class,
    /// An ARIA `role` attribute token.
    Role,
}

/// One retained authored navigation semantic token and its recognized meaning.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct NavigationSemanticToken {
    source: NavigationSemanticSource,
    raw: String,
    epub_semantic: Option<EpubStructuralSemantic>,
    dpub_role: Option<DpubAriaRole>,
}

impl NavigationSemanticToken {
    pub(crate) fn epub_type(
        raw: impl Into<String>,
        semantic: Option<EpubStructuralSemantic>,
    ) -> Self {
        Self {
            source: NavigationSemanticSource::EpubType,
            raw: raw.into(),
            epub_semantic: semantic,
            dpub_role: None,
        }
    }

    pub(crate) fn ncx_class(raw: impl Into<String>) -> Self {
        Self {
            source: NavigationSemanticSource::Class,
            raw: raw.into(),
            epub_semantic: None,
            dpub_role: None,
        }
    }

    pub(crate) fn role(raw: impl Into<String>, role: Option<DpubAriaRole>) -> Self {
        Self {
            source: NavigationSemanticSource::Role,
            raw: raw.into(),
            epub_semantic: None,
            dpub_role: role,
        }
    }

    /// The attribute family that supplied the token.
    pub fn source(&self) -> NavigationSemanticSource {
        self.source
    }

    /// The authored spelling.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// A recognized meaning asserted by an EPUB `type` token.
    pub fn epub_semantic(&self) -> Option<EpubStructuralSemantic> {
        self.epub_semantic
    }

    /// A recognized DPUB meaning asserted by an ARIA role token.
    pub fn dpub_role(&self) -> Option<DpubAriaRole> {
        self.dpub_role
    }

    /// Direct EPUB meaning, or the EPUB meaning related to a DPUB role.
    pub fn related_epub_semantic(&self) -> Option<EpubStructuralSemantic> {
        self.epub_semantic
            .or_else(|| self.dpub_role.and_then(DpubAriaRole::related_epub_semantic))
    }
}

pub(crate) fn first_semantics(
    tokens: &[NavigationSemanticToken],
) -> Option<EpubStructuralSemantic> {
    tokens
        .iter()
        .filter(|token| token.source() == NavigationSemanticSource::EpubType)
        .find_map(NavigationSemanticToken::epub_semantic)
}

fn first_navigation_list_semantic(
    tokens: &[NavigationSemanticToken],
) -> Option<EpubStructuralSemantic> {
    tokens
        .iter()
        .filter_map(NavigationSemanticToken::epub_semantic)
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
/// A navigation point with normalized meaning and source-token evidence.
///
/// Typed construction requires a nonempty label. Parsing can still recover authored points
/// whose labels are missing or empty.
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
    authored_semantic_tokens: Vec<NavigationSemanticToken>,
}

#[bon::bon]
impl NavigationPoint {
    /// Builds a navigation point from a label, optional href, children, and semantic meaning.
    ///
    /// The label is already trimmed because it is an [`EpubString`].
    ///
    /// # Errors
    ///
    /// Returns [`NavigationDepthError`] when the resulting tree exceeds 128 levels.
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
        authored_semantic_tokens: Vec<NavigationSemanticToken>,
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
    pub fn authored_semantic_tokens(&self) -> &[NavigationSemanticToken] {
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
