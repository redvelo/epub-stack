//! Parse and compare shared EPUB and DPUB semantic values.
//!
//! Use [`EpubStructuralSemantic`] for `epub:type` terms, [`DpubAriaRole`] for DPUB-ARIA roles,
//! [`TextDirection`] for direction attributes, and [`HeadingLevel`] for HTML heading levels.
//! [`SemanticToken`] records one authored or native token together with its recognized meaning.
//! Vocabulary enums expose canonical spellings through `as_str`.

use crate::string::{EpubString, EpubStringEmpty};
use crate::vocab::VocabToken;
use std::fmt;
use std::str::FromStr;

/// A constrained HTML heading level from 1 through 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
pub struct HeadingLevel(u8);

impl HeadingLevel {
    /// Creates a heading level, returning `None` outside the inclusive range 1 through 6.
    pub fn new(level: u8) -> Option<Self> {
        (1..=6).contains(&level).then_some(Self(level))
    }

    /// Returns the numeric level in the inclusive range 1 through 6.
    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Text direction accepted by EPUB `dir` attributes.
pub enum TextDirection {
    /// Left-to-right text.
    Ltr,
    /// Right-to-left text.
    Rtl,
    /// Direction inferred from the text.
    Auto,
}

impl TextDirection {
    /// Returns the canonical lowercase attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
            Self::Auto => "auto",
        }
    }

    /// Parses an attribute value, ignoring ASCII case.
    pub fn from_token(value: &str) -> Option<Self> {
        [Self::Ltr, Self::Rtl, Self::Auto]
            .into_iter()
            .find(|direction| direction.as_str().eq_ignore_ascii_case(value))
    }
}

impl fmt::Display for TextDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One authored or native token that establishes structural meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        tag = "source",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum SemanticToken {
    /// One whitespace-separated `epub:type` token.
    EpubType {
        /// The authored token and its recognized EPUB structural meaning.
        token: VocabToken<EpubStructuralSemantic>,
    },
    /// One whitespace-separated ARIA `role` token.
    AriaRole {
        /// The authored token and its recognized DPUB role.
        token: VocabToken<DpubAriaRole>,
    },
    /// One whitespace-separated NCX `class` token.
    NcxClass {
        /// The exact token spelling.
        token: EpubString,
    },
    /// Native semantics contributed by an HTML element itself.
    HtmlElement {
        /// The element's structural meaning.
        element: HtmlStructuralElement,
    },
}

impl SemanticToken {
    /// Classifies one authored `epub:type` token, which is recognized case-sensitively.
    pub fn epub_type(value: impl AsRef<str>) -> Result<Self, EpubStringEmpty> {
        VocabToken::try_new(value).map(|token| Self::EpubType { token })
    }

    /// Classifies one authored ARIA `role` token, which is recognized case-insensitively.
    pub fn aria_role(value: impl AsRef<str>) -> Result<Self, EpubStringEmpty> {
        VocabToken::try_new(value).map(|token| Self::AriaRole { token })
    }

    /// Retains one authored NCX `class` token, which has no recognized vocabulary.
    pub fn ncx_class(value: impl AsRef<str>) -> Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(|token| Self::NcxClass { token })
    }

    /// Returns the authored token or lowercase HTML element name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::EpubType { token } => token.as_str(),
            Self::AriaRole { token } => token.as_str(),
            Self::NcxClass { token } => token.as_str(),
            Self::HtmlElement { element } => element.as_str(),
        }
    }

    /// Returns a recognized EPUB meaning for an `epub:type` token.
    pub fn epub_semantic(&self) -> Option<EpubStructuralSemantic> {
        match self {
            Self::EpubType { token } => token.known_value(),
            Self::AriaRole { .. } | Self::NcxClass { .. } | Self::HtmlElement { .. } => None,
        }
    }

    /// Returns a recognized DPUB meaning for an ARIA `role` token.
    pub fn dpub_role(&self) -> Option<DpubAriaRole> {
        match self {
            Self::AriaRole { token } => token.known_value(),
            Self::EpubType { .. } | Self::NcxClass { .. } | Self::HtmlElement { .. } => None,
        }
    }

    /// Returns the direct EPUB meaning, or the EPUB meaning related to a DPUB role.
    pub fn related_epub_semantic(&self) -> Option<EpubStructuralSemantic> {
        self.epub_semantic().or_else(|| {
            self.dpub_role()
                .and_then(DpubAriaRole::related_epub_semantic)
        })
    }
}

/// An HTML element with native structural meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum HtmlStructuralElement {
    /// `section`.
    Section,
    /// `nav`.
    Navigation,
    /// `aside`.
    Aside,
    /// `figure`.
    Figure,
    /// `table`.
    Table,
    /// An `h1` through `h6` element.
    Heading(HeadingLevel),
}

impl HtmlStructuralElement {
    /// Returns the lowercase HTML local name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::Navigation => "nav",
            Self::Aside => "aside",
            Self::Figure => "figure",
            Self::Table => "table",
            Self::Heading(level) => match level.get() {
                1 => "h1",
                2 => "h2",
                3 => "h3",
                4 => "h4",
                5 => "h5",
                6 => "h6",
                _ => unreachable!("HeadingLevel accepts only 1 through 6"),
            },
        }
    }
}

macro_rules! vocabulary {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $token:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "kebab-case"))]
        #[cfg_attr(feature = "specta", derive(specta::Type))]
        pub enum $name {
            $(
                #[cfg_attr(not(feature = "specta"), doc = concat!("The canonical `", $token, "` token."))]
                #[cfg_attr(feature = "specta", doc = "A recognized canonical vocabulary token.")]
                #[cfg_attr(feature = "serde", serde(rename = $token))]
                $variant
            ),+
        }

        impl $name {
            /// Every recognized value in canonical vocabulary order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Returns the exact canonical vocabulary token.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }

            /// Parses an exact, case-sensitive canonical vocabulary token.
            pub fn from_token(value: &str) -> Option<Self> {
                match value {
                    $($token => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

vocabulary! {
    /// A recognized term from the EPUB Structural Semantics Vocabulary 1.1.
    ///
    /// Parsing is case-sensitive because EPUB vocabulary references are compact URLs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum EpubStructuralSemantic {
        Backmatter => "backmatter",
        Bodymatter => "bodymatter",
        Cover => "cover",
        Frontmatter => "frontmatter",
        Chapter => "chapter",
        Division => "division",
        Part => "part",
        Volume => "volume",
        Abstract => "abstract",
        Afterword => "afterword",
        Conclusion => "conclusion",
        Epigraph => "epigraph",
        Epilogue => "epilogue",
        Foreword => "foreword",
        Introduction => "introduction",
        Preamble => "preamble",
        Preface => "preface",
        Prologue => "prologue",
        Landmarks => "landmarks",
        Loa => "loa",
        Loi => "loi",
        Lot => "lot",
        Lov => "lov",
        Toc => "toc",
        Appendix => "appendix",
        Colophon => "colophon",
        Credits => "credits",
        Bibliography => "bibliography",
        AntonymGroup => "antonym-group",
        CondensedEntry => "condensed-entry",
        Def => "def",
        DictEntry => "dictentry",
        Dictionary => "dictionary",
        Etymology => "etymology",
        Example => "example",
        GramInfo => "gram-info",
        Idiom => "idiom",
        PartOfSpeech => "part-of-speech",
        PartOfSpeechList => "part-of-speech-list",
        PartOfSpeechGroup => "part-of-speech-group",
        PhoneticTranscription => "phonetic-transcription",
        PhraseList => "phrase-list",
        PhraseGroup => "phrase-group",
        SenseList => "sense-list",
        SenseGroup => "sense-group",
        SynonymGroup => "synonym-group",
        Tran => "tran",
        TranInfo => "tran-info",
        Glossary => "glossary",
        GlossDef => "glossdef",
        GlossTerm => "glossterm",
        Index => "index",
        IndexEditorNote => "index-editor-note",
        IndexEntry => "index-entry",
        IndexEntryList => "index-entry-list",
        IndexGroup => "index-group",
        IndexHeadnotes => "index-headnotes",
        IndexLegend => "index-legend",
        IndexLocator => "index-locator",
        IndexLocatorList => "index-locator-list",
        IndexLocatorRange => "index-locator-range",
        IndexTerm => "index-term",
        IndexTermCategories => "index-term-categories",
        IndexTermCategory => "index-term-category",
        IndexXrefPreferred => "index-xref-preferred",
        IndexXrefRelated => "index-xref-related",
        Acknowledgments => "acknowledgments",
        Contributors => "contributors",
        CopyrightPage => "copyright-page",
        Dedication => "dedication",
        Errata => "errata",
        HalfTitlePage => "halftitlepage",
        Imprimatur => "imprimatur",
        Imprint => "imprint",
        OtherCredits => "other-credits",
        RevisionHistory => "revision-history",
        TitlePage => "titlepage",
        Notice => "notice",
        PullQuote => "pullquote",
        Tip => "tip",
        CoverTitle => "covertitle",
        FullTitle => "fulltitle",
        HalfTitle => "halftitle",
        Subtitle => "subtitle",
        Title => "title",
        LearningObjective => "learning-objective",
        LearningResource => "learning-resource",
        Assessment => "assessment",
        Qna => "qna",
        Balloon => "balloon",
        Panel => "panel",
        PanelGroup => "panel-group",
        SoundArea => "sound-area",
        TextArea => "text-area",
        Endnotes => "endnotes",
        Footnote => "footnote",
        Footnotes => "footnotes",
        Backlink => "backlink",
        BiblioRef => "biblioref",
        GlossRef => "glossref",
        NoteRef => "noteref",
        ConcludingSentence => "concluding-sentence",
        Credit => "credit",
        Keyword => "keyword",
        TopicSentence => "topic-sentence",
        PageList => "page-list",
        Pagebreak => "pagebreak",
        Table => "table",
        TableRow => "table-row",
        TableCell => "table-cell",
        List => "list",
        ListItem => "list-item",
        Figure => "figure",
        Aside => "aside",
        AnnoRef => "annoref",
        Annotation => "annotation",
        BiblioEntry => "biblioentry",
        Bridgehead => "bridgehead",
        Endnote => "endnote",
        Help => "help",
        Marginalia => "marginalia",
        Note => "note",
        RearNote => "rearnote",
        RearNotes => "rearnotes",
        Sidebar => "sidebar",
        Subchapter => "subchapter",
        Warning => "warning"
    }
}

vocabulary! {
    /// A recognized role from Digital Publishing WAI-ARIA 1.1.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum DpubAriaRole {
        Abstract => "doc-abstract",
        Acknowledgments => "doc-acknowledgments",
        Afterword => "doc-afterword",
        Appendix => "doc-appendix",
        Backlink => "doc-backlink",
        BiblioEntry => "doc-biblioentry",
        Bibliography => "doc-bibliography",
        BiblioRef => "doc-biblioref",
        Chapter => "doc-chapter",
        Colophon => "doc-colophon",
        Conclusion => "doc-conclusion",
        Cover => "doc-cover",
        Credit => "doc-credit",
        Credits => "doc-credits",
        Dedication => "doc-dedication",
        Endnote => "doc-endnote",
        Endnotes => "doc-endnotes",
        Epigraph => "doc-epigraph",
        Epilogue => "doc-epilogue",
        Errata => "doc-errata",
        Example => "doc-example",
        Footnote => "doc-footnote",
        Foreword => "doc-foreword",
        Glossary => "doc-glossary",
        GlossRef => "doc-glossref",
        Index => "doc-index",
        Introduction => "doc-introduction",
        NoteRef => "doc-noteref",
        Notice => "doc-notice",
        Pagebreak => "doc-pagebreak",
        PageFooter => "doc-pagefooter",
        PageHeader => "doc-pageheader",
        PageList => "doc-pagelist",
        Part => "doc-part",
        Preface => "doc-preface",
        Prologue => "doc-prologue",
        PullQuote => "doc-pullquote",
        Qna => "doc-qna",
        Subtitle => "doc-subtitle",
        Tip => "doc-tip",
        Toc => "doc-toc"
    }
}

impl EpubStructuralSemantic {
    /// Returns whether the recognized EPUB term is deprecated by its vocabulary.
    pub fn is_deprecated(self) -> bool {
        matches!(
            self,
            Self::AnnoRef
                | Self::Annotation
                | Self::BiblioEntry
                | Self::Bridgehead
                | Self::Endnote
                | Self::Help
                | Self::Marginalia
                | Self::Note
                | Self::RearNote
                | Self::RearNotes
                | Self::Sidebar
                | Self::Subchapter
                | Self::Warning
        )
    }

    /// Returns the explicitly defined DPUB-ARIA crosswalk, if one exists.
    ///
    /// Similar spelling alone does not imply a relationship.
    pub fn related_dpub_role(self) -> Option<DpubAriaRole> {
        Some(match self {
            Self::Abstract => DpubAriaRole::Abstract,
            Self::Acknowledgments => DpubAriaRole::Acknowledgments,
            Self::Afterword => DpubAriaRole::Afterword,
            Self::Appendix => DpubAriaRole::Appendix,
            Self::Backlink => DpubAriaRole::Backlink,
            Self::BiblioEntry => DpubAriaRole::BiblioEntry,
            Self::Bibliography => DpubAriaRole::Bibliography,
            Self::BiblioRef => DpubAriaRole::BiblioRef,
            Self::Chapter => DpubAriaRole::Chapter,
            Self::Colophon => DpubAriaRole::Colophon,
            Self::Conclusion => DpubAriaRole::Conclusion,
            Self::Cover => DpubAriaRole::Cover,
            Self::Credit => DpubAriaRole::Credit,
            Self::Credits => DpubAriaRole::Credits,
            Self::Dedication => DpubAriaRole::Dedication,
            Self::Endnote => DpubAriaRole::Endnote,
            Self::Endnotes => DpubAriaRole::Endnotes,
            Self::Epigraph => DpubAriaRole::Epigraph,
            Self::Epilogue => DpubAriaRole::Epilogue,
            Self::Errata => DpubAriaRole::Errata,
            Self::Footnote => DpubAriaRole::Footnote,
            Self::Foreword => DpubAriaRole::Foreword,
            Self::Glossary => DpubAriaRole::Glossary,
            Self::GlossRef => DpubAriaRole::GlossRef,
            Self::Index => DpubAriaRole::Index,
            Self::Introduction => DpubAriaRole::Introduction,
            Self::NoteRef => DpubAriaRole::NoteRef,
            Self::Notice => DpubAriaRole::Notice,
            Self::Pagebreak => DpubAriaRole::Pagebreak,
            Self::PageList => DpubAriaRole::PageList,
            Self::Part => DpubAriaRole::Part,
            Self::Preface => DpubAriaRole::Preface,
            Self::Prologue => DpubAriaRole::Prologue,
            Self::PullQuote => DpubAriaRole::PullQuote,
            Self::Qna => DpubAriaRole::Qna,
            Self::Subtitle => DpubAriaRole::Subtitle,
            Self::Tip => DpubAriaRole::Tip,
            Self::Toc => DpubAriaRole::Toc,
            _ => return None,
        })
    }
}

impl FromStr for EpubStructuralSemantic {
    type Err = UnrecognizedTerm;

    /// Parses an exact, case-sensitive canonical token, as compact URL references require.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_token(value).ok_or(UnrecognizedTerm)
    }
}

impl FromStr for DpubAriaRole {
    type Err = UnrecognizedTerm;

    /// Parses a role using HTML's ASCII case-insensitive token semantics.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_html_token(value).ok_or(UnrecognizedTerm)
    }
}

/// Authored text that is not a recognized vocabulary term.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("term is not recognized")]
pub struct UnrecognizedTerm;

impl DpubAriaRole {
    /// Parses a DPUB role using HTML's ASCII case-insensitive token semantics.
    pub fn from_html_token(value: &str) -> Option<Self> {
        // XHTML content is intentionally processed with HTML-compatible semantics.
        Self::ALL
            .iter()
            .copied()
            .find(|role| role.as_str().eq_ignore_ascii_case(value))
    }

    /// Returns whether the recognized DPUB-ARIA role is deprecated.
    pub fn is_deprecated(self) -> bool {
        matches!(self, Self::BiblioEntry | Self::Endnote)
    }

    /// Returns the explicitly defined EPUB semantics crosswalk, if one exists.
    ///
    /// Similar spelling alone does not imply a relationship.
    pub fn related_epub_semantic(self) -> Option<EpubStructuralSemantic> {
        Some(match self {
            Self::Abstract => EpubStructuralSemantic::Abstract,
            Self::Acknowledgments => EpubStructuralSemantic::Acknowledgments,
            Self::Afterword => EpubStructuralSemantic::Afterword,
            Self::Appendix => EpubStructuralSemantic::Appendix,
            Self::Backlink => EpubStructuralSemantic::Backlink,
            Self::BiblioEntry => EpubStructuralSemantic::BiblioEntry,
            Self::Bibliography => EpubStructuralSemantic::Bibliography,
            Self::BiblioRef => EpubStructuralSemantic::BiblioRef,
            Self::Chapter => EpubStructuralSemantic::Chapter,
            Self::Colophon => EpubStructuralSemantic::Colophon,
            Self::Conclusion => EpubStructuralSemantic::Conclusion,
            Self::Cover => EpubStructuralSemantic::Cover,
            Self::Credit => EpubStructuralSemantic::Credit,
            Self::Credits => EpubStructuralSemantic::Credits,
            Self::Dedication => EpubStructuralSemantic::Dedication,
            Self::Endnote => EpubStructuralSemantic::Endnote,
            Self::Endnotes => EpubStructuralSemantic::Endnotes,
            Self::Epigraph => EpubStructuralSemantic::Epigraph,
            Self::Epilogue => EpubStructuralSemantic::Epilogue,
            Self::Errata => EpubStructuralSemantic::Errata,
            Self::Footnote => EpubStructuralSemantic::Footnote,
            Self::Foreword => EpubStructuralSemantic::Foreword,
            Self::Glossary => EpubStructuralSemantic::Glossary,
            Self::GlossRef => EpubStructuralSemantic::GlossRef,
            Self::Index => EpubStructuralSemantic::Index,
            Self::Introduction => EpubStructuralSemantic::Introduction,
            Self::NoteRef => EpubStructuralSemantic::NoteRef,
            Self::Notice => EpubStructuralSemantic::Notice,
            Self::Pagebreak => EpubStructuralSemantic::Pagebreak,
            Self::PageList => EpubStructuralSemantic::PageList,
            Self::Part => EpubStructuralSemantic::Part,
            Self::Preface => EpubStructuralSemantic::Preface,
            Self::Prologue => EpubStructuralSemantic::Prologue,
            Self::PullQuote => EpubStructuralSemantic::PullQuote,
            Self::Qna => EpubStructuralSemantic::Qna,
            Self::Subtitle => EpubStructuralSemantic::Subtitle,
            Self::Tip => EpubStructuralSemantic::Tip,
            Self::Toc => EpubStructuralSemantic::Toc,
            Self::Example | Self::PageFooter | Self::PageHeader => return None,
        })
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for HeadingLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let level = u8::deserialize(deserializer)?;
        Self::new(level).ok_or_else(|| serde::de::Error::custom("heading level is outside 1 to 6"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_parsing_preserves_source_specific_meaning() {
        assert_eq!(EpubStructuralSemantic::ALL.len(), 127);
        assert_eq!(DpubAriaRole::ALL.len(), 41);
        assert_eq!(
            EpubStructuralSemantic::from_token("page-list"),
            Some(EpubStructuralSemantic::PageList)
        );
        assert_eq!(EpubStructuralSemantic::from_token("Page-List"), None);
        assert_eq!(
            DpubAriaRole::from_html_token("DOC-PAGELIST"),
            Some(DpubAriaRole::PageList)
        );
        assert_eq!(DpubAriaRole::PageList.as_str(), "doc-pagelist");

        for semantic in EpubStructuralSemantic::ALL {
            assert_eq!(
                EpubStructuralSemantic::from_token(semantic.as_str()),
                Some(*semantic)
            );
        }
        for role in DpubAriaRole::ALL {
            assert_eq!(DpubAriaRole::from_token(role.as_str()), Some(*role));
            assert_eq!(
                DpubAriaRole::from_html_token(&role.as_str().to_ascii_uppercase()),
                Some(*role)
            );
        }
    }

    #[test]
    fn crosswalk_is_explicit_and_does_not_infer_similar_names() {
        assert_eq!(
            EpubStructuralSemantic::PageList.related_dpub_role(),
            Some(DpubAriaRole::PageList)
        );
        assert_eq!(EpubStructuralSemantic::Example.related_dpub_role(), None);
        assert_eq!(DpubAriaRole::Example.related_epub_semantic(), None);
        assert_eq!(DpubAriaRole::PageHeader.related_epub_semantic(), None);
    }

    #[test]
    fn deprecated_terms_remain_recognized() {
        assert!(EpubStructuralSemantic::Endnote.is_deprecated());
        assert!(DpubAriaRole::Endnote.is_deprecated());
        assert!(!EpubStructuralSemantic::Endnotes.is_deprecated());
    }
}
