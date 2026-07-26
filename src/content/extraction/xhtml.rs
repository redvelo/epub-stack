use crate::accessibility::{
    AccessibilityElementFact, AccessibilityFact, AccessibilityHeadingLevelFact,
    AccessibilityValueFact,
};
use crate::content::XhtmlFacts;
use crate::content::facts::{
    FormFact, FragmentAttribute, FragmentFact, HtmlStructuralElement, LinkConstructors, LinkFact,
    LinkFactData, MediaFact, MediaSourceContext, NavigationLinkKind, ReferenceAttribute,
    ScriptFact, SemanticToken, StructureFact, ViewportFact, XhtmlLinkSlot,
};
use crate::content::text::{TextChunk, TextChunkContent, TextChunkKind, TextRange, TextStream};
use crate::resource::AuthoredHref;
use crate::semantics::{DpubAriaRole, EpubStructuralSemantic, HeadingLevel, TextDirection};
use lol_html::{HtmlRewriter, Settings, element, end_tag, text};
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

#[derive(Debug)]
pub(crate) struct XhtmlExtraction {
    pub(crate) facts: XhtmlFacts,
    pub(crate) accessibility: Vec<AccessibilityFact>,
    pub(crate) links: Vec<LinkFact>,
    pub(crate) authored_base: Option<AuthoredHref>,
    pub(crate) associations: XhtmlLinkAssociations,
}

#[derive(Debug)]
pub(crate) struct XhtmlLinkAssociations {
    pub(crate) media: Vec<Vec<usize>>,
    pub(crate) forms: Vec<Option<usize>>,
    pub(crate) scripts: Vec<Option<usize>>,
}

pub(crate) fn parse_xhtml_document_from_reader_counted(
    reader: &mut dyn Read,
) -> std::result::Result<(XhtmlExtraction, u64), String> {
    HtmlContentExtractor.parse(reader)
}

fn link_indices(slots: Vec<Option<XhtmlLinkSlot>>) -> Vec<Option<usize>> {
    slots
        .into_iter()
        .map(|slot| slot.map(XhtmlLinkSlot::index))
        .collect()
}

fn media_link_indices(slots: Vec<Vec<XhtmlLinkSlot>>) -> Vec<Vec<usize>> {
    slots
        .into_iter()
        .map(|slots| slots.into_iter().map(XhtmlLinkSlot::index).collect())
        .collect()
}

struct HtmlContentExtractor;

impl HtmlContentExtractor {
    fn parse(self, reader: &mut dyn Read) -> std::result::Result<(XhtmlExtraction, u64), String> {
        let state = Rc::new(RefCell::new(ExtractorState::new()));
        let start_state = Rc::clone(&state);
        let text_state = Rc::clone(&state);

        let mut bytes_read = 0u64;
        let rewrite_result: std::result::Result<(), String> = {
            let mut rewriter = HtmlRewriter::new(
                Settings::new()
                    .append_element_content_handler(element!("*", move |el| {
                        let element = el.tag_name();
                        let namespace = el.namespace_uri();
                        let in_svg = namespace == "http://www.w3.org/2000/svg";
                        let in_html = namespace == "http://www.w3.org/1999/xhtml";
                        let end_state = Rc::clone(&start_state);
                        let end_element = element.clone();
                        let has_end = el
                            .on_end_tag(end_tag!(move |_| {
                                end_state.borrow_mut().handle_end(&end_element);
                                Ok(())
                            }))
                            .is_ok();
                        let attrs = ElementAttrs::from_lol_html(el.attributes());
                        start_state
                            .borrow_mut()
                            .handle_start(&element, in_svg, in_html, &attrs, has_end);
                        Ok(())
                    }))
                    .append_element_content_handler(text!("*", move |chunk| {
                        text_state.borrow_mut().push_text(chunk.as_str());
                        Ok(())
                    })),
                |_: &[u8]| {},
            );

            let mut buffer = [0u8; 64 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(len) => {
                        bytes_read = bytes_read.saturating_add(len as u64);
                        rewriter
                            .write(&buffer[..len])
                            .map_err(|err| err.to_string())?
                    }
                    Err(err) => return Err(err.to_string()),
                }
            }
            rewriter.end().map_err(|err| err.to_string())
        };

        let state = Rc::try_unwrap(state).unwrap().into_inner();
        rewrite_result?;
        Ok((state.finish(), bytes_read))
    }
}

#[derive(Debug, Clone)]
pub(super) struct ElementAttrs {
    values: Vec<(String, String)>,
}

impl ElementAttrs {
    fn from_lol_html(attrs: &[lol_html::html_content::Attribute<'_>]) -> Self {
        Self {
            values: attrs
                .iter()
                .map(|attr| (attr.name().to_string(), attr.value().to_string()))
                .collect(),
        }
    }

    pub(super) fn from_values(values: Vec<(String, String)>) -> Self {
        Self { values }
    }

    pub(super) fn value(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(attr, _)| attr == key)
            .map(|(_, value)| value.as_str())
    }

    fn value_any(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|key| self.value(key))
    }

    fn values(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

#[derive(Debug)]
pub(super) struct ExtractorState {
    facts: XhtmlFacts,
    accessibility: Vec<AccessibilityFact>,
    links: Vec<LinkFact>,
    authored_base: Option<AuthoredHref>,
    element_stack: Vec<ElementFrame>,
    nearest_fragment: Option<String>,
    text_blocks: Vec<TextBlock>,
    pending_alt_text: Vec<PendingAltText>,
    text_stream_builder: TextStreamBuilder,
    next_text_chunk: usize,
    next_element_ordinal: usize,
    media_links: Vec<Vec<XhtmlLinkSlot>>,
    form_links: Vec<Option<XhtmlLinkSlot>>,
    script_links: Vec<Option<XhtmlLinkSlot>>,
}

#[derive(Debug, Default)]
struct TextStreamBuilder {
    text: String,
    code_point_len: u64,
    in_body: bool,
    suppressed_depth: usize,
    pending_space: bool,
    pending_block_boundary: bool,
    fragment_mode: bool,
}

impl TextStreamBuilder {
    fn handle_start(&mut self, element: &str, suppress_text: bool) {
        if element == "body" && !self.fragment_mode {
            self.in_body = true;
            return;
        }
        if !self.in_body {
            return;
        }
        if suppress_text {
            self.suppressed_depth += 1;
            return;
        }
        if self.suppressed_depth == 0 && is_text_stream_block(element) {
            self.pending_block_boundary = true;
            self.pending_space = false;
        }
    }

    fn handle_end(&mut self, element: &str, suppress_text: bool) {
        if element == "body" && !self.fragment_mode {
            self.in_body = false;
            self.pending_space = false;
            self.pending_block_boundary = false;
            return;
        }
        if !self.in_body {
            return;
        }
        if suppress_text {
            self.suppressed_depth = self.suppressed_depth.saturating_sub(1);
            return;
        }
        if self.suppressed_depth == 0 && is_text_stream_block(element) {
            self.pending_block_boundary = true;
            self.pending_space = false;
        }
    }

    fn push_text(&mut self, value: &str) -> Option<TextRange> {
        if !self.in_body || self.suppressed_depth > 0 {
            return None;
        }
        let mut start = None;
        let mut end = None;
        for ch in value.chars() {
            if is_text_whitespace(ch) {
                self.pending_space = true;
                continue;
            }
            if !self.text.is_empty() {
                if self.pending_block_boundary {
                    self.text.push('\n');
                    self.code_point_len += 1;
                } else if self.pending_space {
                    self.text.push(' ');
                    self.code_point_len += 1;
                }
            }
            self.pending_space = false;
            self.pending_block_boundary = false;
            start.get_or_insert(self.code_point_len);
            self.text.push(ch);
            self.code_point_len += 1;
            end = Some(self.code_point_len);
        }
        start.zip(end).map(|(start, end)| TextRange { start, end })
    }

    fn finish(self) -> TextStream {
        TextStream { text: self.text }
    }
}

#[derive(Debug, Clone)]
struct ElementFrame {
    element: String,
    navigation: Option<NavigationLinkKind>,
    navigation_link_claimed: bool,
    previous_fragment: Option<String>,
    lang: Option<String>,
    dir: Option<TextDirection>,
    suppresses_text: bool,
    structure_indices: Vec<usize>,
    inline_script_index: Option<usize>,
}

#[derive(Debug, Clone)]
enum TextBlock {
    Body(TextBlockData),
    Heading(HeadingTextBlockData),
    PagebreakLabel(TextBlockData),
    FigureCaption(TextBlockData),
    TableCaption(TextBlockData),
}

#[derive(Debug, Clone)]
struct PendingAltText {
    text: String,
    fragment: Option<String>,
    lang: Option<String>,
    dir: Option<TextDirection>,
}

#[derive(Debug, Clone)]
struct TextBlockData {
    element: String,
    fragment: Option<String>,
    text: String,
    lang: Option<String>,
    dir: Option<TextDirection>,
    structure_indices: Vec<usize>,
    stream_range: Option<TextRange>,
}

#[derive(Debug, Clone)]
struct HeadingTextBlockData {
    element: String,
    level: u8,
    fragment: Option<String>,
    text: String,
    lang: Option<String>,
    dir: Option<TextDirection>,
    structure_indices: Vec<usize>,
    stream_range: Option<TextRange>,
}

impl TextBlock {
    fn data(&self) -> TextBlockDataRef<'_> {
        match self {
            Self::Body(data)
            | Self::PagebreakLabel(data)
            | Self::FigureCaption(data)
            | Self::TableCaption(data) => TextBlockDataRef {
                element: &data.element,
                fragment: data.fragment.as_deref(),
                text: &data.text,
                structure_indices: &data.structure_indices,
            },
            Self::Heading(data) => TextBlockDataRef {
                element: &data.element,
                fragment: data.fragment.as_deref(),
                text: &data.text,
                structure_indices: &data.structure_indices,
            },
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self {
            Self::Body(data)
            | Self::PagebreakLabel(data)
            | Self::FigureCaption(data)
            | Self::TableCaption(data) => &mut data.text,
            Self::Heading(data) => &mut data.text,
        }
    }

    fn extend_stream_range(&mut self, emitted: TextRange) {
        let range = match self {
            Self::Body(data)
            | Self::PagebreakLabel(data)
            | Self::FigureCaption(data)
            | Self::TableCaption(data) => &mut data.stream_range,
            Self::Heading(data) => &mut data.stream_range,
        };
        *range = Some(match *range {
            Some(current) => TextRange {
                start: current.start,
                end: emitted.end,
            },
            None => emitted,
        });
    }

    fn into_text_chunk(self, id: String, text: String) -> TextChunk {
        let (kind, content, fragment, lang, dir) = match self {
            Self::Body(data) => (
                TextChunkKind::Body,
                data.stream_range
                    .map(TextChunkContent::Stream)
                    .unwrap_or_else(|| TextChunkContent::Owned(text.clone())),
                data.fragment,
                data.lang,
                data.dir,
            ),
            Self::Heading(data) => (
                TextChunkKind::Heading {
                    level: HeadingLevel::new(data.level).expect("HTML heading level is valid"),
                },
                data.stream_range
                    .map(TextChunkContent::Stream)
                    .unwrap_or_else(|| TextChunkContent::Owned(text.clone())),
                data.fragment,
                data.lang,
                data.dir,
            ),
            Self::PagebreakLabel(data) => (
                TextChunkKind::PagebreakLabel,
                TextChunkContent::Owned(text),
                data.fragment,
                data.lang,
                data.dir,
            ),
            Self::FigureCaption(data) => (
                TextChunkKind::FigureCaption,
                data.stream_range
                    .map(TextChunkContent::Stream)
                    .unwrap_or_else(|| TextChunkContent::Owned(text.clone())),
                data.fragment,
                data.lang,
                data.dir,
            ),
            Self::TableCaption(data) => (
                TextChunkKind::TableCaption,
                data.stream_range
                    .map(TextChunkContent::Stream)
                    .unwrap_or_else(|| TextChunkContent::Owned(text.clone())),
                data.fragment,
                data.lang,
                data.dir,
            ),
        };
        TextChunk {
            id,
            kind,
            content,
            fragment,
            lang,
            dir,
        }
    }
}

struct TextBlockDataRef<'a> {
    element: &'a str,
    fragment: Option<&'a str>,
    text: &'a str,
    structure_indices: &'a [usize],
}

impl ExtractorState {
    fn new() -> Self {
        Self::with_text_stream_builder(TextStreamBuilder::default())
    }

    pub(super) fn new_fragment() -> Self {
        Self::with_text_stream_builder(TextStreamBuilder {
            in_body: true,
            fragment_mode: true,
            ..TextStreamBuilder::default()
        })
    }

    fn with_text_stream_builder(text_stream_builder: TextStreamBuilder) -> Self {
        Self {
            facts: XhtmlFacts {
                fragments: Vec::new(),
                viewports: Vec::new(),
                text_stream: TextStream {
                    text: String::new(),
                },
                text: Vec::new(),
                structure: Vec::new(),
                media: Vec::new(),
                forms: Vec::new(),
                scripts: Vec::new(),
            },
            accessibility: Vec::new(),
            links: Vec::new(),
            authored_base: None,
            element_stack: Vec::new(),
            nearest_fragment: None,
            text_blocks: Vec::new(),
            pending_alt_text: Vec::new(),
            text_stream_builder,
            next_text_chunk: 0,
            next_element_ordinal: 0,
            media_links: Vec::new(),
            form_links: Vec::new(),
            script_links: Vec::new(),
        }
    }

    pub(super) fn finish(mut self) -> XhtmlExtraction {
        self.finish_all_text_blocks();
        self.build_text_stream();
        XhtmlExtraction {
            facts: self.facts,
            accessibility: self.accessibility,
            links: self.links,
            authored_base: self.authored_base,
            associations: XhtmlLinkAssociations {
                media: media_link_indices(self.media_links),
                forms: link_indices(self.form_links),
                scripts: link_indices(self.script_links),
            },
        }
    }

    pub(super) fn links_len(&self) -> usize {
        self.links.len()
    }

    pub(super) fn links_from(&self, index: usize) -> &[LinkFact] {
        &self.links[index..]
    }

    pub(super) fn accessibility_len(&self) -> usize {
        self.accessibility.len()
    }

    pub(super) fn accessibility_from(&self, index: usize) -> &[AccessibilityFact] {
        &self.accessibility[index..]
    }

    pub(super) fn handle_start(
        &mut self,
        element: &str,
        in_svg: bool,
        in_html: bool,
        attrs: &ElementAttrs,
        has_end: bool,
    ) {
        let element_ordinal = self.next_element_ordinal;
        self.next_element_ordinal += 1;
        let previous_fragment = self.nearest_fragment.clone();
        self.extract_base(element, in_svg, attrs);
        self.extract_viewport(element, in_html, attrs);
        self.extract_fragments(element, attrs, element_ordinal);
        let (lang, dir) = self.derived_text_metadata(attrs);
        let structure_indices = self.extract_structure(element, attrs);
        self.extract_accessibility(element, attrs, lang.clone(), dir);
        let first_link = self.links.len();
        self.extract_links(element, in_svg, attrs);
        self.extract_media(element, attrs, first_link);
        self.extract_forms(element, attrs, first_link);
        let inline_script_index = self.extract_scripts(element, in_svg, attrs, first_link);

        let pagebreak_attr_label = is_pagebreak(attrs) && pagebreak_label(attrs).is_some();
        let preserves_native_content = preserves_native_pagebreak_content(element);
        let suppresses_text =
            suppresses_text(element) || pagebreak_attr_label && !preserves_native_content;
        self.text_stream_builder
            .handle_start(element, suppresses_text);
        if element == "br"
            && !self.text_suppressed()
            && let Some(block) = self.text_blocks.last_mut()
        {
            append_text(block.text_mut(), " ");
        }
        if pagebreak_attr_label && heading_level(element).is_none() {
            self.emit_pagebreak_label(attrs, &structure_indices, lang.clone(), dir);
        }
        if !has_end && suppresses_text {
            self.text_stream_builder.handle_end(element, true);
        }

        if has_end {
            self.element_stack.push(ElementFrame {
                element: element.to_string(),
                navigation: (!self
                    .element_stack
                    .iter()
                    .any(|frame| frame.element == "nav"))
                .then(|| navigation_kind(element, attrs))
                .flatten(),
                navigation_link_claimed: false,
                previous_fragment,
                lang,
                dir,
                suppresses_text,
                structure_indices: structure_indices.clone(),
                inline_script_index,
            });
        } else {
            self.nearest_fragment = previous_fragment;
        }

        if has_end {
            let (lang, dir) = self.current_text_metadata();
            if let Some(block) = text_block_for_element(
                element,
                attrs,
                in_svg,
                self.nearest_fragment.clone(),
                lang,
                dir,
                structure_indices,
            ) && !self.text_suppressed()
            {
                let aggregates_descendants = self.text_blocks.last().is_some_and(|block| {
                    matches!(
                        block,
                        TextBlock::PagebreakLabel(_)
                            | TextBlock::FigureCaption(_)
                            | TextBlock::TableCaption(_)
                    )
                });
                let preserves_nested_projection = aggregates_descendants
                    && matches!(block, TextBlock::Heading(_) | TextBlock::PagebreakLabel(_));
                if self.text_blocks.last().is_some() && !aggregates_descendants {
                    self.finish_text_block();
                }
                if !aggregates_descendants || preserves_nested_projection {
                    self.text_blocks.push(block);
                }
            }
        }
    }

    pub(super) fn handle_end(&mut self, element: &str) {
        let stream_suppresses_text = self
            .element_stack
            .iter()
            .rfind(|frame| frame.element == element)
            .is_some_and(|frame| frame.suppresses_text);
        self.text_stream_builder
            .handle_end(element, stream_suppresses_text);
        if self
            .text_blocks
            .last()
            .is_some_and(|block| block.data().element == element)
        {
            self.finish_text_block();
        }

        if let Some(pos) = self
            .element_stack
            .iter()
            .rposition(|frame| frame.element == element)
        {
            while self.element_stack.len() - 1 > pos {
                self.element_stack.pop();
            }
            if let Some(frame) = self.element_stack.pop() {
                self.nearest_fragment = frame.previous_fragment;
            }
        }
    }

    pub(super) fn push_text(&mut self, text: &str) {
        if !text.trim().is_empty() {
            for frame in self.element_stack.iter().rev() {
                if let Some(index) = frame.inline_script_index {
                    if let Some(
                        ScriptFact::Inline { has_text, .. }
                        | ScriptFact::DataBlock { has_text, .. },
                    ) = self.facts.scripts.get_mut(index)
                    {
                        *has_text = true;
                    }
                    break;
                }
            }
        }
        if self.text_suppressed() {
            return;
        }
        if self.text_blocks.is_empty() {
            self.start_text_block_from_context();
        }
        let emitted = self.text_stream_builder.push_text(text);
        if self.text_blocks.is_empty() {
            return;
        }
        for block in &mut self.text_blocks {
            append_text(block.text_mut(), text);
            if let Some(emitted) = emitted {
                block.extend_stream_range(emitted);
            }
        }
    }

    fn build_text_stream(&mut self) {
        let builder = std::mem::take(&mut self.text_stream_builder);
        self.facts.text_stream = builder.finish();
    }

    fn start_text_block_from_context(&mut self) {
        let (lang, dir) = self.current_text_metadata();
        let Some(block) = self
            .element_stack
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, frame)| {
                let in_svg = self.element_stack[..=idx]
                    .iter()
                    .any(|frame| frame.element == "svg");
                text_block_for_element_name(
                    &frame.element,
                    in_svg,
                    self.nearest_fragment.clone(),
                    lang.clone(),
                    dir,
                    Vec::new(),
                )
            })
        else {
            return;
        };
        self.text_blocks.push(block);
    }

    fn finish_text_block(&mut self) {
        let Some(block) = self.text_blocks.pop() else {
            return;
        };
        let data = block.data();
        let text = normalize_whitespace(data.text);
        let element = data.element.to_string();
        let fragment = data.fragment.map(str::to_string);
        let structure_indices = data.structure_indices.to_vec();
        let is_heading = matches!(block, TextBlock::Heading(_));
        let text_metadata = match &block {
            TextBlock::Body(data)
            | TextBlock::PagebreakLabel(data)
            | TextBlock::FigureCaption(data)
            | TextBlock::TableCaption(data) => (data.lang.clone(), data.dir),
            TextBlock::Heading(data) => (data.lang.clone(), data.dir),
        };
        if is_heading {
            for index in &structure_indices {
                if let Some(structure @ StructureFact::Heading { .. }) =
                    self.facts.structure.get_mut(*index)
                {
                    structure.set_label((!text.is_empty()).then(|| text.clone()));
                }
            }
            if text.is_empty() {
                self.accessibility.push(AccessibilityFact::EmptyHeading(
                    AccessibilityElementFact {
                        element,
                        fragment: fragment.clone(),
                    },
                ));
            }
        }
        if matches!(block, TextBlock::PagebreakLabel(_)) {
            for index in &structure_indices {
                if let Some(structure @ StructureFact::Pagebreak { .. }) =
                    self.facts.structure.get_mut(*index)
                {
                    structure.set_label((!text.is_empty()).then(|| text.clone()));
                }
            }
        }
        if matches!(block, TextBlock::FigureCaption(_))
            && let Some(index) = self.parent_figure_structure_index()
            && let Some(structure) = self.facts.structure.get_mut(index)
        {
            structure.set_label((!text.is_empty()).then(|| text.clone()));
        }
        if matches!(block, TextBlock::TableCaption(_))
            && let Some(index) = self.parent_table_structure_index()
            && let Some(structure) = self.facts.structure.get_mut(index)
        {
            structure.set_label((!text.is_empty()).then(|| text.clone()));
        }
        let pagebreak_label = if !matches!(block, TextBlock::PagebreakLabel(_)) {
            structure_indices
                .iter()
                .find_map(|index| match self.facts.structure.get(*index) {
                    Some(StructureFact::Pagebreak { label, .. }) => Some(
                        label
                            .clone()
                            .or_else(|| (!text.is_empty()).then(|| text.clone())),
                    ),
                    _ => None,
                })
                .flatten()
        } else {
            None
        };
        if !text.is_empty() {
            let id = self.next_text_chunk_id();
            self.facts
                .text
                .push(block.into_text_chunk(id, text.clone()));
        }
        if let Some(label) = pagebreak_label {
            for index in &structure_indices {
                if let Some(structure @ StructureFact::Pagebreak { .. }) =
                    self.facts.structure.get_mut(*index)
                {
                    structure.set_label(Some(label.clone()));
                }
            }
            let id = self.next_text_chunk_id();
            let (lang, dir) = text_metadata;
            self.facts.text.push(TextChunk {
                id,
                kind: TextChunkKind::PagebreakLabel,
                content: TextChunkContent::Owned(label),
                fragment,
                lang,
                dir,
            });
        }
        self.flush_pending_alt_text();
    }

    fn finish_all_text_blocks(&mut self) {
        while !self.text_blocks.is_empty() {
            self.finish_text_block();
        }
        self.flush_pending_alt_text();
    }

    fn flush_pending_alt_text(&mut self) {
        for alt in std::mem::take(&mut self.pending_alt_text) {
            self.emit_alt_text(alt);
        }
    }

    fn emit_alt_text(&mut self, alt: PendingAltText) {
        let id = self.next_text_chunk_id();
        self.facts.text.push(TextChunk {
            id,
            kind: TextChunkKind::AltText,
            content: TextChunkContent::Owned(alt.text),
            fragment: alt.fragment,
            lang: alt.lang,
            dir: alt.dir,
        });
    }

    fn extract_fragments(&mut self, element: &str, attrs: &ElementAttrs, element_ordinal: usize) {
        if let Some(id) = attrs.value("id") {
            self.push_fragment(element, id, FragmentAttribute::Id, element_ordinal);
        }
        if let Some(id) = attrs.value("xml:id") {
            self.push_fragment(element, id, FragmentAttribute::XmlId, element_ordinal);
        }
    }

    fn extract_base(&mut self, element: &str, in_svg: bool, attrs: &ElementAttrs) {
        if in_svg || !element.eq_ignore_ascii_case("base") {
            return;
        }
        if self.authored_base.is_none()
            && let Some(href) = attrs.value("href")
        {
            self.authored_base = Some(AuthoredHref::new(href));
        }
    }

    fn extract_viewport(&mut self, element: &str, in_html: bool, attrs: &ElementAttrs) {
        if !in_html
            || self.in_svg()
            || !element.eq_ignore_ascii_case("meta")
            || !self
                .element_stack
                .iter()
                .any(|frame| frame.element.eq_ignore_ascii_case("head"))
            || !attrs
                .value("name")
                .is_some_and(|name| name.eq_ignore_ascii_case("viewport"))
        {
            return;
        }
        self.facts.viewports.push(ViewportFact::new(
            attrs.value("content").map(str::to_string),
        ));
    }

    fn extract_forms(&mut self, element: &str, attrs: &ElementAttrs, first_link: usize) {
        if element.eq_ignore_ascii_case("form") {
            let link = self.link_slot(first_link, &["action"]);
            self.facts.forms.push(FormFact::Form {
                fragment: self.nearest_fragment.clone(),
                method: attrs.value("method").map(str::to_string),
            });
            self.form_links.push(link);
        } else if element.eq_ignore_ascii_case("input")
            || element.eq_ignore_ascii_case("button")
            || element.eq_ignore_ascii_case("select")
            || element.eq_ignore_ascii_case("textarea")
        {
            let link = self.link_slot(first_link, &["formaction"]);
            self.facts.forms.push(FormFact::Control {
                element: element.to_string(),
                fragment: self.nearest_fragment.clone(),
                control_type: attrs.value("type").map(str::to_string),
                name: attrs.value("name").map(str::to_string),
                value: attrs.value("value").map(str::to_string),
            });
            self.form_links.push(link);
        }
    }

    fn extract_scripts(
        &mut self,
        element: &str,
        in_svg: bool,
        attrs: &ElementAttrs,
        first_link: usize,
    ) -> Option<usize> {
        attrs
            .values()
            .filter(|(name, _)| is_event_handler_attr(name))
            .for_each(|(name, _)| {
                self.facts.scripts.push(ScriptFact::EventHandler {
                    element: element.to_string(),
                    fragment: self.nearest_fragment.clone(),
                    attribute: name.to_string(),
                });
                self.script_links.push(None);
            });

        if !element.eq_ignore_ascii_case("script") {
            return None;
        }
        let script_type = attrs.value("type").map(str::to_string);
        let executable = in_svg || is_executable_script_type(script_type.as_deref());
        let svg_source_attribute = in_svg.then(|| selected_svg_href(attrs)).flatten();
        let source_attribute = if in_svg {
            svg_source_attribute
                .as_ref()
                .map(ReferenceAttribute::as_str)
        } else {
            attrs.value("src").map(|_| "src")
        };
        if executable && let Some(source_attribute) = source_attribute {
            let link = self
                .link_slot(first_link, &[source_attribute])
                .expect("external script link was extracted");
            self.facts.scripts.push(ScriptFact::External {
                fragment: self.nearest_fragment.clone(),
                script_type,
            });
            self.script_links.push(Some(link));
            None
        } else {
            let index = self.facts.scripts.len();
            let fact = if executable {
                ScriptFact::Inline {
                    fragment: self.nearest_fragment.clone(),
                    script_type,
                    has_text: false,
                }
            } else {
                ScriptFact::DataBlock {
                    fragment: self.nearest_fragment.clone(),
                    script_type,
                    has_text: false,
                }
            };
            self.facts.scripts.push(fact);
            self.script_links.push(None);
            Some(index)
        }
    }

    fn push_fragment(
        &mut self,
        element: &str,
        id: &str,
        attribute: FragmentAttribute,
        element_ordinal: usize,
    ) {
        self.nearest_fragment = Some(id.to_string());
        self.facts.fragments.push(FragmentFact::new(
            id.to_string(),
            element.to_string(),
            attribute,
            element_ordinal,
        ));
    }

    fn extract_structure(&mut self, element: &str, attrs: &ElementAttrs) -> Vec<usize> {
        let facts = structure_facts_for_element(self.nearest_fragment.clone(), element, attrs);
        let start = self.facts.structure.len();
        self.facts.structure.extend(facts);
        if let Some(level) = heading_level(element) {
            self.accessibility.push(AccessibilityFact::HeadingLevel(
                AccessibilityHeadingLevelFact {
                    element: element.to_string(),
                    fragment: self.nearest_fragment.clone(),
                    level,
                },
            ));
        }
        (start..self.facts.structure.len()).collect()
    }

    fn extract_accessibility(
        &mut self,
        element: &str,
        attrs: &ElementAttrs,
        lang: Option<String>,
        dir: Option<TextDirection>,
    ) {
        if let Some(value) = attrs.value("alt") {
            let normalized = normalize_whitespace(value);
            if normalized.is_empty() {
                self.accessibility.push(AccessibilityFact::EmptyImageAlt(
                    AccessibilityElementFact {
                        element: element.to_string(),
                        fragment: self.nearest_fragment.clone(),
                    },
                ));
            } else {
                if has_image_alt(element, attrs)
                    && self.text_stream_builder.in_body
                    && !self.text_suppressed()
                {
                    let alt = PendingAltText {
                        text: normalized,
                        fragment: self.nearest_fragment.clone(),
                        lang,
                        dir,
                    };
                    if self.text_blocks.is_empty() {
                        self.emit_alt_text(alt);
                    } else {
                        self.pending_alt_text.push(alt);
                    }
                }
                self.accessibility
                    .push(AccessibilityFact::ImageAlt(AccessibilityValueFact {
                        element: element.to_string(),
                        fragment: self.nearest_fragment.clone(),
                        value: value.to_string(),
                    }));
            }
        } else if has_image_alt(element, attrs) {
            self.accessibility.push(AccessibilityFact::MissingImageAlt(
                AccessibilityElementFact {
                    element: element.to_string(),
                    fragment: self.nearest_fragment.clone(),
                },
            ));
        }

        for attr in [
            "aria-label",
            "aria-labelledby",
            "aria-describedby",
            "role",
            "epub:type",
            "dir",
        ] {
            if let Some(value) = attrs.value(attr) {
                let fact = AccessibilityValueFact {
                    element: element.to_string(),
                    fragment: self.nearest_fragment.clone(),
                    value: value.to_string(),
                };
                self.accessibility.push(match attr {
                    "aria-label" => AccessibilityFact::AriaLabel(fact),
                    "aria-labelledby" => AccessibilityFact::AriaLabelledBy(fact),
                    "aria-describedby" => AccessibilityFact::AriaDescribedBy(fact),
                    "role" => AccessibilityFact::Role(fact),
                    "epub:type" => AccessibilityFact::EpubType(fact),
                    "dir" => AccessibilityFact::Dir(fact),
                    _ => unreachable!(),
                });
            }
        }

        if let Some(value) = attrs.value_any(&["lang", "xml:lang"]) {
            self.accessibility
                .push(AccessibilityFact::Lang(AccessibilityValueFact {
                    element: element.to_string(),
                    fragment: self.nearest_fragment.clone(),
                    value: value.to_string(),
                }));
        }
    }

    fn extract_media(&mut self, element: &str, attrs: &ElementAttrs, first_link: usize) {
        match element {
            "img" | "image" => self.push_media(
                MediaFact::Image {
                    element: element.to_string(),
                    alt: attrs.value("alt").map(str::to_string),
                },
                self.link_slots(first_link, &["src", "srcset", "href", "xlink:href"]),
            ),
            "input"
                if attrs
                    .value("type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("image")) =>
            {
                self.push_media(
                    MediaFact::Image {
                        element: element.to_string(),
                        alt: attrs.value("alt").map(str::to_string),
                    },
                    self.link_slots(first_link, &["src"]),
                )
            }
            "audio" => self.push_media(MediaFact::Audio, self.link_slots(first_link, &["src"])),
            "video" => {
                self.push_media(MediaFact::Video, self.link_slots(first_link, &["src"]));
                if attrs.value("poster").is_some() {
                    self.push_media(MediaFact::Poster, self.link_slots(first_link, &["poster"]));
                }
            }
            "source" => self.push_media(
                MediaFact::Source {
                    context: source_context(self.parent_element()),
                },
                self.link_slots(first_link, &["src", "srcset"]),
            ),
            "track" => self.push_media(
                MediaFact::Track {
                    kind: attrs.value("kind").map(str::to_string),
                    srclang: attrs.value("srclang").map(str::to_string),
                    label: attrs.value("label").map(str::to_string),
                },
                self.link_slots(first_link, &["src"]),
            ),
            _ => {}
        }
    }

    fn push_media(&mut self, fact: MediaFact, links: Vec<XhtmlLinkSlot>) {
        self.facts.media.push(fact);
        self.media_links.push(links);
    }

    fn link_slots(&self, first_link: usize, attributes: &[&str]) -> Vec<XhtmlLinkSlot> {
        self.links[first_link..]
            .iter()
            .enumerate()
            .filter(|(_, link)| attributes.contains(&link.attribute().as_str()))
            .map(|(offset, _)| XhtmlLinkSlot::new(first_link + offset))
            .collect()
    }

    fn link_slot(&self, first_link: usize, attributes: &[&str]) -> Option<XhtmlLinkSlot> {
        self.links[first_link..]
            .iter()
            .position(|link| attributes.contains(&link.attribute().as_str()))
            .map(|offset| XhtmlLinkSlot::new(first_link + offset))
    }

    fn extract_links(&mut self, element: &str, in_svg: bool, attrs: &ElementAttrs) {
        for (attribute, constructors, value) in
            link_attributes(element, in_svg, attrs, self.parent_element())
        {
            let declared = AuthoredHref::new(value.to_string());
            let navigation = (element == "a" && !self.in_svg())
                .then(|| self.navigation_link_kind())
                .flatten();
            self.links.push((constructors.fact)(LinkFactData {
                declared,
                element: element.to_string(),
                attribute,
                navigation,
            }));
        }
    }

    fn navigation_link_kind(&mut self) -> Option<NavigationLinkKind> {
        let nav = self
            .element_stack
            .iter()
            .rposition(|frame| frame.element == "nav")?;
        let kind = self.element_stack[nav].navigation?;
        let list_item = (nav + 1..self.element_stack.len())
            .rev()
            .find(|index| self.element_stack[*index].element == "li")?;
        let frame = &mut self.element_stack[list_item];
        if frame.navigation_link_claimed {
            return None;
        }
        frame.navigation_link_claimed = true;
        Some(kind)
    }

    fn emit_pagebreak_label(
        &mut self,
        attrs: &ElementAttrs,
        structure_indices: &[usize],
        lang: Option<String>,
        dir: Option<TextDirection>,
    ) {
        let Some(label) = pagebreak_label(attrs) else {
            return;
        };
        for index in structure_indices {
            if let Some(structure @ StructureFact::Pagebreak { .. }) =
                self.facts.structure.get_mut(*index)
            {
                structure.set_label(Some(label.clone()));
            }
        }
        let id = self.next_text_chunk_id();
        self.facts.text.push(TextChunk {
            id,
            kind: TextChunkKind::PagebreakLabel,
            content: TextChunkContent::Owned(label),
            fragment: self.nearest_fragment.clone(),
            lang,
            dir,
        });
    }

    fn derived_text_metadata(
        &self,
        attrs: &ElementAttrs,
    ) -> (Option<String>, Option<TextDirection>) {
        let (mut lang, mut dir) = self.current_text_metadata();
        if let Some(value) = attrs.value_any(&["lang", "xml:lang"]) {
            lang = Some(value.to_string());
        }
        if let Some(value) = attrs.value("dir") {
            dir = text_direction(value);
        }
        (lang, dir)
    }

    fn current_text_metadata(&self) -> (Option<String>, Option<TextDirection>) {
        self.element_stack
            .last()
            .map(|frame| (frame.lang.clone(), frame.dir))
            .unwrap_or((None, None))
    }

    fn text_suppressed(&self) -> bool {
        self.element_stack.iter().any(|frame| frame.suppresses_text)
    }

    fn parent_element(&self) -> Option<&str> {
        self.element_stack
            .last()
            .map(|frame| frame.element.as_str())
    }

    fn in_svg(&self) -> bool {
        self.element_stack
            .iter()
            .any(|frame| frame.element == "svg")
    }

    fn parent_figure_structure_index(&self) -> Option<usize> {
        self.element_stack.iter().rev().find_map(|frame| {
            frame.structure_indices.iter().copied().find(|index| {
                matches!(
                    self.facts.structure.get(*index),
                    Some(StructureFact::Figure { .. })
                )
            })
        })
    }

    fn parent_table_structure_index(&self) -> Option<usize> {
        self.element_stack.iter().rev().find_map(|frame| {
            frame.structure_indices.iter().copied().find(|index| {
                matches!(
                    self.facts.structure.get(*index),
                    Some(StructureFact::Table { .. })
                )
            })
        })
    }

    fn next_text_chunk_id(&mut self) -> String {
        self.next_text_chunk += 1;
        format!("t-{:06}", self.next_text_chunk)
    }
}

fn navigation_kind(element: &str, attrs: &ElementAttrs) -> Option<NavigationLinkKind> {
    if element != "nav" {
        return None;
    }
    attrs
        .values()
        .find(|(name, _)| *name == "epub:type" || name.ends_with(":type"))?
        .1
        .split_whitespace()
        .find_map(|token| match EpubStructuralSemantic::from_token(token) {
            Some(EpubStructuralSemantic::Toc) => Some(NavigationLinkKind::Toc),
            Some(EpubStructuralSemantic::PageList) => Some(NavigationLinkKind::PageList),
            Some(EpubStructuralSemantic::Landmarks) => Some(NavigationLinkKind::Landmark),
            _ => None,
        })
}

fn append_text(target: &mut String, value: &str) {
    target.push_str(value);
}

fn normalize_whitespace(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if is_text_whitespace(ch) {
            pending_space = true;
        } else {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.push(ch);
        }
    }
    normalized
}

fn is_text_whitespace(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\u{000C}' | '\r' | ' ')
}

fn is_text_stream_block(element: &str) -> bool {
    matches!(
        element,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "caption"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "figcaption"
            | "figure"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn heading_level(element: &str) -> Option<u8> {
    match element {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn text_block_for_element(
    element: &str,
    attrs: &ElementAttrs,
    in_svg: bool,
    fragment: Option<String>,
    lang: Option<String>,
    dir: Option<TextDirection>,
    structure_indices: Vec<usize>,
) -> Option<TextBlock> {
    if is_pagebreak(attrs)
        && pagebreak_label(attrs).is_none()
        && !preserves_native_pagebreak_content(element)
    {
        return Some(TextBlock::PagebreakLabel(TextBlockData {
            element: element.to_string(),
            fragment,
            text: String::new(),
            lang,
            dir,
            structure_indices,
            stream_range: None,
        }));
    }
    text_block_for_element_name(element, in_svg, fragment, lang, dir, structure_indices)
}

fn preserves_native_pagebreak_content(element: &str) -> bool {
    heading_level(element).is_some()
        || matches!(element, "figure" | "figcaption" | "table" | "caption")
}

fn text_block_for_element_name(
    element: &str,
    in_svg: bool,
    fragment: Option<String>,
    lang: Option<String>,
    dir: Option<TextDirection>,
    structure_indices: Vec<usize>,
) -> Option<TextBlock> {
    if let Some(level) = heading_level(element) {
        return Some(TextBlock::Heading(HeadingTextBlockData {
            element: element.to_string(),
            level,
            fragment,
            text: String::new(),
            lang,
            dir,
            structure_indices,
            stream_range: None,
        }));
    }
    match element {
        "p" | "li" | "blockquote" | "dt" | "dd" | "td" | "th" | "text" => {
            Some(TextBlock::Body(TextBlockData {
                element: element.to_string(),
                fragment,
                text: String::new(),
                lang,
                dir,
                structure_indices,
                stream_range: None,
            }))
        }
        "title" | "desc" if in_svg => Some(TextBlock::Body(TextBlockData {
            element: element.to_string(),
            fragment,
            text: String::new(),
            lang,
            dir,
            structure_indices,
            stream_range: None,
        })),
        "figcaption" => Some(TextBlock::FigureCaption(TextBlockData {
            element: element.to_string(),
            fragment,
            text: String::new(),
            lang,
            dir,
            structure_indices,
            stream_range: None,
        })),
        "caption" => Some(TextBlock::TableCaption(TextBlockData {
            element: element.to_string(),
            fragment,
            text: String::new(),
            lang,
            dir,
            structure_indices,
            stream_range: None,
        })),
        _ => None,
    }
}

fn suppresses_text(element: &str) -> bool {
    matches!(element, "script" | "style" | "template")
}

fn text_direction(value: &str) -> Option<TextDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ltr" => Some(TextDirection::Ltr),
        "rtl" => Some(TextDirection::Rtl),
        "auto" => Some(TextDirection::Auto),
        _ => None,
    }
}

fn is_pagebreak(attrs: &ElementAttrs) -> bool {
    has_epub_semantic(attrs, EpubStructuralSemantic::Pagebreak)
        || has_dpub_role(attrs, DpubAriaRole::Pagebreak)
}

fn has_image_alt(element: &str, attrs: &ElementAttrs) -> bool {
    matches!(element, "img" | "image")
        || element == "input"
            && attrs
                .value("type")
                .is_some_and(|value| value.eq_ignore_ascii_case("image"))
}

fn pagebreak_label(attrs: &ElementAttrs) -> Option<String> {
    attrs
        .value("title")
        .or_else(|| attrs.value("aria-label"))
        .map(normalize_whitespace)
        .filter(|value| !value.is_empty())
}

fn structure_facts_for_element(
    fragment: Option<String>,
    element: &str,
    attrs: &ElementAttrs,
) -> Vec<StructureFact> {
    let semantics = semantic_tokens(element, attrs);
    let mut facts = Vec::new();
    if let Some(level) = heading_level(element).and_then(HeadingLevel::new) {
        facts.push(StructureFact::Heading {
            label: None,
            fragment: fragment.clone(),
            level,
            semantics: semantics.clone(),
        });
    }
    if is_pagebreak(attrs) {
        facts.push(StructureFact::Pagebreak {
            label: pagebreak_label(attrs),
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if element == "figure" {
        facts.push(StructureFact::Figure {
            label: None,
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if element == "table" {
        facts.push(StructureFact::Table {
            label: None,
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if has_epub_semantic(attrs, EpubStructuralSemantic::Footnote)
        || has_dpub_role(attrs, DpubAriaRole::Footnote)
    {
        facts.push(StructureFact::Footnote {
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if has_epub_semantic(attrs, EpubStructuralSemantic::Endnote)
        || has_dpub_role(attrs, DpubAriaRole::Endnote)
    {
        facts.push(StructureFact::Endnote {
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if has_epub_semantic(attrs, EpubStructuralSemantic::Note)
        || attrs
            .value("role")
            .into_iter()
            .flat_map(str::split_whitespace)
            .any(|role| role.eq_ignore_ascii_case("note"))
    {
        facts.push(StructureFact::Note {
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if element == "nav" {
        facts.push(StructureFact::NavigationList {
            fragment: fragment.clone(),
            semantics: semantics.clone(),
        });
    }
    if publication_section(attrs) {
        facts.push(StructureFact::PublicationSection {
            fragment,
            semantics,
        });
    }
    facts
}

fn publication_section(attrs: &ElementAttrs) -> bool {
    [
        EpubStructuralSemantic::Chapter,
        EpubStructuralSemantic::Cover,
        EpubStructuralSemantic::Division,
        EpubStructuralSemantic::Part,
        EpubStructuralSemantic::Volume,
        EpubStructuralSemantic::Frontmatter,
        EpubStructuralSemantic::Bodymatter,
        EpubStructuralSemantic::Backmatter,
        EpubStructuralSemantic::Appendix,
        EpubStructuralSemantic::Bibliography,
        EpubStructuralSemantic::Glossary,
        EpubStructuralSemantic::Index,
        EpubStructuralSemantic::Endnotes,
        EpubStructuralSemantic::Footnotes,
        EpubStructuralSemantic::Preamble,
        EpubStructuralSemantic::Abstract,
        EpubStructuralSemantic::Acknowledgments,
        EpubStructuralSemantic::Afterword,
        EpubStructuralSemantic::Colophon,
        EpubStructuralSemantic::Conclusion,
        EpubStructuralSemantic::CopyrightPage,
        EpubStructuralSemantic::Credits,
        EpubStructuralSemantic::Dedication,
        EpubStructuralSemantic::Epigraph,
        EpubStructuralSemantic::Epilogue,
        EpubStructuralSemantic::Errata,
        EpubStructuralSemantic::Foreword,
        EpubStructuralSemantic::Introduction,
        EpubStructuralSemantic::Preface,
        EpubStructuralSemantic::Prologue,
        EpubStructuralSemantic::TitlePage,
    ]
    .iter()
    .any(|semantic| has_epub_semantic(attrs, *semantic))
        || [
            DpubAriaRole::Chapter,
            DpubAriaRole::Cover,
            DpubAriaRole::Part,
            DpubAriaRole::Appendix,
            DpubAriaRole::Bibliography,
            DpubAriaRole::Glossary,
            DpubAriaRole::Index,
            DpubAriaRole::Endnotes,
            DpubAriaRole::Abstract,
            DpubAriaRole::Acknowledgments,
            DpubAriaRole::Afterword,
            DpubAriaRole::Colophon,
            DpubAriaRole::Conclusion,
            DpubAriaRole::Credits,
            DpubAriaRole::Dedication,
            DpubAriaRole::Epigraph,
            DpubAriaRole::Epilogue,
            DpubAriaRole::Errata,
            DpubAriaRole::Foreword,
            DpubAriaRole::Introduction,
            DpubAriaRole::Preface,
            DpubAriaRole::Prologue,
        ]
        .iter()
        .any(|role| has_dpub_role(attrs, *role))
}

fn semantic_tokens(element: &str, attrs: &ElementAttrs) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    if let Some(value) = attrs.value("epub:type") {
        tokens.extend(value.split_whitespace().map(|raw| SemanticToken::EpubType {
            raw: raw.to_string(),
            semantic: EpubStructuralSemantic::from_token(raw),
        }));
    }
    if let Some(value) = attrs.value("role") {
        tokens.extend(value.split_whitespace().map(|raw| SemanticToken::AriaRole {
            raw: raw.to_string(),
            role: DpubAriaRole::from_html_token(raw),
        }));
    }
    if let Some(element) = html_structural_element(element) {
        tokens.push(SemanticToken::HtmlElement(element));
    }
    tokens
}

fn has_epub_semantic(attrs: &ElementAttrs, expected: EpubStructuralSemantic) -> bool {
    attrs
        .value("epub:type")
        .into_iter()
        .flat_map(str::split_whitespace)
        .any(|token| EpubStructuralSemantic::from_token(token) == Some(expected))
}

fn has_dpub_role(attrs: &ElementAttrs, expected: DpubAriaRole) -> bool {
    attrs
        .value("role")
        .into_iter()
        .flat_map(str::split_whitespace)
        .any(|token| DpubAriaRole::from_html_token(token) == Some(expected))
}

fn html_structural_element(element: &str) -> Option<HtmlStructuralElement> {
    match element {
        "section" => Some(HtmlStructuralElement::Section),
        "nav" => Some(HtmlStructuralElement::Navigation),
        "aside" => Some(HtmlStructuralElement::Aside),
        "figure" => Some(HtmlStructuralElement::Figure),
        "table" => Some(HtmlStructuralElement::Table),
        _ => heading_level(element).map(|level| {
            HtmlStructuralElement::Heading(
                HeadingLevel::new(level).expect("HTML heading level is valid"),
            )
        }),
    }
}

fn link_attributes(
    element: &str,
    in_svg: bool,
    attrs: &ElementAttrs,
    parent_element: Option<&str>,
) -> Vec<(ReferenceAttribute, LinkConstructors, String)> {
    let attributes = match element {
        "a" if in_svg => selected_svg_href(attrs)
            .map(|attribute| vec![(attribute, LinkConstructors::HYPERLINK)])
            .unwrap_or_default(),
        "a" => vec![(ReferenceAttribute::Href, LinkConstructors::HYPERLINK)],
        "area" => vec![(ReferenceAttribute::Href, LinkConstructors::HYPERLINK)],
        "link"
            if attrs.value("rel").is_some_and(|rel| {
                rel.split_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("stylesheet"))
            }) =>
        {
            vec![(ReferenceAttribute::Href, LinkConstructors::STYLESHEET)]
        }
        "form" => vec![(ReferenceAttribute::Action, LinkConstructors::FORM_ACTION)],
        "img" => vec![
            (ReferenceAttribute::Src, LinkConstructors::IMAGE),
            (ReferenceAttribute::Srcset, LinkConstructors::IMAGE),
        ],
        "input" => {
            let mut attributes = Vec::new();
            if attrs
                .value("type")
                .is_some_and(|value| value.eq_ignore_ascii_case("image"))
            {
                attributes.push((ReferenceAttribute::Src, LinkConstructors::IMAGE));
            }
            if attrs.value("formaction").is_some() && input_supports_formaction(attrs) {
                attributes.push((
                    ReferenceAttribute::FormAction,
                    LinkConstructors::FORM_ACTION,
                ));
            }
            attributes
        }
        "button" if attrs.value("formaction").is_some() && button_supports_formaction(attrs) => {
            vec![(
                ReferenceAttribute::FormAction,
                LinkConstructors::FORM_ACTION,
            )]
        }
        "script" if in_svg => selected_svg_href(attrs)
            .map(|attribute| vec![(attribute, LinkConstructors::SCRIPT)])
            .unwrap_or_default(),
        "script" if is_executable_script_type(attrs.value("type")) => {
            vec![(ReferenceAttribute::Src, LinkConstructors::SCRIPT)]
        }
        "script" => Vec::new(),
        "audio" => vec![(ReferenceAttribute::Src, LinkConstructors::AUDIO)],
        "video" => vec![
            (ReferenceAttribute::Src, LinkConstructors::VIDEO),
            (ReferenceAttribute::Poster, LinkConstructors::POSTER),
        ],
        "source" => match parent_element {
            Some("picture") => vec![(ReferenceAttribute::Srcset, LinkConstructors::IMAGE)],
            Some("audio") => vec![(ReferenceAttribute::Src, LinkConstructors::AUDIO)],
            Some("video") => vec![(ReferenceAttribute::Src, LinkConstructors::VIDEO)],
            _ => Vec::new(),
        },
        "track" => vec![(ReferenceAttribute::Src, LinkConstructors::TRACK)],
        "iframe" => vec![(ReferenceAttribute::Src, LinkConstructors::IFRAME)],
        "object" => vec![(ReferenceAttribute::Data, LinkConstructors::OBJECT)],
        "embed" => vec![(ReferenceAttribute::Src, LinkConstructors::EMBED)],
        "image" if in_svg => selected_svg_href(attrs)
            .map(|attribute| vec![(attribute, LinkConstructors::IMAGE)])
            .unwrap_or_default(),
        "use" if in_svg => selected_svg_href(attrs)
            .map(|attribute| vec![(attribute, LinkConstructors::SVG_REFERENCE)])
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    attributes
        .into_iter()
        .flat_map(|(attribute, constructors)| {
            let values = match attribute {
                ReferenceAttribute::Srcset => attrs
                    .value(attribute.as_str())
                    .map(parse_srcset_candidates)
                    .unwrap_or_default(),
                _ => attrs
                    .value(attribute.as_str())
                    .map(|value| vec![value.to_string()])
                    .unwrap_or_default(),
            };
            values
                .into_iter()
                .map(move |value| (attribute.clone(), constructors, value))
        })
        .collect()
}

fn selected_svg_href(attrs: &ElementAttrs) -> Option<ReferenceAttribute> {
    if attrs.value("href").is_some() {
        Some(ReferenceAttribute::Href)
    } else if attrs.value("xlink:href").is_some() {
        Some(ReferenceAttribute::XlinkHref)
    } else {
        None
    }
}

fn input_supports_formaction(attrs: &ElementAttrs) -> bool {
    attrs.value("type").is_some_and(|value| {
        value.eq_ignore_ascii_case("submit") || value.eq_ignore_ascii_case("image")
    })
}

fn button_supports_formaction(attrs: &ElementAttrs) -> bool {
    attrs.value("type").is_none_or(|value| {
        !value.eq_ignore_ascii_case("reset") && !value.eq_ignore_ascii_case("button")
    })
}

pub(crate) fn parse_srcset_candidates(value: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut position = 0;
    while position < value.len() {
        while position < value.len() {
            let ch = value[position..].chars().next().expect("character exists");
            if ch.is_ascii_whitespace() || ch == ',' {
                position += ch.len_utf8();
            } else {
                break;
            }
        }
        if position == value.len() {
            break;
        }

        let url_start = position;
        while position < value.len() {
            let ch = value[position..].chars().next().expect("character exists");
            if ch.is_ascii_whitespace() {
                break;
            }
            position += ch.len_utf8();
        }
        let mut url = &value[url_start..position];
        if url.ends_with(',') {
            url = url.trim_end_matches(',');
            if !url.is_empty() {
                candidates.push(url.to_string());
            }
            continue;
        }

        while position < value.len()
            && value[position..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_whitespace())
        {
            position += value[position..].chars().next().unwrap().len_utf8();
        }
        let descriptors_start = position;
        let mut parentheses = 0usize;
        while position < value.len() {
            let ch = value[position..].chars().next().expect("character exists");
            match ch {
                '(' => parentheses += 1,
                ')' => parentheses = parentheses.saturating_sub(1),
                ',' if parentheses == 0 => break,
                _ => {}
            }
            position += ch.len_utf8();
        }
        let descriptors = &value[descriptors_start..position];
        if position < value.len() {
            position += 1;
        }
        if !url.is_empty() && valid_srcset_descriptors(descriptors) {
            candidates.push(url.to_string());
        }
    }
    candidates
}

fn valid_srcset_descriptors(value: &str) -> bool {
    let mut width = false;
    let mut density = false;
    let mut height = false;
    for descriptor in value.split_ascii_whitespace() {
        let Some((suffix_index, suffix)) = descriptor.char_indices().next_back() else {
            return false;
        };
        let number = &descriptor[..suffix_index];
        let valid = match suffix {
            'w' if !width && !density => {
                width = number.bytes().all(|byte| byte.is_ascii_digit())
                    && number.parse::<u64>().is_ok_and(|value| value > 0);
                width
            }
            'x' if !density && !width && !height => {
                density = valid_html_float(number)
                    && number
                        .parse::<f64>()
                        .is_ok_and(|value| value.is_finite() && value > 0.0);
                density
            }
            'h' if !height && !density => {
                height = number.bytes().all(|byte| byte.is_ascii_digit())
                    && number.parse::<u64>().is_ok_and(|value| value > 0);
                height
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    !height || width
}

fn valid_html_float(value: &str) -> bool {
    let value = value.strip_prefix('-').unwrap_or(value);
    if value.is_empty() || value.starts_with('+') {
        return false;
    }
    let mut parts = value.split(['e', 'E']);
    let mantissa = parts.next().unwrap_or_default();
    let exponent = parts.next();
    if exponent.is_some_and(|value| {
        let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
        digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
    }) || parts.next().is_some()
    {
        return false;
    }
    match mantissa.split_once('.') {
        Some((integer, fraction)) => {
            !fraction.is_empty()
                && fraction.bytes().all(|byte| byte.is_ascii_digit())
                && (integer.is_empty() || integer.bytes().all(|byte| byte.is_ascii_digit()))
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

fn source_context(parent_element: Option<&str>) -> MediaSourceContext {
    match parent_element {
        Some("picture") => MediaSourceContext::Picture,
        Some("audio") => MediaSourceContext::Audio,
        Some("video") => MediaSourceContext::Video,
        _ => MediaSourceContext::Other,
    }
}

// Deliberate union of HTML event-handler content attributes and the embedded SVG
// animation/event attributes that represent executable script in EPUB content.
pub(crate) const EVENT_HANDLER_ATTRIBUTES: &[&str] = &[
    "onabort",
    "onactivate",
    "onafterprint",
    "onanimationcancel",
    "onanimationend",
    "onanimationiteration",
    "onanimationstart",
    "onauxclick",
    "onbeforeinput",
    "onbeforematch",
    "onbeforeprint",
    "onbeforetoggle",
    "onbeforeunload",
    "onbegin",
    "onblur",
    "oncancel",
    "oncanplay",
    "oncanplaythrough",
    "onchange",
    "onclick",
    "onclose",
    "oncommand",
    "oncontentvisibilityautostatechange",
    "oncontextlost",
    "oncontextmenu",
    "oncontextrestored",
    "oncopy",
    "oncuechange",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "ondurationchange",
    "onemptied",
    "onend",
    "onended",
    "onerror",
    "onfocus",
    "onfocusin",
    "onfocusout",
    "onformdata",
    "onfullscreenchange",
    "onfullscreenerror",
    "ongotpointercapture",
    "onhashchange",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onlanguagechange",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onlostpointercapture",
    "onmessage",
    "onmessageerror",
    "onmousedown",
    "onmouseenter",
    "onmouseleave",
    "onmousemove",
    "onmouseout",
    "onmouseover",
    "onmouseup",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpagereveal",
    "onpageshow",
    "onpageswap",
    "onpaste",
    "onpause",
    "onplay",
    "onplaying",
    "onpointercancel",
    "onpointerdown",
    "onpointerenter",
    "onpointerleave",
    "onpointermove",
    "onpointerout",
    "onpointerover",
    "onpointerrawupdate",
    "onpointerup",
    "onpopstate",
    "onprogress",
    "onratechange",
    "onrejectionhandled",
    "onrepeat",
    "onreset",
    "onresize",
    "onscroll",
    "onscrollend",
    "onsecuritypolicyviolation",
    "onseeked",
    "onseeking",
    "onselect",
    "onselectionchange",
    "onselectstart",
    "onslotchange",
    "onstalled",
    "onstorage",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    "ontransitioncancel",
    "ontransitionend",
    "ontransitionrun",
    "ontransitionstart",
    "onunhandledrejection",
    "onunload",
    "onvolumechange",
    "onwaiting",
    "onwheel",
    "onzoom",
];

pub(crate) fn is_event_handler_attr(name: &str) -> bool {
    EVENT_HANDLER_ATTRIBUTES.contains(&name.to_ascii_lowercase().as_str())
}

pub(crate) fn is_executable_script_type(script_type: Option<&str>) -> bool {
    let Some(script_type) = script_type.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if script_type.eq_ignore_ascii_case("module") {
        return true;
    }
    let essence = script_type
        .split_once(';')
        .map_or(script_type, |(essence, _)| essence)
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "text/javascript"
            | "application/javascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "application/x-ecmascript"
            | "text/x-javascript"
            | "text/x-ecmascript"
            | "text/livescript"
            | "text/jscript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
    )
}

#[cfg(test)]
mod parser_tests {
    use lol_html::{HtmlRewriter, Settings, element, text};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug)]
    struct SeenElement {
        name: String,
        attrs: Vec<(String, String)>,
    }

    fn collect(input: &[u8]) -> (Vec<SeenElement>, String) {
        let elements = Rc::new(RefCell::new(Vec::new()));
        let text_value = Rc::new(RefCell::new(String::new()));
        let element_sink = Rc::clone(&elements);
        let text_sink = Rc::clone(&text_value);
        let mut rewriter = HtmlRewriter::new(
            Settings::new()
                .append_element_content_handler(element!("*", move |element| {
                    element_sink.borrow_mut().push(SeenElement {
                        name: element.tag_name(),
                        attrs: element
                            .attributes()
                            .iter()
                            .map(|attr| (attr.name().to_string(), attr.value().to_string()))
                            .collect(),
                    });
                    Ok(())
                }))
                .append_element_content_handler(text!("*", move |chunk| {
                    text_sink.borrow_mut().push_str(chunk.as_str());
                    Ok(())
                })),
            |_: &[u8]| {},
        );
        rewriter.write(input).unwrap();
        rewriter.end().unwrap();
        (
            Rc::try_unwrap(elements).unwrap().into_inner(),
            Rc::try_unwrap(text_value).unwrap().into_inner(),
        )
    }

    fn has_attr(element: &SeenElement, name: &str, value: &str) -> bool {
        element
            .attrs
            .iter()
            .any(|(seen_name, seen_value)| seen_name == name && seen_value == value)
    }

    #[test]
    fn preserves_epub_xhtml_attribute_names() {
        let (elements, text) = collect(
            br##"<section epub:type="chapter" xml:lang="en"><p xml:id="p1" id="fallback">Text</p><svg><use xlink:href="#icon"/></svg></section>"##,
        );
        let section = elements
            .iter()
            .find(|element| element.name == "section")
            .unwrap();
        let paragraph = elements.iter().find(|element| element.name == "p").unwrap();
        let use_element = elements
            .iter()
            .find(|element| element.name == "use")
            .unwrap();
        assert!(has_attr(section, "epub:type", "chapter"));
        assert!(has_attr(section, "xml:lang", "en"));
        assert!(has_attr(paragraph, "xml:id", "p1"));
        assert!(has_attr(paragraph, "id", "fallback"));
        assert!(has_attr(use_element, "xlink:href", "#icon"));
        assert!(text.contains("Text"));
    }

    #[test]
    fn uses_html_style_raw_text_behavior() {
        let (elements, text) =
            collect(br#"<root><style><child id="x">Text</child></style></root>"#);
        assert!(elements.iter().any(|element| element.name == "root"));
        assert!(elements.iter().any(|element| element.name == "style"));
        assert!(!elements.iter().any(|element| element.name == "child"));
        assert!(text.contains(r#"<child id="x">Text</child>"#));
    }

    #[test]
    fn normalizes_xml_and_svg_names_but_preserves_colon_attributes() {
        let (elements, _) = collect(
            br##"<Root><svg><linearGradient id="g"><Stop offset="0"/></linearGradient><use xlink:href="#g"/></svg></Root>"##,
        );
        assert_eq!(
            elements
                .iter()
                .map(|element| element.name.as_str())
                .collect::<Vec<_>>(),
            ["root", "svg", "lineargradient", "stop", "use"]
        );
        let use_element = elements
            .iter()
            .find(|element| element.name == "use")
            .unwrap();
        assert!(has_attr(use_element, "xlink:href", "#g"));
    }

    #[test]
    fn parses_svg_links_and_text() {
        let (elements, text) = collect(
            br##"<svg><image href="cover.jpg"/><image xlink:href="alt.jpg"/><a href="chapter.xhtml"><text>Chapter</text></a><a xlink:href="legacy.xhtml"><text>Legacy</text></a><title>Diagram title</title><desc>Diagram desc</desc><use href="#symbol"/><use xlink:href="icons.svg#star"/></svg>"##,
        );
        for (name, value) in [
            ("href", "cover.jpg"),
            ("xlink:href", "alt.jpg"),
            ("href", "chapter.xhtml"),
            ("xlink:href", "legacy.xhtml"),
            ("href", "#symbol"),
            ("xlink:href", "icons.svg#star"),
        ] {
            assert!(
                elements
                    .iter()
                    .any(|element| has_attr(element, name, value))
            );
        }
        for value in ["Chapter", "Legacy", "Diagram title", "Diagram desc"] {
            assert!(text.contains(value));
        }
    }
}
