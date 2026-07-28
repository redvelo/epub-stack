use super::Epub;
use crate::{
    cfi::{
        Assertion, Cfi, CfiPath, CfiRange, CfiResolveError, LocalPath, Offset, RedirectedPath,
        ResolvedCfi, ResolvedCfiLocation, ResolvedCfiPoint, ResolvedCfiRange, Step,
    },
    resource::{
        ReadingOrderTargetRow, ResourceAddress, ResourceLookupError,
        provider::{ProviderReadError, ResourceProvider},
    },
    xml::decode_xml,
};
use xot::{Node, Xot};

impl<R: ResourceProvider> Epub<R> {
    /// Resolves an EPUB CFI when an application needs a current content location or range.
    ///
    /// Resolution reads and parses the referenced content document without a byte limit. Result
    /// locations use UTF-16 positions, as required by EPUB CFI, and describe the current committed
    /// publication state rather than an older detached analysis.
    /// ```
    /// # use epub_stack::Cfi;
    /// # use epub_stack::{Epub, MemoryResourceProvider};
    /// # use std::str::FromStr;
    /// let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>Example</dc:title><dc:identifier id="uid">example</dc:identifier><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref id="chapter-ref" idref="chapter"/></spine></package>"#;
    /// let provider = MemoryResourceProvider::from_entries([
    ///     ("EPUB/package.opf", package.to_vec()),
    ///     ("EPUB/chapter.xhtml", br#"<html><head/><body id="body"><p id="line">Hello world.</p></body></html>"#.to_vec()),
    /// ]).unwrap();
    /// let book = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
    /// let cfi = Cfi::from_str("epubcfi(/6/2[chapter-ref]!/4[body]/2[line],/1:0,/1:5)").unwrap();
    /// let cfi_range = cfi.range().expect("range");
    /// assert_eq!(book.text_from_cfi_range(cfi_range).unwrap(), "Hello");
    /// ```
    pub fn resolve_cfi(&self, cfi: &Cfi) -> std::result::Result<ResolvedCfi, CfiResolveError> {
        match cfi.range() {
            Some(range) => self
                .resolve_cfi_range(range)
                .map(Box::new)
                .map(ResolvedCfi::Range),
            None => self
                .resolve_cfi_point(cfi.path())
                .map(Box::new)
                .map(ResolvedCfi::Point),
        }
    }

    /// Returns the source text selected by a CFI range in the current publication state.
    pub fn text_from_cfi_range(
        &self,
        cfi: &CfiRange,
    ) -> std::result::Result<String, CfiResolveError> {
        Ok(self.resolve_cfi_range(cfi)?.text().to_string())
    }

    /// Resolves a CFI range for applications that need its resource, endpoints, and source text.
    pub fn resolve_cfi_range(
        &self,
        cfi: &CfiRange,
    ) -> std::result::Result<ResolvedCfiRange, CfiResolveError> {
        let (source, xot, root_node, document_path) = self.resolve_cfi_document(cfi.parent())?;
        if document_path.offset().is_some() || document_path.redirected().is_some() {
            return Err(CfiResolveError::RedirectedPathUnsupported);
        }
        let parent_node = if document_path.steps().is_empty() {
            root_node
        } else {
            get_node_from_steps_xot(&xot, root_node, document_path.steps())
                .ok_or(CfiResolveError::ParentNodeUnresolved)?
        };

        ensure_local_path_supported(cfi.start())?;
        ensure_local_path_supported(cfi.end())?;
        let start = resolve_local_path_location(&xot, parent_node, cfi.start())?;
        let end = resolve_local_path_location(&xot, parent_node, cfi.end())?;
        let start_offset = character_offset(cfi.start())?;
        let end_offset = character_offset(cfi.end())?;
        if start_offset.is_some() && !start.node().is_some_and(|node| xot.is_text(node)) {
            return Err(CfiResolveError::OffsetNotText);
        }
        if end_offset.is_some() && !end.node().is_some_and(|node| xot.is_text(node)) {
            return Err(CfiResolveError::OffsetNotText);
        }
        if let Some(node) = start.node() {
            validate_text_assertion(&xot, root_node, node, cfi.start().offset())?;
        }
        if let Some(node) = end.node() {
            validate_text_assertion(&xot, root_node, node, cfi.end().offset())?;
        }
        let text = xot
            .descendants(root_node)
            .filter_map(|node| xot.text_str(node))
            .collect::<String>();
        let start_position = location_utf16_position(&xot, root_node, start, start_offset)?;
        let end_position = location_utf16_position(&xot, root_node, end, end_offset)?;
        if end_position < start_position {
            return Err(CfiResolveError::ReversedRange);
        }
        let selected = slice_utf16(&text, Some(start_position), Some(end_position))?;
        let start = ResolvedCfiLocation::new(
            document_path.clone(),
            Some(cfi.start().clone()),
            start_position,
        );
        let end = ResolvedCfiLocation::new(document_path, Some(cfi.end().clone()), end_position);
        Ok(ResolvedCfiRange::new(source, start, end, selected))
    }

    fn resolve_cfi_point(
        &self,
        cfi: &CfiPath,
    ) -> std::result::Result<ResolvedCfiPoint, CfiResolveError> {
        let (source, xot, root_node, document_path) = self.resolve_cfi_document(cfi)?;
        if document_path.redirected().is_some() {
            return Err(CfiResolveError::RedirectedPathUnsupported);
        }
        let local = document_path.as_local_path();
        let location = resolve_local_path_location(&xot, root_node, &local)?;
        let offset = character_offset(&local)?;
        if offset.is_some() && !location.node().is_some_and(|node| xot.is_text(node)) {
            return Err(CfiResolveError::OffsetNotText);
        }
        if let Some(node) = location.node() {
            validate_text_assertion(&xot, root_node, node, local.offset())?;
        }
        let position = location_utf16_position(&xot, root_node, location, offset)?;
        Ok(ResolvedCfiPoint::new(
            source,
            ResolvedCfiLocation::new(document_path, None, position),
        ))
    }

    fn resolve_cfi_document(
        &self,
        cfi: &CfiPath,
    ) -> std::result::Result<(ResourceAddress, Xot, Node, CfiPath), CfiResolveError> {
        let steps = cfi.steps();
        let spine_step = steps.first().ok_or(CfiResolveError::ParentPathEmpty)?;
        if spine_step.step() % 2 != 0 {
            return Err(CfiResolveError::SpineStepOdd {
                step: spine_step.step(),
            });
        }
        if spine_step.step() != 6 {
            return Err(CfiResolveError::SpineStepInvalid {
                step: spine_step.step(),
            });
        }
        if let Some(assertion) = spine_step.assertion()
            && !assertion.values().is_empty()
            && !assertion.values().iter().all(|value| {
                self.package().spine().id().map(|id| id.as_ref()) == Some(value.as_str())
            })
        {
            return Err(CfiResolveError::SpineAssertionMismatch);
        }
        let page_step = steps.get(1).ok_or(CfiResolveError::ParentPathTooShort)?;
        if page_step.step() % 2 != 0 {
            return Err(CfiResolveError::SpineItemrefStepOdd {
                step: page_step.step(),
            });
        }
        let page = page_step
            .step()
            .checked_div(2)
            .and_then(|value| value.checked_sub(1))
            .ok_or(CfiResolveError::InvalidSpineItemrefStep)?;
        let itemref = self
            .package()
            .spine()
            .itemrefs()
            .get(page)
            .ok_or(CfiResolveError::InvalidLocation)?;
        if let Some(assertion) = page_step.assertion()
            && !assertion.values().is_empty()
            && !assertion
                .values()
                .iter()
                .all(|value| itemref.id().map(|id| id.as_ref()) == Some(value.as_str()))
        {
            return Err(CfiResolveError::SpineItemrefAssertionMismatch);
        }
        let reading_order = self
            .resources
            .reading_order()
            .nth(page)
            .ok_or(CfiResolveError::InvalidLocation)?;
        let record = match reading_order.target_row() {
            ReadingOrderTargetRow::Declaration {
                resource: Some(resource),
                ..
            } => self
                .resources
                .resource((*resource).into())
                .map_err(|_| CfiResolveError::InvalidLocation)?,
            ReadingOrderTargetRow::Declaration {
                declaration,
                resource: None,
            } => {
                return Err(CfiResolveError::ResourceLookup {
                    source: ResourceLookupError::UnresolvedDeclaration(
                        crate::resource::ManifestOrdinal::from_index(declaration.0),
                    ),
                });
            }
            ReadingOrderTargetRow::MissingIdref => return Err(CfiResolveError::InvalidLocation),
            ReadingOrderTargetRow::MissingManifestId => {
                return Err(CfiResolveError::InvalidLocation);
            }
            ReadingOrderTargetRow::AmbiguousManifestId { candidates } => {
                let id = reading_order
                    .idref()
                    .map(|idref| idref.as_str())
                    .unwrap_or_default();
                return Err(CfiResolveError::ResourceLookup {
                    source: ResourceLookupError::AmbiguousManifestId {
                        id: crate::package::normalize_manifest_id(id)
                            .map_err(|_| CfiResolveError::InvalidLocation)?
                            .to_string(),
                        candidates: candidates.iter().copied().map(|row| row.into()).collect(),
                    },
                });
            }
        };
        let source = record.address().clone();
        let path = source
            .local_path()
            .ok_or(CfiResolveError::InvalidLocation)?
            .clone();
        let bytes = if let Some(change) = self.resource_changes.entry(path.as_path()) {
            change
                .map(Vec::from)
                .ok_or_else(|| CfiResolveError::ResourceRead {
                    path: path.clone(),
                    source: ProviderReadError::MissingResource { path: path.clone() },
                })?
        } else {
            self.container
                .read_with(&path, |reader| {
                    let mut bytes = Vec::new();
                    reader
                        .read_to_end(&mut bytes)
                        .map_err(|source| ProviderReadError::IoPath {
                            source,
                            path: path.clone(),
                        })?;
                    Ok::<_, ProviderReadError>(bytes)
                })
                .map_err(|source| CfiResolveError::ResourceRead {
                    path: path.clone(),
                    source,
                })?
                .map_err(|source| CfiResolveError::ResourceRead {
                    path: path.clone(),
                    source,
                })?
        };
        let xhtml = decode_xml(&bytes).map_err(|source| CfiResolveError::XmlDecode {
            path: path.clone(),
            source,
        })?;
        let mut xot = Xot::new();
        xot.set_text_consolidation(true);
        let doc = xot.parse(xhtml.as_ref()).map_err(CfiResolveError::from)?;
        let root_node = xot
            .document_element(doc)
            .map_err(|source| CfiResolveError::Xml { source })?;

        let redirected = match cfi.redirected() {
            Some(RedirectedPath::Path(path)) => path.as_ref().clone(),
            Some(RedirectedPath::Offset(_)) => {
                return Err(CfiResolveError::PackageRedirectOffset);
            }
            None => {
                return Err(CfiResolveError::PackageRedirectMissing);
            }
        };
        Ok((source, xot, root_node, redirected))
    }
}

fn ensure_local_path_supported(path: &LocalPath) -> std::result::Result<(), CfiResolveError> {
    if path.redirected().is_some() {
        return Err(CfiResolveError::LocalPathRedirectUnsupported);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum XmlCfiLocation {
    Node(Node),
    BeforeChildren(Node),
    AfterChildren(Node),
}

impl XmlCfiLocation {
    fn node(self) -> Option<Node> {
        match self {
            Self::Node(node) => Some(node),
            Self::BeforeChildren(_) | Self::AfterChildren(_) => None,
        }
    }
}

fn resolve_local_path_location(
    xot: &Xot,
    node: Node,
    path: &LocalPath,
) -> std::result::Result<XmlCfiLocation, CfiResolveError> {
    let Some((last, prefix)) = path.steps().split_last() else {
        return Ok(XmlCfiLocation::Node(node));
    };
    let parent =
        get_node_from_steps_xot(xot, node, prefix).ok_or(CfiResolveError::LocalPathUnresolved)?;
    if last.step() == 0 {
        if !xot.is_element(parent) {
            return Err(CfiResolveError::LocalPathUnresolved);
        }
        return Ok(XmlCfiLocation::BeforeChildren(parent));
    }
    if last.step() % 2 != 0 {
        return get_odd_step(xot, parent, last)
            .map(XmlCfiLocation::Node)
            .ok_or(CfiResolveError::LocalPathUnresolved);
    }
    if let Some(node) =
        check_node_assertion(xot, get_even_step(xot, parent, last), last.assertion())
    {
        return Ok(XmlCfiLocation::Node(node));
    }
    let trailing_step = xot
        .children(parent)
        .filter(|child| xot.is_element(*child))
        .count()
        .checked_add(1)
        .and_then(|count| count.checked_mul(2));
    if xot.is_element(parent) && trailing_step == Some(last.step()) && last.assertion().is_none() {
        return Ok(XmlCfiLocation::AfterChildren(parent));
    }
    Err(CfiResolveError::LocalPathUnresolved)
}

fn location_utf16_position(
    xot: &Xot,
    root: Node,
    location: XmlCfiLocation,
    offset: Option<usize>,
) -> std::result::Result<usize, CfiResolveError> {
    let (node, after) = match location {
        XmlCfiLocation::Node(node) => (node, false),
        XmlCfiLocation::BeforeChildren(node) => (node, false),
        XmlCfiLocation::AfterChildren(node) => (node, true),
    };
    let before = utf16_position_before_node(xot, root, node)?;
    if let Some(offset) = offset {
        let text_length = xot
            .text_str(node)
            .ok_or(CfiResolveError::MissingTextNode)?
            .encode_utf16()
            .count();
        if offset > text_length {
            return Err(CfiResolveError::InvalidOffsetRange);
        }
        return before
            .checked_add(offset)
            .ok_or(CfiResolveError::InvalidOffsetRange);
    }
    if after {
        return before
            .checked_add(subtree_utf16_len(xot, node))
            .ok_or(CfiResolveError::InvalidOffsetRange);
    }
    Ok(before)
}

fn utf16_position_before_node(
    xot: &Xot,
    root: Node,
    target: Node,
) -> std::result::Result<usize, CfiResolveError> {
    if root == target {
        return Ok(0);
    }
    let mut position = 0usize;
    for node in xot.descendants(root) {
        if node == target {
            return Ok(position);
        }
        if let Some(text) = xot.text_str(node) {
            position = position
                .checked_add(text.encode_utf16().count())
                .ok_or(CfiResolveError::InvalidOffsetRange)?;
        }
    }
    Err(CfiResolveError::LocalPathUnresolved)
}

fn subtree_utf16_len(xot: &Xot, node: Node) -> usize {
    if let Some(text) = xot.text_str(node) {
        return text.encode_utf16().count();
    }
    xot.descendants(node)
        .filter_map(|node| xot.text_str(node))
        .map(|text| text.encode_utf16().count())
        .sum()
}

fn character_offset(path: &LocalPath) -> std::result::Result<Option<usize>, CfiResolveError> {
    match path.offset() {
        None if path.steps().last().is_some_and(|step| step.step() % 2 != 0) => Ok(Some(0)),
        None => Ok(None),
        Some(offset) if offset.character_value().is_some() => Ok(offset.character_value()),
        Some(_) => Err(CfiResolveError::UnsupportedOffset),
    }
}

fn slice_utf16(
    text: &str,
    start: Option<usize>,
    end: Option<usize>,
) -> std::result::Result<String, CfiResolveError> {
    let text_u16: Vec<u16> = text.encode_utf16().collect();
    let start_index = start.unwrap_or(0);
    let end_index = end.unwrap_or(text_u16.len());
    if start_index > end_index || end_index > text_u16.len() {
        return Err(CfiResolveError::InvalidOffsetRange);
    }
    let slice = text_u16
        .get(start_index..end_index)
        .ok_or(CfiResolveError::InvalidOffsetRange)?;
    String::from_utf16(slice).map_err(|source| CfiResolveError::Utf16 { source })
}

fn get_node_from_steps_xot(xot: &Xot, node: Node, steps: &[Step]) -> Option<Node> {
    steps.iter().try_fold(node, |node, step| {
        if step.step() % 2 == 0 {
            let next = get_even_step(xot, node, step);
            check_node_assertion(xot, next, step.assertion())
        } else {
            get_odd_step(xot, node, step)
        }
    })
}

fn validate_text_assertion(
    xot: &Xot,
    root: Node,
    node: Node,
    offset: Option<&Offset>,
) -> std::result::Result<(), CfiResolveError> {
    let Some((index, Some(assertion))) = offset.and_then(Offset::as_character) else {
        return Ok(());
    };
    if assertion.values().is_empty() {
        return Ok(());
    }
    let text = xot.text_str(node).ok_or(CfiResolveError::MissingTextNode)?;
    let text_u16 = text.encode_utf16().collect::<Vec<_>>();
    let node_before = String::from_utf16(
        text_u16
            .get(..index)
            .ok_or(CfiResolveError::InvalidOffsetRange)?,
    )
    .map_err(|source| CfiResolveError::Utf16 { source })?;
    let node_after = String::from_utf16(
        text_u16
            .get(index..)
            .ok_or(CfiResolveError::InvalidOffsetRange)?,
    )
    .map_err(|source| CfiResolveError::Utf16 { source })?;
    let mut before = String::new();
    let mut after = String::new();
    let mut found = false;
    for current in xot.descendants(root) {
        if current == node {
            before.push_str(&node_before);
            after.push_str(&node_after);
            found = true;
        } else if let Some(text) = xot.text_str(current) {
            if found {
                after.push_str(text);
            } else {
                before.push_str(text);
            }
        }
    }
    if !found {
        return Err(CfiResolveError::MissingTextNode);
    }
    let (expected_before, expected_after) = if assertion.preceding_comma() {
        (None, assertion.values().first())
    } else {
        (assertion.values().first(), assertion.values().get(1))
    };
    let before = collapse_whitespace(&before);
    let after = collapse_whitespace(&after);
    let matches = expected_before
        .is_none_or(|expected| before.ends_with(&collapse_whitespace(expected)))
        && expected_after.is_none_or(|expected| after.starts_with(&collapse_whitespace(expected)));
    if !matches {
        return Err(CfiResolveError::TextAssertionMismatch);
    }
    Ok(())
}

fn collapse_whitespace(text: &str) -> String {
    let mut output = String::new();
    let mut in_whitespace = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_whitespace {
                output.push(' ');
                in_whitespace = true;
            }
        } else {
            output.push(ch);
            in_whitespace = false;
        }
    }
    output
}

fn get_even_step(xot: &Xot, node: Node, step: &Step) -> Option<Node> {
    let step_index = step.step().checked_div(2)?.checked_sub(1)?;
    xot.children(node)
        .filter(|n| xot.is_element(*n))
        .enumerate()
        .find_map(|(index, n)| (step_index == index).then_some(n))
}

fn get_odd_step(xot: &Xot, node: Node, step: &Step) -> Option<Node> {
    let step_index = step.step().checked_sub(1)?.checked_div(2)?;
    let chunks = text_chunks(xot, node);
    chunks.get(step_index).and_then(|chunk| *chunk)
}

fn text_chunks(xot: &Xot, node: Node) -> Vec<Option<Node>> {
    let mut chunks = vec![None];
    for child in xot.children(node) {
        if xot.is_text(child) {
            if let Some(last) = chunks.last_mut()
                && last.is_none()
            {
                *last = Some(child);
            }
        } else if xot.is_element(child) {
            chunks.push(None);
        }
    }
    chunks
}

fn check_node_assertion(
    xot: &Xot,
    node: Option<Node>,
    assertion: Option<&Assertion>,
) -> Option<Node> {
    let node = node?;
    let Some(assertion) = assertion else {
        return Some(node);
    };
    if assertion.preceding_comma() || assertion.values().is_empty() {
        return Some(node);
    }
    let name_id = xot.name("id");
    let xml_id = xot
        .namespace("http://www.w3.org/XML/1998/namespace")
        .and_then(|namespace| xot.name_ns("id", namespace));
    assertion
        .values()
        .iter()
        .all(|value| {
            name_id.and_then(|name| xot.get_attribute(node, name)) == Some(value.as_str())
                || xml_id.and_then(|name| xot.get_attribute(node, name)) == Some(value.as_str())
        })
        .then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryResourceProvider, cfi::CfiXmlDecodeError};

    const PACKAGE: &[u8] = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>CFI fixture</dc:title><dc:identifier id="uid">fixture</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref id="chapter-ref" idref="chapter"/></spine>
</package>"#;

    const CHAPTER: &str = concat!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml">"#,
        "\n<head><title>Fixture</title></head>\n",
        "<body id=\"body\">\n",
        "<p>one</p><p>two</p><p>three</p><p>four</p>",
        "<p id=\"target\">xxx<em>yyy</em>0123456789</p>",
        "<p>six</p><p>seven</p>",
        "</body></html>",
    );

    fn cfi_epub(chapter: Vec<u8>) -> Epub<MemoryResourceProvider> {
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", PACKAGE.to_vec()),
            ("EPUB/chapter.xhtml", chapter),
        ])
        .unwrap();
        Epub::from_provider(provider, "EPUB/package.opf").unwrap()
    }

    fn fixture_epub() -> Epub<MemoryResourceProvider> {
        cfi_epub(CHAPTER.as_bytes().to_vec())
    }

    fn utf16_xml(xml: &str, little_endian: bool) -> Vec<u8> {
        let mut bytes = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in xml.encode_utf16() {
            bytes.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    #[test]
    fn resolves_ranges_assertions_and_package_failures() {
        let epub = fixture_epub();
        let parent = "/6/2[chapter-ref]!/4[body]/10[target]/2";
        for (suffix, expected) in [
            (",/1:0,/1:3", "yyy"),
            (",/1:0[,yyy],/1:3[yyy]", "yyy"),
            (",/1:0[xxx,yyy],/1:3[yyy,012]", "yyy"),
        ] {
            let range = format!("{parent}{suffix}").parse::<CfiRange>().unwrap();
            assert_eq!(epub.text_from_cfi_range(&range).unwrap(), expected);
        }

        let across_sibling_nodes = "/6/2[chapter-ref]!/4[body]/10[target],/2/1:1,/3:4"
            .parse::<CfiRange>()
            .unwrap();
        assert_eq!(
            epub.text_from_cfi_range(&across_sibling_nodes).unwrap(),
            "yy0123"
        );
        let across_elements = "/6/2[chapter-ref]!/4[body],/10[target]/3:0,/12/1:3"
            .parse::<CfiRange>()
            .unwrap();
        assert_eq!(
            epub.text_from_cfi_range(&across_elements).unwrap(),
            "0123456789six"
        );

        let mismatch = format!("{parent},/1:0[,wrong],/1:3[yyy]")
            .parse::<CfiRange>()
            .unwrap();
        assert!(matches!(
            epub.text_from_cfi_range(&mismatch),
            Err(CfiResolveError::TextAssertionMismatch)
        ));

        for (input, expected) in [
            (
                "/4/2[chapter-ref]!/4[body]/10[target]/2,/1:0,/1:3",
                CfiResolveError::SpineStepInvalid { step: 4 },
            ),
            (
                "/6[wrong]/2[chapter-ref]!/4[body]/10[target]/2,/1:0,/1:3",
                CfiResolveError::SpineAssertionMismatch,
            ),
            (
                "/6/2[wrong]!/4[body]/10[target]/2,/1:0,/1:3",
                CfiResolveError::SpineItemrefAssertionMismatch,
            ),
        ] {
            let error = epub
                .text_from_cfi_range(&input.parse::<CfiRange>().unwrap())
                .unwrap_err();
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected)
            );
            if let CfiResolveError::SpineStepInvalid { step } = error {
                assert_eq!(step, 4);
            }
        }
    }

    #[test]
    fn resolves_empty_virtual_and_implicit_text_boundaries() {
        let epub = fixture_epub();
        let parent = "/6/2[chapter-ref]!/4[body]/10[target]";
        for (suffix, expected) in [
            (",,", ""),
            (",,/2", "xxx"),
            (",/2,/2", ""),
            (",/0,/4", "xxxyyy0123456789"),
            (",/1,/1:2", "xx"),
        ] {
            let range = format!("{parent}{suffix}").parse::<CfiRange>().unwrap();
            assert_eq!(epub.text_from_cfi_range(&range).unwrap(), expected);
        }

        let reversed = format!("{parent},/2,").parse::<CfiRange>().unwrap();
        assert!(matches!(
            epub.text_from_cfi_range(&reversed),
            Err(CfiResolveError::ReversedRange)
        ));
        for invalid in ["/1/0", "/1/2"] {
            let range = format!("{parent},{invalid},").parse::<CfiRange>().unwrap();
            assert!(matches!(
                epub.text_from_cfi_range(&range),
                Err(CfiResolveError::LocalPathUnresolved)
            ));
        }
    }

    #[test]
    fn element_assertions_recognize_xml_id() {
        let epub = cfi_epub(
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title/></head><body id="body"><p xml:id="xml-target">xml id text</p></body></html>"#.to_vec(),
        );
        let range = "/6/2[chapter-ref]!/4[body]/2[xml-target],/0,/2"
            .parse::<CfiRange>()
            .unwrap();
        assert_eq!(epub.text_from_cfi_range(&range).unwrap(), "xml id text");
    }

    #[test]
    fn resolves_utf16_little_and_big_endian_content_documents() {
        for little_endian in [true, false] {
            let xml = r#"<?xml version="1.0" encoding="UTF-16"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>Hello world</p></body></html>"#;
            let epub = cfi_epub(utf16_xml(xml, little_endian));
            let cfi = "epubcfi(/6/2[chapter-ref]!/2/2,/1:0,/1:5)"
                .parse::<Cfi>()
                .unwrap();
            let ResolvedCfi::Range(range) = epub.resolve_cfi(&cfi).unwrap() else {
                panic!("expected a range");
            };
            assert_eq!(range.text(), "Hello");
            assert_eq!(range.start().utf16_position(), 0);
            assert_eq!(range.end().utf16_position(), 5);
        }
    }

    #[test]
    fn non_bmp_text_uses_utf16_positions_and_ranges() {
        let epub = cfi_epub(
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>A😀BC</p></body></html>"#
                .as_bytes()
                .to_vec(),
        );
        let point = "epubcfi(/6/2[chapter-ref]!/2/2/1:3)"
            .parse::<Cfi>()
            .unwrap();
        let ResolvedCfi::Point(point) = epub.resolve_cfi(&point).unwrap() else {
            panic!("expected a point");
        };
        assert_eq!(point.location().utf16_position(), 3);

        let range = "epubcfi(/6/2[chapter-ref]!/2/2,/1:1,/1:3)"
            .parse::<Cfi>()
            .unwrap();
        let ResolvedCfi::Range(range) = epub.resolve_cfi(&range).unwrap() else {
            panic!("expected a range");
        };
        assert_eq!(range.text(), "😀");
        assert_eq!(range.start().utf16_position(), 1);
        assert_eq!(range.end().utf16_position(), 3);

        let split_surrogate = "epubcfi(/6/2[chapter-ref]!/2/2,/1:1,/1:2)"
            .parse::<Cfi>()
            .unwrap();
        assert!(matches!(
            epub.resolve_cfi(&split_surrogate),
            Err(CfiResolveError::Utf16 { .. })
        ));
    }

    #[test]
    fn xml_decode_errors_carry_the_content_document_path() {
        let epub = cfi_epub(b"<html>\xff</html>".to_vec());
        let cfi = "epubcfi(/6/2[chapter-ref]!/2)".parse::<Cfi>().unwrap();
        assert!(matches!(
            epub.resolve_cfi(&cfi),
            Err(CfiResolveError::XmlDecode {
                path,
                source: CfiXmlDecodeError::InvalidBytes { encoding: "UTF-8" }
            }) if path.as_str() == "EPUB/chapter.xhtml"
        ));
    }
}
