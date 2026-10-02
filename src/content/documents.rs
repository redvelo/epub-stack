use super::facts::{FormFact, FragmentFact, MediaFact, ScriptFact, StructureFact, ViewportFact};
use super::text::TextStream;
use crate::media_overlay::SmilFacts;

/// Things in a document that can do something on their own: media, scripts, embedded content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentActivity {
    /// Audio, video, or their source and track elements.
    Media,
    /// An iframe, object, or embed element.
    EmbeddedContent,
    /// A script element, including a non-executable data block.
    ScriptElement,
    /// Declarative SVG animation.
    Animation,
    /// An autofocus attribute.
    Autofocus,
    /// An attribute whose local name starts with `on`.
    EventAttribute,
    /// A refresh metadata directive.
    Refresh,
    /// A speculative resource-loading link relation.
    ResourceHint,
    /// A standalone SVG document type declaration.
    Doctype,
}

impl DocumentActivity {
    const ALL: [Self; 9] = [
        Self::Media,
        Self::EmbeddedContent,
        Self::ScriptElement,
        Self::Animation,
        Self::Autofocus,
        Self::EventAttribute,
        Self::Refresh,
        Self::ResourceHint,
        Self::Doctype,
    ];

    fn bit(self) -> u16 {
        1 << Self::ALL
            .iter()
            .position(|activity| *activity == self)
            .expect("every activity is listed")
    }
}

/// Which of those a document actually contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct DocumentActivities(u16);

impl DocumentActivities {
    /// Returns whether the construct was observed.
    pub fn contains(self, activity: DocumentActivity) -> bool {
        self.0 & activity.bit() != 0
    }

    /// Returns whether no construct was observed.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterates observed constructs in declaration order.
    pub fn iter(self) -> impl Iterator<Item = DocumentActivity> {
        DocumentActivity::ALL
            .into_iter()
            .filter(move |activity| self.contains(*activity))
    }

    pub(crate) fn insert(&mut self, activity: DocumentActivity) {
        self.0 |= activity.bit();
    }
}

impl Extend<DocumentActivity> for DocumentActivities {
    fn extend<I: IntoIterator<Item = DocumentActivity>>(&mut self, activities: I) {
        for activity in activities {
            self.insert(activity);
        }
    }
}

/// What was read out of one document: its text and structure, or its narration timings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentFacts {
    /// XHTML semantic content.
    Xhtml(Box<XhtmlFacts>),
    /// Standalone SVG semantic content.
    Svg(Box<SvgFacts>),
    /// SMIL playback content.
    Smil(Box<SmilFacts>),
    /// A stylesheet. Its imports and `url()` values are reported as authored references.
    Css,
}

impl ContentFacts {
    pub(crate) fn from_xhtml(facts: XhtmlFacts) -> Self {
        Self::Xhtml(Box::new(facts))
    }

    pub(crate) fn from_svg(facts: SvgFacts) -> Self {
        Self::Svg(Box::new(facts))
    }

    pub(crate) fn from_smil(facts: SmilFacts) -> Self {
        Self::Smil(Box::new(facts))
    }

    /// Borrows XHTML facts when this is XHTML content.
    pub fn as_xhtml(&self) -> Option<&XhtmlFacts> {
        match self {
            Self::Xhtml(facts) => Some(facts),
            Self::Svg(_) | Self::Smil(_) | Self::Css => None,
        }
    }

    /// Borrows SVG facts when this is standalone SVG content.
    pub fn as_svg(&self) -> Option<&SvgFacts> {
        match self {
            Self::Svg(facts) => Some(facts),
            Self::Xhtml(_) | Self::Smil(_) | Self::Css => None,
        }
    }

    /// Borrows SMIL facts when this is SMIL content.
    pub fn as_smil(&self) -> Option<&SmilFacts> {
        match self {
            Self::Smil(facts) => Some(facts),
            Self::Xhtml(_) | Self::Svg(_) | Self::Css => None,
        }
    }

    pub(crate) fn as_smil_mut(&mut self) -> Option<&mut SmilFacts> {
        match self {
            Self::Smil(facts) => Some(facts),
            Self::Xhtml(_) | Self::Svg(_) | Self::Css => None,
        }
    }

    /// Returns whether supported document content contains a known executable script occurrence.
    ///
    /// XHTML and standalone SVG are supported, and SVG includes scripts in nested `foreignObject`
    /// XHTML. SMIL and CSS return `None` because this projection does not apply to them. An inline
    /// script without text never runs, so it is not counted.
    pub fn executable_content_detected(&self) -> Option<bool> {
        match self {
            Self::Xhtml(facts) => Some(facts.has_executable_content()),
            Self::Svg(facts) => Some(facts.has_executable_content()),
            Self::Smil(_) | Self::Css => None,
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
    pub(crate) activity: DocumentActivities,
}

impl SvgFacts {
    pub(crate) fn new(
        fragments: Vec<FragmentFact>,
        text: Vec<SvgTextFact>,
        scripts: Vec<ScriptFact>,
        foreign_objects: Vec<SvgForeignObjectFact>,
        activity: DocumentActivities,
    ) -> Self {
        Self {
            fragments,
            text,
            scripts,
            foreign_objects,
            activity,
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

    /// Returns whether the document, including nested `foreignObject` XHTML, contains a known
    /// executable script occurrence.
    ///
    /// An inline script without text never runs, so it is not counted.
    pub fn has_executable_content(&self) -> bool {
        self.scripts.iter().any(script_runs)
            || self
                .foreign_objects
                .iter()
                .any(|foreign| foreign.xhtml.has_executable_content())
    }

    /// Returns observed activity constructs in this SVG document, excluding nested XHTML.
    pub fn activity(&self) -> DocumentActivities {
        self.activity
    }
}

/// XHTML facts nested in one standalone SVG `foreignObject` occurrence.
///
/// Nested foreign objects remain separate occurrences rather than being flattened into their
/// containing XHTML projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgForeignObjectFact {
    pub(crate) fragment: Option<FragmentFact>,
    pub(crate) xhtml: XhtmlFacts,
}

impl SvgForeignObjectFact {
    /// Returns the nearest authored SVG fragment for the occurrence.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        self.fragment.as_ref()
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
    pub(crate) fragment: Option<FragmentFact>,
}

impl SvgTextFact {
    /// Returns the extracted text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the nearest authored fragment when recorded.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        self.fragment.as_ref()
    }
}

/// Source text, fragment targets, viewport metadata, structure, media, forms, and scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XhtmlFacts {
    pub(crate) fragments: Vec<FragmentFact>,
    pub(crate) viewports: Vec<ViewportFact>,
    pub(crate) text_stream: TextStream,
    pub(crate) structure: Vec<StructureFact>,
    pub(crate) media: Vec<MediaFact>,
    pub(crate) forms: Vec<FormFact>,
    pub(crate) scripts: Vec<ScriptFact>,
    pub(crate) is_document: bool,
    pub(crate) activity: DocumentActivities,
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

    /// Returns source-order text with overlapping spans and supplementary attribute text.
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

    /// Reports whether extraction recognized a normalized XHTML document rather than a fragment.
    pub fn is_document(&self) -> bool {
        self.is_document
    }

    /// Returns whether the document contains a known executable script occurrence.
    ///
    /// An inline script without text never runs, so it is not counted.
    pub fn has_executable_content(&self) -> bool {
        self.scripts.iter().any(script_runs)
    }

    /// Returns activity constructs observed in normalized source content.
    pub fn activity(&self) -> DocumentActivities {
        self.activity
    }

    /// Returns scripts and event handlers in source order.
    pub fn scripts(&self) -> &[ScriptFact] {
        &self.scripts
    }
}

fn script_runs(script: &ScriptFact) -> bool {
    script.is_executable()
        && !matches!(
            script,
            ScriptFact::Inline {
                has_text: false,
                ..
            }
        )
}
