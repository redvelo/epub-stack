use super::facts::{FormFact, FragmentFact, MediaFact, ScriptFact, StructureFact, ViewportFact};
use super::text::{TextChunk, TextStream};
use crate::media_overlay::SmilFacts;

/// Text, structure, and playback facts extracted from a supported content resource.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum ContentFacts {
    /// XHTML text, fragments, viewport metadata, structure, media, forms, and scripts.
    Xhtml(XhtmlFacts),
    /// Standalone SVG semantic facts.
    Svg(SvgFacts),
    /// SMIL semantic facts.
    Smil(SmilFacts),
}

impl ContentFacts {
    /// Borrows XHTML facts when this is XHTML content.
    pub fn as_xhtml(&self) -> Option<&XhtmlFacts> {
        match self {
            Self::Xhtml(facts) => Some(facts),
            Self::Svg(_) | Self::Smil(_) => None,
        }
    }

    /// Borrows SVG facts when this is standalone SVG content.
    pub fn as_svg(&self) -> Option<&SvgFacts> {
        match self {
            Self::Svg(facts) => Some(facts),
            Self::Xhtml(_) | Self::Smil(_) => None,
        }
    }

    /// Borrows SMIL facts when this is SMIL content.
    pub fn as_smil(&self) -> Option<&SmilFacts> {
        match self {
            Self::Smil(facts) => Some(facts),
            Self::Xhtml(_) | Self::Svg(_) => None,
        }
    }

    /// Returns whether supported document content contains a known executable script occurrence.
    ///
    /// XHTML and standalone SVG are supported. SVG includes scripts in nested `foreignObject`
    /// XHTML; SMIL returns `None` because this projection does not apply to it.
    pub fn executable_content_detected(&self) -> Option<bool> {
        match self {
            Self::Xhtml(facts) => Some(facts.has_executable_content()),
            Self::Svg(facts) => Some(facts.has_executable_content()),
            Self::Smil(_) => None,
        }
    }
}

/// Fragment targets extracted from a standalone SVG resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgFacts {
    pub(crate) fragments: Vec<FragmentFact>,
    pub(crate) text: Vec<SvgTextFact>,
    pub(crate) scripts: Vec<ScriptFact>,
    pub(crate) foreign_objects: Vec<SvgForeignObjectFact>,
    pub(crate) foreground_preparation_hazard: bool,
}

impl SvgFacts {
    pub(crate) fn new(
        fragments: Vec<FragmentFact>,
        text: Vec<SvgTextFact>,
        scripts: Vec<ScriptFact>,
        script_has_text: Vec<bool>,
        foreign_objects: Vec<SvgForeignObjectFact>,
        foreground_preparation_hazard: bool,
    ) -> Self {
        let scripts = scripts
            .into_iter()
            .enumerate()
            .map(|(index, script)| match script {
                ScriptFact::Inline {
                    fragment,
                    script_type,
                    has_text,
                } if !super::extraction::xhtml::is_executable_script_type(
                    script_type.as_deref(),
                ) =>
                {
                    ScriptFact::DataBlock {
                        fragment,
                        script_type,
                        has_text,
                    }
                }
                ScriptFact::External {
                    fragment,
                    script_type,
                } if !super::extraction::xhtml::is_executable_script_type(
                    script_type.as_deref(),
                ) =>
                {
                    ScriptFact::DataBlock {
                        fragment,
                        script_type,
                        has_text: script_has_text.get(index).copied().unwrap_or(false),
                    }
                }
                script => script,
            })
            .collect();
        Self {
            fragments,
            text,
            scripts,
            foreign_objects,
            foreground_preparation_hazard,
        }
    }

    /// Returns authored fragment declarations in source order.
    pub fn fragments(&self) -> &[FragmentFact] {
        &self.fragments
    }

    /// Returns source-order text from SVG `text` elements.
    pub fn text(&self) -> &[SvgTextFact] {
        &self.text
    }

    /// Returns inline, external, and event-handler script occurrences in source order.
    pub fn scripts(&self) -> &[ScriptFact] {
        &self.scripts
    }

    /// Returns XHTML-bearing SVG `foreignObject` occurrences in source order.
    pub fn foreign_objects(&self) -> &[SvgForeignObjectFact] {
        &self.foreign_objects
    }

    fn has_executable_content(&self) -> bool {
        self.scripts.iter().any(ScriptFact::is_executable)
            || self
                .foreign_objects
                .iter()
                .any(|foreign| foreign.xhtml.has_executable_content())
    }

    pub(crate) fn supports_foreground_preparation(&self) -> bool {
        !self.foreground_preparation_hazard
            && !self.scripts.iter().any(ScriptFact::is_executable)
            && self
                .foreign_objects
                .iter()
                .all(|foreign| foreign.xhtml.supports_foreground_preparation_fragment())
    }
}

/// XHTML facts nested in one standalone SVG `foreignObject` occurrence.
///
/// Nested foreign objects remain separate occurrences rather than being flattened into their
/// containing XHTML projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgForeignObjectFact {
    pub(crate) fragment: Option<String>,
    pub(crate) xhtml: XhtmlFacts,
}

impl SvgForeignObjectFact {
    /// Returns the nearest authored SVG fragment identifier for the occurrence.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Returns facts projected from XHTML descendants of the foreign object.
    pub fn xhtml(&self) -> &XhtmlFacts {
        &self.xhtml
    }
}

/// Source text recovered from one standalone SVG `text` element.
///
/// This is authored source text, not browser-rendered or geometrically ordered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgTextFact {
    pub(crate) text: String,
    pub(crate) fragment: Option<String>,
}

impl SvgTextFact {
    /// Returns the extracted text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
}

/// Searchable text, fragment targets, viewport metadata, structure, media, forms, and scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XhtmlFacts {
    pub(crate) fragments: Vec<FragmentFact>,
    pub(crate) viewports: Vec<ViewportFact>,
    pub(crate) text_stream: TextStream,
    pub(crate) text: Vec<TextChunk>,
    pub(crate) structure: Vec<StructureFact>,
    pub(crate) media: Vec<MediaFact>,
    pub(crate) forms: Vec<FormFact>,
    pub(crate) scripts: Vec<ScriptFact>,
    pub(crate) foreground_preparation_document_supported: bool,
    pub(crate) foreground_preparation_hazard_detected: bool,
}

impl XhtmlFacts {
    /// Returns authored fragment declarations in source order.
    pub fn fragments(&self) -> &[FragmentFact] {
        &self.fragments
    }

    /// Returns authored viewport metadata declarations in source order.
    pub fn viewports(&self) -> &[ViewportFact] {
        &self.viewports
    }

    /// Returns searchable text chunks in extracted reading order.
    pub fn text(&self) -> &[TextChunk] {
        &self.text
    }

    /// Returns the source-order text stream used by ranged chunks.
    pub fn text_stream(&self) -> &TextStream {
        &self.text_stream
    }

    /// Returns extracted structural occurrences in source order.
    pub fn structure(&self) -> &[StructureFact] {
        &self.structure
    }

    /// Returns media occurrences in source order.
    pub fn media(&self) -> &[MediaFact] {
        &self.media
    }

    /// Returns forms and controls in source order.
    pub fn forms(&self) -> &[FormFact] {
        &self.forms
    }

    pub(crate) fn supports_foreground_preparation(&self) -> bool {
        self.foreground_preparation_document_supported
            && self.supports_foreground_preparation_fragment()
    }

    fn has_executable_content(&self) -> bool {
        self.scripts.iter().any(ScriptFact::is_executable)
    }

    fn supports_foreground_preparation_fragment(&self) -> bool {
        !self.foreground_preparation_hazard_detected && !self.has_executable_content()
    }

    /// Returns scripts and event handlers in source order.
    pub fn scripts(&self) -> &[ScriptFact] {
        &self.scripts
    }
}
