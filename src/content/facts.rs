use crate::resource::AuthoredHref;
use crate::semantics::{HeadingLevel, SemanticToken};

/// One authored HTML viewport metadata declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportFact {
    pub(crate) content: Option<String>,
    pub(crate) directives: Vec<ViewportDirectiveFact>,
}

impl ViewportFact {
    pub(crate) fn new(content: Option<String>) -> Self {
        let directives = content
            .as_deref()
            .map(viewport_directives)
            .unwrap_or_default();
        Self {
            content,
            directives,
        }
    }

    /// Returns the parser-exposed authored `content` attribute, if present.
    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    /// Returns authored separator-delimited components in source order.
    pub fn directives(&self) -> &[ViewportDirectiveFact] {
        &self.directives
    }
}

fn viewport_directives(content: &str) -> Vec<ViewportDirectiveFact> {
    let mut directives = Vec::new();
    for punctuated in content.split([',', ';']) {
        let mut start = 0;
        let mut saw_assignment = false;
        let mut saw_value = false;
        for (index, ch) in punctuated.char_indices() {
            if index < start {
                continue;
            }
            if ch == '=' {
                saw_assignment = true;
                continue;
            }
            if ch != ' ' {
                saw_value |= saw_assignment;
                continue;
            }
            if punctuated[start..index].bytes().all(|byte| byte == b' ') {
                continue;
            }

            let next = index
                + punctuated[index..]
                    .bytes()
                    .take_while(|byte| *byte == b' ')
                    .count();
            let next_character = punctuated[next..].chars().next();
            let separates_properties =
                next_character.is_some_and(|next| next != '=' && (!saw_assignment || saw_value));
            if separates_properties {
                directives.push(ViewportDirectiveFact::from_authored(
                    &punctuated[start..index],
                ));
                start = next;
                saw_assignment = false;
                saw_value = false;
            }
        }
        directives.push(ViewportDirectiveFact::from_authored(&punctuated[start..]));
    }
    directives
}

/// One separator-delimited component of authored viewport metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportDirectiveFact {
    raw_name: String,
    raw_value: Option<String>,
}

impl ViewportDirectiveFact {
    fn from_authored(component: &str) -> Self {
        let (raw_name, raw_value) = component
            .split_once('=')
            .map_or((component, None), |(name, value)| (name, Some(value)));
        Self {
            raw_name: raw_name.to_string(),
            raw_value: raw_value.map(str::to_string),
        }
    }

    /// Returns the authored component before its first `=`, including whitespace.
    pub fn raw_name(&self) -> &str {
        &self.raw_name
    }

    /// Returns the authored component after its first `=`, including whitespace.
    pub fn raw_value(&self) -> Option<&str> {
        self.raw_value.as_deref()
    }

    /// Recognizes a supported directive after trimming its name.
    pub fn directive(&self) -> Option<ViewportDirective> {
        let name = self.raw_name.trim();
        if name.eq_ignore_ascii_case("width") {
            Some(ViewportDirective::Width)
        } else if name.eq_ignore_ascii_case("height") {
            Some(ViewportDirective::Height)
        } else if name.eq_ignore_ascii_case("initial-scale") {
            Some(ViewportDirective::InitialScale)
        } else {
            None
        }
    }

    /// Parses the trimmed authored value when it is a finite number.
    pub fn numeric_value(&self) -> Option<f64> {
        self.raw_value
            .as_deref()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
    }
}

/// A recognized viewport directive name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewportDirective {
    /// The layout viewport width.
    Width,
    /// The layout viewport height.
    Height,
    /// The initial zoom scale.
    InitialScale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkFact {
    Hyperlink(LinkFactData),
    Stylesheet(LinkFactData),
    FormAction(LinkFactData),
    Image(LinkFactData),
    Script(LinkFactData),
    Audio(LinkFactData),
    Video(LinkFactData),
    Track(LinkFactData),
    Poster(LinkFactData),
    Object(LinkFactData),
    Embed(LinkFactData),
    Iframe(LinkFactData),
    SvgReference(LinkFactData),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkFactData {
    pub(crate) declared: AuthoredHref,
    pub(crate) element: String,
    pub(crate) attribute: ReferenceAttribute,
    pub(crate) navigation: Option<NavigationLinkKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigationLinkKind {
    Toc,
    PageList,
    Landmark,
}

impl LinkFactData {
    pub fn declared(&self) -> &AuthoredHref {
        &self.declared
    }

    pub fn element(&self) -> &str {
        &self.element
    }

    pub fn attribute(&self) -> &ReferenceAttribute {
        &self.attribute
    }

    pub(crate) fn navigation(&self) -> Option<NavigationLinkKind> {
        self.navigation
    }
}

impl LinkFact {
    fn data(&self) -> &LinkFactData {
        match self {
            Self::Hyperlink(data)
            | Self::Stylesheet(data)
            | Self::FormAction(data)
            | Self::Image(data)
            | Self::Script(data)
            | Self::Audio(data)
            | Self::Video(data)
            | Self::Track(data)
            | Self::Poster(data)
            | Self::Object(data)
            | Self::Embed(data)
            | Self::Iframe(data)
            | Self::SvgReference(data) => data,
        }
    }

    pub fn declared(&self) -> &AuthoredHref {
        self.data().declared()
    }

    pub fn element(&self) -> &str {
        self.data().element()
    }

    pub fn attribute(&self) -> &ReferenceAttribute {
        self.data().attribute()
    }

    pub(crate) fn navigation(&self) -> Option<NavigationLinkKind> {
        self.data().navigation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ReferenceAttribute {
    Href,
    Action,
    FormAction,
    Src,
    Srcset,
    Poster,
    Data,
    XlinkHref,
}

impl ReferenceAttribute {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Href => "href",
            Self::Action => "action",
            Self::FormAction => "formaction",
            Self::Src => "src",
            Self::Srcset => "srcset",
            Self::Poster => "poster",
            Self::Data => "data",
            Self::XlinkHref => "xlink:href",
        }
    }
}

/// A fragment declaration extracted from one content resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentFact {
    id: String,
    element: String,
    attribute: FragmentAttribute,
    element_ordinal: usize,
}

/// The authored attribute that declared a fragment identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentAttribute {
    /// An unqualified HTML or SVG `id` attribute.
    Id,
    /// An `id` attribute in the XML namespace.
    XmlId,
}

impl FragmentFact {
    pub(crate) fn new(
        id: String,
        element: String,
        attribute: FragmentAttribute,
        element_ordinal: usize,
    ) -> Self {
        Self {
            id,
            element,
            attribute,
            element_ordinal,
        }
    }

    /// Returns the authored fragment identifier without a leading `#`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the declaring element's local name.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns which authored attribute declared the identifier.
    pub fn attribute(&self) -> FragmentAttribute {
        self.attribute
    }

    /// Returns the extraction-local position of the declaring element, not a persistent
    /// identifier.
    pub fn element_ordinal(&self) -> usize {
        self.element_ordinal
    }
}

/// A heading, page break, figure, table, note, navigation list, or section from XHTML.
///
/// One source element can produce multiple facts when its native element and authored
/// semantics establish overlapping categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureFact {
    pub(crate) role: StructureRole,
    pub(crate) label: Option<String>,
    pub(crate) fragment: Option<FragmentFact>,
    pub(crate) semantics: Vec<SemanticToken>,
}

/// The structural category of a [`StructureFact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructureRole {
    /// An HTML heading with its constrained level.
    Heading(HeadingLevel),
    /// An authored EPUB or DPUB page break.
    Pagebreak,
    /// An HTML figure.
    Figure,
    /// An HTML table.
    Table,
    /// An authored EPUB or DPUB footnote.
    Footnote,
    /// An authored EPUB or DPUB endnote.
    Endnote,
    /// An authored generic note.
    Note,
    /// An HTML navigation list.
    NavigationList,
    /// A publication section identified by EPUB or DPUB semantics.
    PublicationSection,
}

impl StructureRole {
    fn is_labeled(self) -> bool {
        matches!(
            self,
            Self::Heading(_) | Self::Pagebreak | Self::Figure | Self::Table
        )
    }
}

impl StructureFact {
    pub(crate) fn new(
        role: StructureRole,
        label: Option<String>,
        fragment: Option<FragmentFact>,
        semantics: Vec<SemanticToken>,
    ) -> Self {
        Self {
            role,
            label: label.filter(|_| role.is_labeled()),
            fragment,
            semantics,
        }
    }

    /// Returns the structural category.
    pub fn role(&self) -> StructureRole {
        self.role
    }

    /// Returns extracted heading text, page label, or figure or table caption.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the nearest authored fragment when recorded.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        self.fragment.as_ref()
    }

    /// Returns the authored and native tokens that established this structure.
    pub fn semantics(&self) -> &[SemanticToken] {
        &self.semantics
    }

    pub(crate) fn set_label(&mut self, label: Option<String>) {
        if self.role.is_labeled() {
            self.label = label;
        }
    }
}

/// A media occurrence extracted from XHTML.
///
/// Use [`crate::analysis::ResourceAnalysisRef::xhtml_media`] to retrieve each occurrence with its
/// authored `src` and `srcset` links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaFact {
    /// An HTML or embedded SVG image, including image inputs.
    #[non_exhaustive]
    Image {
        /// The source element's local name, including image inputs.
        element: String,
        /// The authored alternative text when present.
        alt: Option<String>,
    },
    /// An HTML audio element.
    Audio,
    /// An HTML video element.
    Video,
    /// A source candidate with its parent media context.
    #[non_exhaustive]
    Source {
        /// The recognized parent media context.
        context: MediaSourceContext,
    },
    /// A timed-text track and its authored descriptive attributes.
    #[non_exhaustive]
    Track {
        /// The authored track kind.
        kind: Option<TrackKind>,
        /// The authored source language.
        srclang: Option<String>,
        /// The authored user-facing label.
        label: Option<String>,
    },
    /// An authored video poster attribute.
    Poster,
}

/// The authored `kind` of an HTML `track` element.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// `subtitles`.
    Subtitles,
    /// `captions`.
    Captions,
    /// `descriptions`.
    Descriptions,
    /// `chapters`.
    Chapters,
    /// `metadata`.
    Metadata,
    /// An unrecognized value, retained as authored.
    Other(String),
}

impl TrackKind {
    pub(crate) fn from_attribute(value: &str) -> Self {
        [
            ("subtitles", Self::Subtitles),
            ("captions", Self::Captions),
            ("descriptions", Self::Descriptions),
            ("chapters", Self::Chapters),
            ("metadata", Self::Metadata),
        ]
        .into_iter()
        .find_map(|(token, kind)| token.eq_ignore_ascii_case(value).then_some(kind))
        .unwrap_or_else(|| Self::Other(value.to_string()))
    }
}

/// The containing media context of an HTML source element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaSourceContext {
    /// A source inside a picture element.
    Picture,
    /// A source inside an audio element.
    Audio,
    /// A source inside a video element.
    Video,
    /// A source without a recognized media parent.
    Other,
}

impl MediaFact {
    /// Returns the media element's local name.
    pub fn element(&self) -> &str {
        match self {
            Self::Image { element, .. } => element,
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Source { .. } => "source",
            Self::Track { .. } => "track",
            Self::Poster => "video",
        }
    }
}

/// An XHTML form or form-control occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormFact {
    /// A form with its optional submission method.
    #[non_exhaustive]
    Form {
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The normalized authored submission method.
        method: Option<String>,
    },
    /// An input, button, select, or textarea control.
    #[non_exhaustive]
    Control {
        /// The control element's local name.
        element: String,
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The authored control type.
        control_type: Option<String>,
        /// The authored control name.
        name: Option<String>,
        /// The authored control value.
        value: Option<String>,
    },
}

impl FormFact {
    /// Returns the form or control element's local name.
    pub fn element(&self) -> &str {
        match self {
            Self::Form { .. } => "form",
            Self::Control { element, .. } => element,
        }
    }

    /// Returns the nearest authored fragment when recorded.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        match self {
            Self::Form { fragment, .. } | Self::Control { fragment, .. } => fragment.as_ref(),
        }
    }
}

/// An executable script, data block, or event-handler occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptFact {
    /// An executable external script whose source link is available from analysis.
    #[non_exhaustive]
    External {
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The authored script MIME type.
        script_type: Option<String>,
    },
    /// An executable inline script.
    #[non_exhaustive]
    Inline {
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The authored script MIME type.
        script_type: Option<String>,
        /// Whether the script contains non-whitespace text.
        has_text: bool,
    },
    /// A non-executable script data block.
    #[non_exhaustive]
    DataBlock {
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The authored script MIME type.
        script_type: Option<String>,
        /// Whether the block contains non-whitespace text.
        has_text: bool,
    },
    /// A recognized executable event-handler content attribute.
    #[non_exhaustive]
    EventHandler {
        /// The source element's local name.
        element: String,
        /// The nearest authored fragment.
        fragment: Option<FragmentFact>,
        /// The authored event-handler attribute name.
        attribute: String,
    },
}

impl ScriptFact {
    pub(crate) fn mark_text(&mut self) {
        if let Self::Inline { has_text, .. } | Self::DataBlock { has_text, .. } = self {
            *has_text = true;
        }
    }

    /// Returns the nearest authored fragment when recorded.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        match self {
            Self::External { fragment, .. }
            | Self::Inline { fragment, .. }
            | Self::DataBlock { fragment, .. }
            | Self::EventHandler { fragment, .. } => fragment.as_ref(),
        }
    }

    /// Returns the authored script MIME type when applicable.
    pub fn script_type(&self) -> Option<&str> {
        match self {
            Self::External { script_type, .. }
            | Self::Inline { script_type, .. }
            | Self::DataBlock { script_type, .. } => script_type.as_deref(),
            Self::EventHandler { .. } => None,
        }
    }

    /// Returns whether the occurrence can execute script.
    pub fn is_executable(&self) -> bool {
        matches!(
            self,
            Self::External { .. } | Self::Inline { .. } | Self::EventHandler { .. }
        )
    }

    /// Returns whether inline or data-block script text was nonempty.
    pub fn has_text(&self) -> Option<bool> {
        match self {
            Self::Inline { has_text, .. } | Self::DataBlock { has_text, .. } => Some(*has_text),
            Self::External { .. } | Self::EventHandler { .. } => None,
        }
    }

    /// Returns the source element's local name.
    pub fn element(&self) -> &str {
        match self {
            Self::External { .. } | Self::Inline { .. } | Self::DataBlock { .. } => "script",
            Self::EventHandler { element, .. } => element,
        }
    }

    /// Returns the event-handler attribute name when applicable.
    pub fn attribute(&self) -> Option<&str> {
        match self {
            Self::EventHandler { attribute, .. } => Some(attribute),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct XhtmlLinkSlot(usize);

impl XhtmlLinkSlot {
    pub(crate) fn new(slot: usize) -> Self {
        Self(slot)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LinkConstructors {
    pub(crate) fact: fn(LinkFactData) -> LinkFact,
}

impl LinkConstructors {
    pub(crate) const HYPERLINK: Self = Self {
        fact: LinkFact::Hyperlink,
    };
    pub(crate) const STYLESHEET: Self = Self {
        fact: LinkFact::Stylesheet,
    };
    pub(crate) const FORM_ACTION: Self = Self {
        fact: LinkFact::FormAction,
    };
    pub(crate) const IMAGE: Self = Self {
        fact: LinkFact::Image,
    };
    pub(crate) const SCRIPT: Self = Self {
        fact: LinkFact::Script,
    };
    pub(crate) const AUDIO: Self = Self {
        fact: LinkFact::Audio,
    };
    pub(crate) const VIDEO: Self = Self {
        fact: LinkFact::Video,
    };
    pub(crate) const TRACK: Self = Self {
        fact: LinkFact::Track,
    };
    pub(crate) const POSTER: Self = Self {
        fact: LinkFact::Poster,
    };
    pub(crate) const OBJECT: Self = Self {
        fact: LinkFact::Object,
    };
    pub(crate) const EMBED: Self = Self {
        fact: LinkFact::Embed,
    };
    pub(crate) const IFRAME: Self = Self {
        fact: LinkFact::Iframe,
    };
    pub(crate) const SVG_REFERENCE: Self = Self {
        fact: LinkFact::SvgReference,
    };
}

#[cfg(test)]
mod tests {
    use crate::accessibility::{AccessibilityFact, AccessibilityObservation};
    use crate::content::extraction::xhtml::{
        EVENT_HANDLER_ATTRIBUTES, is_event_handler_attr, is_executable_script_type,
        parse_srcset_candidates,
    };
    use crate::content::text::{
        SupplementaryTextSource, TextRange, TextRangeError, TextRole, TextSpanRef,
    };
    use crate::content::*;
    use crate::semantics::{DpubAriaRole, EpubStructuralSemantic, HeadingLevel, TextDirection};
    use crate::semantics::{HtmlStructuralElement, SemanticToken};
    fn analyze(xml: &str) -> XhtmlExtraction {
        let mut reader = std::io::Cursor::new(xml.as_bytes());
        parse_xhtml_document_from_reader_counted(&mut reader)
            .expect("in-memory XHTML fixture should parse")
            .0
    }

    fn spans(facts: &XhtmlFacts) -> Vec<TextSpanRef<'_>> {
        facts
            .text_stream()
            .spans()
            .filter(|span| span.role() != TextRole::Element)
            .collect()
    }

    #[test]
    fn character_references_are_decoded_once_across_reader_boundaries() {
        struct SmallReads<'a>(&'a [u8]);
        impl std::io::Read for SmallReads<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = self.0.len().min(output.len()).min(3);
                output[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        let xml = "<html><body><p>Fish &amp; chips &#x1F600; &nbsp; &amp;amp;</p><p>Next</p></body></html>";
        let normal = analyze(xml);
        let split = parse_xhtml_document_from_reader_counted(&mut SmallReads(xml.as_bytes()))
            .unwrap()
            .0;
        assert_eq!(normal.facts, split.facts);
        assert_eq!(
            normal.facts.text_stream().text(),
            "Fish & chips 😀 \u{a0} &amp;\nNext"
        );
        let expected = "Fish & chips 😀 \u{a0} &amp;";
        let span = spans(&normal.facts)[0];
        assert_eq!(span.text(), expected);
        assert_eq!(span.range().end(), expected.chars().count() as u64);
    }

    #[test]
    fn spans_retain_inline_language_and_exact_fragment_provenance() {
        let extraction = analyze(
            r#"<html><body><p id="same" lang="en">Hello <span xml:id="same" lang="fr" dir="rtl">monde</span> again</p><p id="same">Elsewhere</p></body></html>"#,
        );
        let stream = extraction.facts.text_stream();
        assert_eq!(stream.text(), "Hello monde again\nElsewhere");
        let paragraph = stream
            .spans()
            .find(|span| span.origin().element() == "p")
            .unwrap();
        let inline = stream
            .spans()
            .find(|span| span.origin().element() == "span")
            .unwrap();
        assert_eq!(paragraph.text(), "Hello monde again");
        assert_eq!(inline.text(), "monde");
        assert_eq!(inline.origin().lang(), Some("fr"));
        assert_eq!(inline.origin().dir(), Some(TextDirection::Rtl));
        assert_eq!(paragraph.origin().lang(), Some("en"));
        let parent = paragraph.origin().fragment().unwrap();
        let child = inline.origin().fragment().unwrap();
        assert_eq!(parent.attribute(), FragmentAttribute::Id);
        assert_eq!(child.attribute(), FragmentAttribute::XmlId);
        assert_ne!(parent.element_ordinal(), child.element_ordinal());
        assert_eq!(
            stream.text_for_range(inline.range()).unwrap(),
            inline.text()
        );
    }

    #[test]
    fn supplementary_text_retains_its_own_element_in_source_order() {
        let extraction = analyze(
            r#"<html><body><p>Before<img id="image" alt="Fish &amp; chips"/>after<span role="doc-pagebreak" aria-label="Page 2" title="Ignored">2</span></p></body></html>"#,
        );
        let stream = extraction.facts.text_stream();
        assert_eq!(stream.text(), "Beforeafter2");
        let supplementary = stream.supplementary();
        assert_eq!(supplementary.len(), 2);
        assert_eq!(supplementary[0].text(), "Fish & chips");
        assert_eq!(supplementary[0].origin().element(), "img");
        assert_eq!(supplementary[0].origin().fragment().unwrap().id(), "image");
        assert_eq!(
            supplementary[1].source(),
            SupplementaryTextSource::PagebreakTitle
        );
        assert_eq!(supplementary[1].text(), "Ignored");
    }

    #[test]
    fn extracts_ordered_viewport_directives_and_numeric_projections() {
        let extraction = analyze(
            r#"<html><head>
                <meta name="VIEWPORT" content=" width = 1200 ,HEIGHT=800,Initial-Scale = 1.25,width=640" />
                <meta name="viewport" content="height=900" />
                <meta name="viewport" content="width=1000; height=700 initial-scale=1" />
            </head><body></body></html>"#,
        );
        let viewports = extraction.facts.viewports();

        assert_eq!(viewports.len(), 3);
        assert_eq!(
            viewports[0].content(),
            Some(" width = 1200 ,HEIGHT=800,Initial-Scale = 1.25,width=640")
        );
        assert_eq!(
            viewports[0]
                .directives()
                .iter()
                .map(|directive| (
                    directive.raw_name(),
                    directive.raw_value(),
                    directive.directive(),
                    directive.numeric_value(),
                ))
                .collect::<Vec<_>>(),
            [
                (
                    " width ",
                    Some(" 1200 "),
                    Some(ViewportDirective::Width),
                    Some(1200.0)
                ),
                (
                    "HEIGHT",
                    Some("800"),
                    Some(ViewportDirective::Height),
                    Some(800.0)
                ),
                (
                    "Initial-Scale ",
                    Some(" 1.25"),
                    Some(ViewportDirective::InitialScale),
                    Some(1.25)
                ),
                (
                    "width",
                    Some("640"),
                    Some(ViewportDirective::Width),
                    Some(640.0)
                ),
            ]
        );
        assert_eq!(viewports[1].directives()[0].raw_name(), "height");
        assert_eq!(
            viewports[2]
                .directives()
                .iter()
                .map(|directive| directive.directive())
                .collect::<Vec<_>>(),
            [
                Some(ViewportDirective::Width),
                Some(ViewportDirective::Height),
                Some(ViewportDirective::InitialScale),
            ]
        );
    }

    #[test]
    fn viewport_facts_preserve_unknown_malformed_valueless_and_missing_content() {
        let extraction = analyze(
            r#"<html><head>
                <meta name="viewport" content="unknown = device-width,,width,height=,initial-scale=NaN,width=1=2," />
                <meta name="viewport" />
            </head><body></body></html>"#,
        );
        let viewports = extraction.facts.viewports();
        let directives = viewports[0].directives();

        assert_eq!(viewports.len(), 2);
        assert_eq!(directives.len(), 7);
        assert_eq!(
            (directives[0].raw_name(), directives[0].raw_value()),
            ("unknown ", Some(" device-width"))
        );
        assert_eq!(
            (directives[1].raw_name(), directives[1].raw_value()),
            ("", None)
        );
        assert_eq!(
            (directives[2].raw_name(), directives[2].raw_value()),
            ("width", None)
        );
        assert_eq!(
            (directives[3].raw_name(), directives[3].raw_value()),
            ("height", Some(""))
        );
        assert_eq!(
            (directives[5].raw_name(), directives[5].raw_value()),
            ("width", Some("1=2"))
        );
        assert_eq!(
            (directives[6].raw_name(), directives[6].raw_value()),
            ("", None)
        );
        assert_eq!(directives[0].directive(), None);
        assert_eq!(directives[2].directive(), Some(ViewportDirective::Width));
        assert!(
            directives
                .iter()
                .all(|directive| directive.numeric_value().is_none())
        );
        assert_eq!(viewports[1].content(), None);
        assert!(viewports[1].directives().is_empty());
    }

    #[test]
    fn viewport_extraction_requires_html_head_and_does_not_pollute_text() {
        let extraction = analyze(
            r#"<html><head>
                <meta name="viewport" content="width=600" />
                <svg xmlns="http://www.w3.org/2000/svg"><meta name="viewport" content="width=700" /></svg>
            </head><body>
                <meta name="viewport" content="width=800" />
                <p>Visible text</p>
            </body></html>"#,
        );

        assert_eq!(
            extraction
                .facts
                .viewports()
                .iter()
                .map(ViewportFact::content)
                .collect::<Vec<_>>(),
            [Some("width=600")]
        );
        assert_eq!(extraction.facts.text_stream().text(), "Visible text");
        assert_eq!(spans(&extraction.facts).len(), 1);
        assert_eq!(spans(&extraction.facts)[0].text(), "Visible text");
    }

    #[test]
    fn separates_document_text_spans_and_attribute_text() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
                <h1 id="ch1"> Chapter <em>One</em> </h1>
                <p xml:lang="en">Call me <em>Ishmael</em>.</p>
                <a href="other.xhtml#frag">Other</a>
                <img src="../img/pic.jpg" alt="Picture" />
                <span epub:type="pagebreak" id="p1" title="1" />
            </body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(facts.fragments().len(), 2);
        assert_eq!(extraction.links.len(), 2);
        assert!(extraction.accessibility.iter().any(|fact| matches!(
            fact,
            AccessibilityFact {
                observation: AccessibilityObservation::ImageAlt(_),
                ..
            }
        )));
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();
        assert_eq!(text, vec!["Chapter One", "Call me Ishmael."]);
        assert_eq!(
            facts.text_stream().text(),
            "Chapter One\nCall me Ishmael.\nOther"
        );
        assert_eq!(spans(facts)[0].range().start(), 0);
        assert_eq!(
            facts
                .text_stream()
                .supplementary()
                .iter()
                .map(|text| (text.source(), text.text()))
                .collect::<Vec<_>>(),
            [
                (SupplementaryTextSource::Alternative, "Picture"),
                (SupplementaryTextSource::PagebreakTitle, "1")
            ]
        );
    }

    #[test]
    fn semantic_tokens_keep_source_specific_interpretations_and_alt_metadata() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
                <section id="chapter" xml:lang="fr" dir="rtl"
                    epub:type="chapter vendor:experimental"
                    role="doc-chapter custom-role">
                    <img src="cover.jpg" alt=" Illustration de couverture " />
                </section>
            </body></html>"#,
        );
        let facts = &extraction.facts;
        let structure = facts
            .structure()
            .iter()
            .find(|fact| matches!(fact.role(), StructureRole::PublicationSection))
            .expect("chapter section should be extracted");
        let semantics = structure.semantics();

        assert_eq!(
            semantics
                .iter()
                .map(SemanticToken::as_str)
                .collect::<Vec<_>>(),
            vec![
                "chapter",
                "vendor:experimental",
                "doc-chapter",
                "custom-role",
                "section",
            ]
        );
        assert_eq!(
            semantics[0].epub_semantic(),
            Some(EpubStructuralSemantic::Chapter)
        );
        assert_eq!(semantics[1].epub_semantic(), None);
        assert_eq!(semantics[2].dpub_role(), Some(DpubAriaRole::Chapter));
        assert_eq!(semantics[3].dpub_role(), None);
        assert_eq!(
            semantics[4],
            SemanticToken::HtmlElement {
                element: HtmlStructuralElement::Section
            }
        );

        let alt = facts
            .text_stream()
            .supplementary()
            .iter()
            .find(|text| text.source() == SupplementaryTextSource::Alternative)
            .expect("image alternative");
        assert_eq!(alt.text(), "Illustration de couverture");
        assert_eq!(alt.origin().fragment().unwrap().id(), "chapter");
        assert_eq!(alt.origin().lang(), Some("fr"));
        assert_eq!(alt.origin().dir(), Some(TextDirection::Rtl));
    }

    #[test]
    fn alternative_text_is_separate_and_respects_suppression() {
        let extraction = analyze(
            r#"<html><head><img alt="Head" /></head><body>
                <p>Before <img alt="Inline" /> after</p>
                <template><img alt="Template" /></template>
            </body></html>"#,
        );
        let facts = &extraction.facts;
        assert_eq!(facts.text_stream().text(), "Before after");
        assert_eq!(
            facts
                .text_stream()
                .supplementary()
                .iter()
                .map(|text| text.text())
                .collect::<Vec<_>>(),
            ["Inline"]
        );
    }

    #[test]
    fn recognized_dpub_tokens_are_authored_structural_claims() {
        let extraction = analyze(
            r#"<html><body>
                <span role="button doc-pagebreak" title="ignored"></span>
                <span role="unknown doc-pagebreak" title="2"></span>
                <aside id="note" role="doc-footnote"></aside>
            </body></html>"#,
        );
        let structures = extraction.facts.structure();

        assert_eq!(
            structures
                .iter()
                .filter(|fact| matches!(fact.role(), StructureRole::Pagebreak))
                .count(),
            2
        );
        assert_eq!(
            structures
                .iter()
                .filter(|fact| matches!(fact.role(), StructureRole::Footnote))
                .count(),
            1
        );
    }

    #[test]
    fn pagebreak_labels_do_not_fabricate_epub_type_observations() {
        let extraction = analyze(
            r#"<html><body>
                <span role="doc-pagebreak" title="2"></span>
                <span epub:type="pagebreak" title="3"></span>
            </body></html>"#,
        );
        let accessibility = &extraction.accessibility;

        assert_eq!(
            accessibility
                .iter()
                .filter(
                    |fact| matches!(fact, AccessibilityFact { observation: AccessibilityObservation::Role(value), .. } if value == "doc-pagebreak")
                )
                .count(),
            1
        );
        assert_eq!(
            accessibility
                .iter()
                .filter(
                    |fact| matches!(fact, AccessibilityFact { observation: AccessibilityObservation::EpubType(value), .. } if value == "pagebreak")
                )
                .count(),
            1
        );
    }

    #[test]
    fn image_input_alt_text_is_searchable_and_missing_alt_is_observed() {
        let extraction = analyze(
            r#"<html><body><p lang="fr" dir="rtl">Avant
                <input type="IMAGE" alt=" Envoyer " /> après</p>
                <input type="image" />
            </body></html>"#,
        );
        let facts = &extraction.facts;
        let alt = facts
            .text_stream()
            .supplementary()
            .iter()
            .find(|text| text.source() == SupplementaryTextSource::Alternative)
            .expect("image input alternative");

        assert_eq!(alt.text(), "Envoyer");
        assert_eq!(alt.origin().lang(), Some("fr"));
        assert_eq!(alt.origin().dir(), Some(TextDirection::Rtl));
        assert!(extraction.accessibility.iter().any(
            |fact| matches!(fact, AccessibilityFact { element, observation: AccessibilityObservation::MissingImageAlt, .. } if element == "input")
        ));
    }

    #[test]
    fn non_structural_semantic_attributes_remain_raw_accessibility_observations() {
        let extraction = analyze(
            r#"<html><body><span epub:type="keyword" role="doc-noteref">Term</span></body></html>"#,
        );
        let accessibility = &extraction.accessibility;

        assert!(accessibility.iter().any(
            |fact| matches!(fact, AccessibilityFact { observation: AccessibilityObservation::EpubType(value), .. } if value == "keyword")
        ));
        assert!(accessibility.iter().any(
            |fact| matches!(fact, AccessibilityFact { observation: AccessibilityObservation::Role(value), .. } if value == "doc-noteref")
        ));
        assert!(extraction.facts.structure().is_empty());
    }

    #[test]
    fn resource_text_stream_ranges_use_unicode_code_points() {
        let extraction = analyze("<html><body><p>A😀B</p><p>Next</p></body></html>");
        let facts = &extraction.facts;
        let stream = facts.text_stream();

        assert_eq!(stream.text(), "A😀B\nNext");
        assert_eq!(stream.code_point_len(), 8);
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 3));
        assert_eq!(Some(spans(facts)[1].range()), TextRange::new(4, 8));
        assert_eq!(
            stream.text_for_range(TextRange::new(1, 2).unwrap()),
            Ok("😀")
        );
        assert_eq!(
            stream.text_for_range(TextRange::new(2, 3).unwrap()),
            Ok("B")
        );
        assert_eq!(stream.text_for_range(TextRange::new(2, 2).unwrap()), Ok(""));
        assert_eq!(
            stream.text_for_range(TextRange::new(8, 9).unwrap()),
            Err(TextRangeError::OutOfBounds)
        );
        assert!(TextRange::new(2, 1).is_none());
    }

    #[test]
    fn text_stream_ranges_follow_br_block_and_inline_structure() {
        let extraction =
            analyze("<html><body><p>Alpha<br/>Beta <em>Gamma</em></p><p>Delta</p></body></html>");
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "Alpha\nBeta Gamma\nDelta");
        assert_eq!(spans(facts)[0].text(), "Alpha\nBeta Gamma");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 16));
        assert_eq!(
            facts.text_stream().text_for_range(spans(facts)[0].range()),
            Ok("Alpha\nBeta Gamma")
        );
        assert_eq!(Some(spans(facts)[1].range()), TextRange::new(17, 22));
    }

    #[test]
    fn attributed_pagebreak_preserves_visible_text_and_separate_label() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><span epub:type="pagebreak" title="same">same</span><p>same</p></body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "same\nsame");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 4));
        assert_eq!(Some(spans(facts)[1].range()), TextRange::new(5, 9));
        assert_eq!(facts.text_stream().supplementary()[0].text(), "same");
    }

    #[test]
    fn attributed_void_pagebreaks_do_not_suppress_following_stream_text() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><hr epub:type="pagebreak" title="1"><p>After hr</p><img epub:type="pagebreak" aria-label="2"><p>After image</p></body></html>"#,
        );
        assert_eq!(
            extraction.facts.text_stream().text(),
            "After hr\nAfter image"
        );
    }

    #[test]
    fn pagebreak_span_does_not_claim_repeated_body_text() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><span epub:type="pagebreak">same</span><p>same</p></body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "same\nsame");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 4));
        assert_eq!(Some(spans(facts)[1].range()), TextRange::new(5, 9));
    }

    #[test]
    fn repeated_chunks_keep_their_extracted_source_ranges() {
        let extraction = analyze("<html><body><p>same</p><p>same</p></body></html>");
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "same\nsame");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 4));
        assert_eq!(Some(spans(facts)[1].range()), TextRange::new(5, 9));
    }

    #[test]
    fn visible_unicode_whitespace_is_preserved_in_stream_and_search_text() {
        let extraction = analyze("<html><body><p>A\u{00a0}\u{2003}<em>B</em></p></body></html>");
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "A\u{00a0}\u{2003}B");
        assert_eq!(spans(facts)[0].text(), "A\u{00a0}\u{2003}B");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 4));
    }

    #[test]
    fn loose_body_text_stays_stream_only_without_shifting_semantic_ranges() {
        let extraction = analyze("<html><body>loose<p>kept</p>tail</body></html>");
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "loose\nkept\ntail");
        assert_eq!(spans(facts).len(), 1);
        assert_eq!(spans(facts)[0].text(), "kept");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(6, 10));
    }

    #[test]
    fn resource_text_stream_is_stable_across_reader_chunks_and_inline_elements() {
        let long_text = "a".repeat(70_000);
        let xml = format!("<html><body><p>{long_text}<em>b</em> c</p></body></html>");
        let extraction = analyze(&xml);
        let stream = extraction.facts.text_stream();

        assert_eq!(stream.text(), format!("{long_text}b c"));
    }

    #[test]
    fn equal_id_and_xml_id_on_one_element_preserve_both_declarations() {
        let extraction =
            analyze(r#"<html><body><p id="same" xml:id="same">Text</p></body></html>"#);
        let fragments = extraction.facts.fragments();

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].id(), "same");
        assert_eq!(fragments[0].attribute(), FragmentAttribute::Id);
        assert_eq!(fragments[1].id(), "same");
        assert_eq!(fragments[1].attribute(), FragmentAttribute::XmlId);
    }

    #[test]
    fn distinct_id_and_xml_id_preserve_values_and_provenance() {
        let extraction = analyze(r#"<html><body><p id="html" xml:id="xml">Text</p></body></html>"#);
        let fragments = extraction.facts.fragments();

        assert_eq!(
            fragments
                .iter()
                .map(|fact| (fact.id(), fact.attribute()))
                .collect::<Vec<_>>(),
            [
                ("html", FragmentAttribute::Id),
                ("xml", FragmentAttribute::XmlId),
            ]
        );
    }

    #[test]
    fn overlapping_structure_and_note_semantics_emit_ordered_categories() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
                <h2 epub:type="pagebreak chapter">Heading page</h2>
                <aside epub:type="footnote" role="doc-footnote"></aside>
                <aside epub:type="endnote" role="doc-endnote"></aside>
                <aside epub:type="note" role="note"></aside>
                <section role="doc-chapter"></section>
            </body></html>"#,
        );
        let categories = extraction
            .facts
            .structure()
            .iter()
            .map(|fact| match fact.role() {
                StructureRole::Heading(_) => "heading",
                StructureRole::Pagebreak => "pagebreak",
                StructureRole::Footnote => "footnote",
                StructureRole::Endnote => "endnote",
                StructureRole::Note => "note",
                StructureRole::PublicationSection => "section",
                _ => "other",
            })
            .collect::<Vec<_>>();

        assert_eq!(
            categories,
            [
                "heading",
                "pagebreak",
                "section",
                "footnote",
                "endnote",
                "note",
                "section"
            ]
        );
        assert_eq!(
            extraction.facts.structure()[0].role(),
            StructureRole::Heading(HeadingLevel::new(2).unwrap())
        );
        assert!(!matches!(
            extraction.facts.structure()[1].role(),
            StructureRole::Heading(_)
        ));
    }

    #[test]
    fn heading_pagebreak_keeps_visible_heading_and_separate_page_label() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><h2 epub:type="pagebreak" title="12">Chapter</h2></body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "Chapter");
        assert_eq!(facts.structure().len(), 2);
        assert!(
            matches!(&facts.structure()[0].role(), StructureRole::Heading(_))
                && facts.structure()[0].label() == Some("Chapter")
        );
        assert!(
            matches!(&facts.structure()[1].role(), StructureRole::Pagebreak)
                && facts.structure()[1].label() == Some("12")
        );
        assert_eq!(
            spans(facts)
                .iter()
                .map(|span| (span.role(), span.text(), Some(span.range())))
                .collect::<Vec<_>>(),
            [(
                TextRole::Heading {
                    level: HeadingLevel::new(2).unwrap()
                },
                "Chapter",
                TextRange::new(0, 7)
            ),]
        );
    }

    #[test]
    fn pagebreak_overlaps_preserve_heading_and_caption_content() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
                <h1 epub:type="pagebreak">Visible heading</h1>
                <figure epub:type="pagebreak" title="figure-page"><figcaption>Figure caption</figcaption></figure>
                <table role="doc-pagebreak" aria-label="table-page"><caption>Table caption</caption></table>
            </body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(
            facts.text_stream().text(),
            "Visible heading\nFigure caption\nTable caption"
        );
        assert_eq!(
            spans(facts)
                .iter()
                .map(|span| (span.role(), span.text()))
                .collect::<Vec<_>>(),
            [
                (
                    TextRole::Heading {
                        level: HeadingLevel::new(1).unwrap()
                    },
                    "Visible heading"
                ),
                (TextRole::Pagebreak, "Figure caption"),
                (TextRole::FigureCaption, "Figure caption"),
                (TextRole::Pagebreak, "Table caption"),
                (TextRole::TableCaption, "Table caption"),
            ]
        );
        assert!(
            matches!(&facts.structure()[0].role(), StructureRole::Heading(_))
                && facts.structure()[0].label() == Some("Visible heading")
        );
        assert!(
            matches!(&facts.structure()[1].role(), StructureRole::Pagebreak)
                && facts.structure()[1].label() == Some("Visible heading")
        );
        assert!(
            matches!(&facts.structure()[2].role(), StructureRole::Pagebreak)
                && facts.structure()[2].label() == Some("figure-page")
        );
        assert!(
            matches!(&facts.structure()[3].role(), StructureRole::Figure)
                && facts.structure()[3].label() == Some("Figure caption")
        );
        assert!(
            matches!(&facts.structure()[4].role(), StructureRole::Pagebreak)
                && facts.structure()[4].label() == Some("table-page")
        );
        assert!(
            matches!(&facts.structure()[5].role(), StructureRole::Table)
                && facts.structure()[5].label() == Some("Table caption")
        );
    }

    #[test]
    fn fragmentary_xhtml_text_without_a_body_has_a_stream() {
        let extraction = analyze("<p>Recovered text</p>");
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "Recovered text");
        assert_eq!(spans(facts).len(), 1);
        assert_eq!(spans(facts)[0].text(), "Recovered text");
        assert_eq!(Some(spans(facts)[0].range()), TextRange::new(0, 14));
    }

    #[test]
    fn captions_aggregate_nested_flow_content() {
        let extraction = analyze(
            "<html><body><figure><figcaption>Before <h2>Inside</h2> After</figcaption></figure><table><caption>One <span>Two</span> Three</caption></table></body></html>",
        );
        let facts = &extraction.facts;

        assert_eq!(
            spans(facts)
                .iter()
                .map(|span| (span.role(), span.text()))
                .collect::<Vec<_>>(),
            [
                (TextRole::FigureCaption, "Before\nInside\nAfter"),
                (
                    TextRole::Heading {
                        level: HeadingLevel::new(2).unwrap()
                    },
                    "Inside"
                ),
                (TextRole::TableCaption, "One Two Three"),
            ]
        );
        assert!(
            matches!(&facts.structure()[0].role(), StructureRole::Figure)
                && facts.structure()[0].label() == Some("Before Inside After")
        );
        assert!(
            matches!(&facts.structure()[1].role(), StructureRole::Heading(_))
                && facts.structure()[1].label() == Some("Inside")
        );
        assert!(
            matches!(&facts.structure()[2].role(), StructureRole::Table)
                && facts.structure()[2].label() == Some("One Two Three")
        );
    }

    #[test]
    fn caption_pagebreaks_keep_caption_and_pagebreak_projections() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
                <figure><figcaption epub:type="pagebreak">12</figcaption></figure>
                <table><caption role="doc-pagebreak">13</caption></table>
            </body></html>"#,
        );
        let facts = &extraction.facts;

        assert_eq!(facts.text_stream().text(), "12\n13");
        assert_eq!(
            facts
                .structure()
                .iter()
                .filter(|fact| fact.role() == StructureRole::Pagebreak)
                .filter_map(StructureFact::label)
                .collect::<Vec<_>>(),
            ["12", "13"]
        );
        assert_eq!(
            spans(facts)
                .iter()
                .map(|span| (span.role(), span.text()))
                .collect::<Vec<_>>(),
            [
                (TextRole::FigureCaption, "12"),
                (TextRole::TableCaption, "13"),
            ]
        );
    }

    #[test]
    fn extracts_link_variants_and_svg_references() {
        let extraction = analyze(
            r#"<html><head><link rel="stylesheet" href="style.css" /></head><body>
                <form action="submit.xhtml" method="post"><input name="q" /></form>
                <a href="chapter2.xhtml">Next</a>
                <img src="pic.jpg" />
                <script src="app.js"></script>
                <audio src="audio.mp3"><source src="audio.ogg" /><track src="captions.vtt" kind="captions" /></audio>
                <video src="video.mp4" poster="poster.jpg"><source src="video.webm" /></video>
                <iframe src="frame.xhtml"></iframe>
                <object data="obj.bin"></object>
                <embed src="embed.bin" />
                <svg><a xlink:href="linked.xhtml"><text>Link</text></a><image href="cover.jpg" /><use xlink:href="icons.svg#star" /></svg>
            </body></html>"#,
        );
        let links: Vec<_> = extraction.links.iter().collect();
        assert!(
            links
                .iter()
                .any(|link| matches!(link, LinkFact::Stylesheet(_)))
        );
        assert!(
            links
                .iter()
                .any(|link| matches!(link, LinkFact::FormAction(_)))
        );
        assert!(
            links
                .iter()
                .any(|link| matches!(link, LinkFact::Hyperlink(_)))
        );
        assert!(links.iter().any(|link| matches!(link, LinkFact::Image(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Script(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Audio(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Video(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Track(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Poster(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Iframe(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Object(_))));
        assert!(links.iter().any(|link| matches!(link, LinkFact::Embed(_))));
        assert!(
            links
                .iter()
                .any(|link| matches!(link, LinkFact::SvgReference(_)))
        );
    }

    #[test]
    fn media_facts_are_extracted_independently_from_relationships() {
        let extraction = analyze(
            r#"<html><body>
                <a href="https://example.test/out">External link</a>
                <form action="https://example.test/search"><input name="q" /></form>
                <img src="https://cdn.example.test/pic.jpg" alt="Remote" />
                <script src="app.js"></script>
            </body></html>"#,
        );
        assert_eq!(extraction.facts.media().len(), 1);
    }

    #[test]
    fn form_facts_capture_actions_and_controls() {
        let extraction = analyze(
            r#"<html><body>
                <form id="search" action="../search.xhtml" method="get">
                    <input type="search" name="q" value="epub" />
                    <button type="submit" name="go" value="1" formaction="../alternate.xhtml">Go</button>
                    <select name="scope"></select>
                    <textarea name="notes"></textarea>
                </form>
            </body></html>"#,
        );
        let forms = extraction.facts.forms();

        assert_eq!(forms.len(), 5);
        assert!(matches!(forms[0], FormFact::Form { .. }));
        assert_eq!(forms[0].fragment().map(FragmentFact::id), Some("search"));
        assert!(
            matches!(&forms[0], FormFact::Form { method: Some(method), .. } if method == "get")
        );
        assert!(
            extraction
                .links
                .iter()
                .any(|link| matches!(link, LinkFact::FormAction(_)))
        );
        assert!(forms.iter().any(|fact| matches!(
            fact,
            FormFact::Control {
                element,
                control_type: Some(control_type),
                name: Some(name),
                value: Some(value),
                ..
            } if element == "input" && control_type == "search" && name == "q" && value == "epub"
        )));
        assert_eq!(
            extraction
                .links
                .iter()
                .filter(|link| matches!(link, LinkFact::FormAction(_)))
                .count(),
            2
        );
    }

    #[test]
    fn script_facts_capture_external_inline_and_event_handlers() {
        let extraction = analyze(
            r#"<html><body onload="boot()">
                <script src="app.js" type="module"></script>
                <script>console.log('inline')</script>
                <button onclick="go()">Go</button>
            </body></html>"#,
        );
        let scripts = extraction.facts.scripts();

        assert!(scripts.iter().any(ScriptFact::is_executable));
        assert!(
            scripts
                .iter()
                .any(|fact| matches!(fact, ScriptFact::External { script_type: Some(value), .. } if value == "module"))
        );
        assert!(
            scripts
                .iter()
                .any(|fact| matches!(fact, ScriptFact::Inline { has_text: true, .. }))
        );
        assert_eq!(
            scripts
                .iter()
                .filter_map(ScriptFact::attribute)
                .collect::<Vec<_>>(),
            vec!["onload", "onclick"]
        );
    }

    #[test]
    fn script_sources_follow_html_and_svg_namespaces() {
        let extraction = analyze(
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:xlink="http://www.w3.org/1999/xlink"><body>
                <script src="html.js" href="not-html.js"></script>
                <svg xmlns="http://www.w3.org/2000/svg">
                    <script href="svg.js" xlink:href="ignored.js" src="not-svg.js"></script>
                    <script xlink:href="legacy.js"></script>
                    <script type="application/example" href="plugin.bin"></script>
                    <a href="right.xhtml" xlink:href="wrong.xhtml"><text>Link</text></a>
                    <image href="right.png" xlink:href="wrong.png" />
                    <use href="right.svg#icon" xlink:href="wrong.svg#icon" />
                    <foreignObject><div xmlns="http://www.w3.org/1999/xhtml"><script src="foreign.js"></script></div></foreignObject>
                </svg>
                <a xlink:href="not-an-html-link.xhtml">No link</a>
            </body></html>"#,
        );

        assert_eq!(
            extraction
                .links
                .iter()
                .map(|link| (link.attribute().as_str(), link.declared().as_str()))
                .collect::<Vec<_>>(),
            [
                ("src", "html.js"),
                ("href", "svg.js"),
                ("xlink:href", "legacy.js"),
                ("href", "plugin.bin"),
                ("href", "right.xhtml"),
                ("href", "right.png"),
                ("href", "right.svg#icon"),
                ("src", "foreign.js"),
            ]
        );
        assert_eq!(
            extraction
                .facts
                .scripts()
                .iter()
                .filter(|fact| matches!(fact, ScriptFact::External { .. }))
                .count(),
            4
        );
        assert_eq!(extraction.associations.media[0].len(), 1);
        assert_eq!(
            extraction
                .associations
                .scripts
                .iter()
                .filter_map(|slot| *slot)
                .map(|index| extraction.links[index].declared().as_str())
                .collect::<Vec<_>>(),
            ["html.js", "svg.js", "legacy.js", "foreign.js"]
        );
    }

    #[test]
    fn prefixed_xhtml_and_svg_elements_resolve_their_namespace() {
        let extraction = analyze(
            r#"<h:html xmlns:h="http://www.w3.org/1999/xhtml" xmlns:x="urn:example"><h:body>
                <h:script>run()</h:script>
                <h:script src="app.js"></h:script>
                <h:script></h:script>
                <h:a href="next.xhtml">Next</h:a>
                <s:svg xmlns:s="http://www.w3.org/2000/svg"><s:script href="svg.js"></s:script></s:svg>
                <x:script>not a script</x:script>
            </h:body></h:html>"#,
        );

        assert_eq!(
            extraction
                .links
                .iter()
                .map(|link| (link.attribute().as_str(), link.declared().as_str()))
                .collect::<Vec<_>>(),
            [
                ("src", "app.js"),
                ("href", "next.xhtml"),
                ("href", "svg.js")
            ]
        );
        let scripts = extraction.facts.scripts();
        assert_eq!(scripts.len(), 4);
        assert!(matches!(
            scripts[0],
            ScriptFact::Inline { has_text: true, .. }
        ));
        assert!(matches!(scripts[1], ScriptFact::External { .. }));
        assert!(matches!(
            scripts[2],
            ScriptFact::Inline {
                has_text: false,
                ..
            }
        ));
        assert!(matches!(scripts[3], ScriptFact::External { .. }));
    }

    #[test]
    fn script_data_blocks_are_not_scripted_content() {
        let extraction = analyze(
            r#"<html><body><script type="application/ld+json">{"name":"Book"}</script></body></html>"#,
        );
        let scripts = extraction.facts.scripts();

        assert!(!scripts.iter().any(ScriptFact::is_executable));
        assert_eq!(scripts.len(), 1);
        assert!(matches!(scripts[0], ScriptFact::DataBlock { .. }));
        assert_eq!(scripts[0].script_type(), Some("application/ld+json"));
        assert_eq!(scripts[0].has_text(), Some(true));
    }

    #[test]
    fn script_mime_essence_ignores_case_parameters_and_unknown_on_attributes() {
        let extraction = analyze(
            r#"<html><body onload="known()" onmadeup="ignored()"><svg onbegin="begin()"></svg>
                <script type=" Text/JavaScript ; charset=utf-8 ">run()</script>
                <script type=" APPLICATION/LD+JSON ; profile=book ">{}</script>
                <script type="application/ld+json" src="data.json">{"remote":false}</script>
                <script type="module;foo" src="not-module.js">notModule()</script>
            </body></html>"#,
        );
        let scripts = extraction.facts.scripts();

        assert!(
            matches!(scripts[0], ScriptFact::EventHandler { ref attribute, .. } if attribute == "onload")
        );
        assert!(matches!(
            scripts[2],
            ScriptFact::Inline { has_text: true, .. }
        ));
        assert!(matches!(
            scripts[3],
            ScriptFact::DataBlock { has_text: true, .. }
        ));
        assert!(matches!(
            scripts[4],
            ScriptFact::DataBlock { has_text: true, .. }
        ));
        assert!(matches!(
            scripts[5],
            ScriptFact::DataBlock { has_text: true, .. }
        ));
        assert_eq!(
            scripts
                .iter()
                .filter_map(ScriptFact::attribute)
                .collect::<Vec<_>>(),
            ["onload", "onbegin"]
        );
        assert_eq!(
            extraction
                .links
                .iter()
                .filter(|link| matches!(link, LinkFact::Script(_)))
                .map(|link| link.declared().as_str())
                .collect::<Vec<_>>(),
            ["data.json", "not-module.js"]
        );
    }

    #[test]
    fn legacy_javascript_mime_essences_and_module_keyword_are_exact() {
        let executable = [
            "text/javascript",
            "application/javascript",
            "application/x-javascript",
            "text/ecmascript",
            "application/ecmascript",
            "application/x-ecmascript",
            "text/x-javascript",
            "text/x-ecmascript",
            "text/livescript",
            "text/jscript",
            "text/javascript1.0",
            "text/javascript1.1",
            "text/javascript1.2",
            "text/javascript1.3",
            "text/javascript1.4",
            "text/javascript1.5",
        ];
        for mime in executable {
            assert!(is_executable_script_type(Some(mime)), "{mime}");
            assert!(
                is_executable_script_type(Some(&format!(" {mime};charset=utf-8 "))),
                "{mime} with parameters"
            );
        }
        assert!(is_executable_script_type(Some(" MoDuLe ")));
        assert!(!is_executable_script_type(Some("module;charset=utf-8")));
        assert!(!is_executable_script_type(Some("application/ld+json")));
    }

    #[test]
    fn deliberate_html_and_embedded_svg_event_handler_matrix_is_complete() {
        for attribute in EVENT_HANDLER_ATTRIBUTES {
            assert!(is_event_handler_attr(attribute), "{attribute}");
            assert!(
                is_event_handler_attr(&attribute.to_ascii_uppercase()),
                "{attribute}"
            );
        }
        for required in [
            "oncommand",
            "oncontentvisibilityautostatechange",
            "oncontextlost",
            "oncontextrestored",
            "onfocusin",
            "onfocusout",
            "onlanguagechange",
            "onpagereveal",
            "onpageswap",
            "onactivate",
            "onzoom",
            "onanimationstart",
            "onanimationiteration",
            "onanimationend",
            "onanimationcancel",
            "onbegin",
            "onend",
            "onrepeat",
        ] {
            assert!(EVENT_HANDLER_ATTRIBUTES.contains(&required), "{required}");
        }
        assert!(!is_event_handler_attr("onmadeup"));
    }

    #[test]
    fn formaction_is_limited_to_controls_that_can_submit() {
        let extraction = analyze(
            r#"<html><body>
                <input formaction="missing-type"/><input type="text" formaction="text"/>
                <input type="SUBMIT" formaction="submit"/><input type="image" src="image.png" formaction="image"/>
                <button formaction="default"></button><button type="submit" formaction="button-submit"></button>
                <button type="reset" formaction="reset"></button><button type="button" formaction="button"></button>
                <textarea formaction="textarea"></textarea><select formaction="select"></select>
            </body></html>"#,
        );
        assert_eq!(
            extraction
                .links
                .iter()
                .filter(|link| matches!(link, LinkFact::FormAction(_)))
                .map(|link| link.declared().as_str())
                .collect::<Vec<_>>(),
            ["submit", "image", "default", "button-submit"]
        );
        assert_eq!(
            extraction
                .links
                .iter()
                .filter(|link| matches!(link, LinkFact::Image(_)))
                .map(|link| link.declared().as_str())
                .collect::<Vec<_>>(),
            ["image.png"]
        );
    }

    #[test]
    fn srcset_tokenizer_preserves_urls_and_rejects_malformed_candidates() {
        assert_eq!(
            parse_srcset_candidates(
                " small.png 1x, wide.png 640w, data:image/png;base64,AAAA 2x, bare.png, , bad.png 0x, duplicate.png 1x 2x "
            ),
            [
                "small.png",
                "wide.png",
                "data:image/png;base64,AAAA",
                "bare.png",
            ]
        );
        assert_eq!(parse_srcset_candidates("a.png,b.png 2x"), ["a.png,b.png"]);
        assert!(parse_srcset_candidates(" , , bad.png nope ").is_empty());
        assert!(parse_srcset_candidates("bad.png infx, huge.png 1e999x").is_empty());
        assert!(
            parse_srcset_candidates("plus.png +1x, dot.png 1.x, plus-width.png +2w").is_empty()
        );
        assert!(parse_srcset_candidates("unicode.png é").is_empty());
    }

    #[test]
    fn responsive_media_keeps_one_occurrence_with_ordered_src_and_srcset_links() {
        let extraction = analyze(
            r#"<html><body><picture>
                <source src="ignored.webp" srcset="one.webp 1x, data:image/webp;base64,AAAA 2x, two.webp 3x"/>
                <img src="fallback.png" srcset="small.png 320w, large.png 640w"/>
            </picture></body></html>"#,
        );
        assert_eq!(extraction.facts.media().len(), 2);
        assert_eq!(extraction.associations.media.len(), 2);
        assert_eq!(
            extraction.associations.media[0]
                .iter()
                .map(|index| extraction.links[*index].declared().as_str())
                .collect::<Vec<_>>(),
            ["one.webp", "data:image/webp;base64,AAAA", "two.webp",]
        );
        assert_eq!(
            extraction.associations.media[1]
                .iter()
                .map(|index| extraction.links[*index].declared().as_str())
                .collect::<Vec<_>>(),
            ["fallback.png", "small.png", "large.png"]
        );
    }

    #[test]
    fn source_media_uses_media_parent_context() {
        let extraction = analyze(
            r#"<html><body>
                <audio><source src="audio.ogg" /></audio>
                <video><source src="video.webm" /></video>
                <picture><source srcset="image.webp 1x" /></picture>
                <source src="unknown.bin" />
            </body></html>"#,
        );
        let variants: Vec<_> = extraction
            .facts
            .media()
            .iter()
            .map(|fact| match fact {
                MediaFact::Audio => "audio",
                MediaFact::Video => "video",
                MediaFact::Source {
                    context: MediaSourceContext::Audio,
                } => "source-audio",
                MediaFact::Source {
                    context: MediaSourceContext::Video,
                } => "source-video",
                MediaFact::Source {
                    context: MediaSourceContext::Picture,
                } => "source-picture",
                MediaFact::Source {
                    context: MediaSourceContext::Other,
                } => "source-other",
                _ => "other",
            })
            .collect();

        assert_eq!(
            variants,
            vec![
                "audio",
                "source-audio",
                "video",
                "source-video",
                "source-picture",
                "source-other"
            ]
        );
    }

    #[test]
    fn pagebreak_text_fallback_is_searchable_label() {
        let extraction = analyze(
            r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><span epub:type="pagebreak" id="p12">12</span></body></html>"#,
        );
        let facts = &extraction.facts;
        let pagebreak = facts.structure().first().unwrap();
        let labels: Vec<_> = spans(facts)
            .iter()
            .filter(|span| span.role() == TextRole::Pagebreak)
            .map(|span| span.text())
            .collect();

        assert!(matches!(pagebreak.role(), StructureRole::Pagebreak));
        assert_eq!(pagebreak.label(), Some("12"));
        assert_eq!(labels, vec!["12"]);
    }

    #[test]
    fn head_title_is_not_searchable_but_svg_title_is() {
        let extraction = analyze(
            "<html><head><title>Head Title</title></head><body><svg><title>SVG Title</title></svg></body></html>",
        );
        let facts = &extraction.facts;
        assert_eq!(facts.text_stream().text(), "SVG Title");
    }

    #[test]
    fn html_auto_closing_does_not_corrupt_text_order() {
        let extraction = analyze("<html><body><p id=\"one\">one<p id=\"two\">two</body></html>");
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();

        assert_eq!(text, vec!["one", "two"]);
        assert_eq!(facts.fragments().len(), 2);
    }

    #[test]
    fn figure_and_table_structures_use_caption_labels() {
        let extraction = analyze(
            r#"<html><body><figure><figcaption>Figure Label</figcaption></figure><table><caption>Table Label</caption></table></body></html>"#,
        );
        let labels: Vec<_> = extraction
            .facts
            .structure()
            .iter()
            .filter(|fact| matches!(fact.role(), StructureRole::Figure | StructureRole::Table))
            .filter_map(StructureFact::label)
            .collect();

        assert_eq!(labels, vec!["Figure Label", "Table Label"]);
    }

    #[test]
    fn nested_blocks_have_overlapping_spans_without_duplicate_stream_text() {
        let extraction =
            analyze("<html><body><ul><li>one <p>two</p> three</li></ul></body></html>");
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();

        assert_eq!(text, vec!["one\ntwo\nthree", "two"]);
        assert_eq!(facts.text_stream().text(), "one\ntwo\nthree");
    }

    #[test]
    fn table_cells_are_searchable_body_chunks() {
        let extraction = analyze(
            "<html><body><table><caption>Roster</caption><tr><th>Name</th><td>Ishmael</td></tr></table></body></html>",
        );
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();

        assert_eq!(text, vec!["Roster", "Name", "Ishmael"]);
    }

    #[test]
    fn nested_anchor_recovery_keeps_links_and_readable_text() {
        let extraction = analyze(
            r##"<html><body><p>before <a href="one.xhtml">one <a href="two.xhtml#frag">two</a> after</p></body></html>"##,
        );
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();
        let links: Vec<_> = extraction
            .links
            .iter()
            .map(|link| link.declared().as_str())
            .collect();

        assert_eq!(text, vec!["before one two after"]);
        assert_eq!(links, vec!["one.xhtml", "two.xhtml#frag"]);
    }

    #[test]
    fn namespaced_math_inside_body_text_is_readable() {
        let extraction = analyze(
            r#"<html><body><p>Equation <math xmlns="http://www.w3.org/1998/Math/MathML"><mi>x</mi><mo>=</mo><mn>1</mn></math>.</p></body></html>"#,
        );
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();

        assert_eq!(text, vec!["Equation x=1."]);
    }

    #[test]
    fn skips_script_style_and_template_text() {
        let extraction = analyze(
            "<html><body><p>Visible</p><script>Hidden</script><style>Hidden</style><template><p>Hidden</p></template></body></html>",
        );
        let facts = &extraction.facts;
        let text: Vec<_> = spans(facts).iter().map(|span| span.text()).collect();

        assert_eq!(text, vec!["Visible"]);
    }
}
