use super::collection::{Collection, CollectionRole, MAX_COLLECTION_NESTING_DEPTH};
use super::legacy::{Guide, Opf2Meta, Reference, ReferenceType};
use super::manifest::{Manifest, ManifestItem, ManifestPropertyToken};
use super::metadata::{
    Element, LinkPropertyToken, LinkRelToken, Meta, MetaPropertyToken, Metadata, MetadataLink,
};
use super::spine::{ItemRef, Linear, PageProgressionDirection, Spine, SpinePropertyToken};
use super::*;
use crate::resource::AuthoredHref;
use crate::string::optional_epub_string;
use crate::xml::{cdata_content, push_general_ref, text_content};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use std::{io::BufRead, str::FromStr};

impl Package {
    /// Parses UTF-8 OPF XML into an owned semantic package model.
    ///
    /// Use the returned [`Package`] to inspect metadata, manifest resources, reading order, and
    /// collections. Parsing is namespace-aware and preserves modeled group order, authored
    /// hrefs, and vocabulary-token spellings after any surrounding whitespace represented by
    /// [`EpubString`] is trimmed. Empty modeled scalars generally become absent. Unknown XML,
    /// malformed typed values, the source tree, and lexical formatting are not retained.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] for malformed XML, a missing or invalid document element,
    /// duplicate spines, I/O failures from XML parsing, or excessive collection nesting.
    pub fn parse(xml: &str) -> Result<Self> {
        parse_package(xml.as_bytes())
    }
}

fn normalize_attr_byte(byte: u8) -> u8 {
    if byte == b'-' { b'_' } else { byte }
}

fn attr_key_matches(attr: &[u8], key: &[u8]) -> bool {
    if attr.len() != key.len() {
        return false;
    }
    attr.iter()
        .zip(key)
        .all(|(attr, key)| normalize_attr_byte(*attr) == normalize_attr_byte(*key))
}

struct PackageEventAttrs {
    values: Vec<(PackageAttrNamespace, Vec<u8>, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageAttrNamespace {
    Unbound,
    Xml,
    Opf,
    Other,
}

impl PackageEventAttrs {
    fn new<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Self {
        let mut values = Vec::new();
        for attr in event.attributes() {
            let attr = match attr {
                Ok(attr) => attr,
                Err(_) => {
                    continue;
                }
            };
            match attr.normalized_value(quick_xml::XmlVersion::default()) {
                Ok(value) => {
                    let (resolved, local) = reader.resolver().resolve_attribute(attr.key);
                    let namespace = match resolved {
                        ResolveResult::Unbound => PackageAttrNamespace::Unbound,
                        ResolveResult::Bound(namespace)
                            if namespace.as_ref() == b"http://www.w3.org/XML/1998/namespace" =>
                        {
                            PackageAttrNamespace::Xml
                        }
                        ResolveResult::Bound(namespace)
                            if namespace.as_ref() == OPF_NS.as_bytes() =>
                        {
                            PackageAttrNamespace::Opf
                        }
                        _ => PackageAttrNamespace::Other,
                    };
                    values.push((namespace, local.as_ref().to_vec(), value.to_string()));
                }
                Err(_) => continue,
            }
        }
        Self { values }
    }

    fn value(&self, key: &[u8]) -> Option<String> {
        self.values
            .iter()
            .find(|(namespace, attr_key, _)| {
                let expected_namespace = if key == LANG.as_bytes() {
                    PackageAttrNamespace::Xml
                } else {
                    PackageAttrNamespace::Unbound
                };
                *namespace == expected_namespace && attr_key_matches(attr_key, key)
            })
            .map(|(_, _, value)| value.clone())
    }

    fn opf_value(&self, key: &[u8]) -> Option<String> {
        self.values
            .iter()
            .find(|(namespace, attr_key, _)| {
                *namespace == PackageAttrNamespace::Opf && attr_key_matches(attr_key, key)
            })
            .map(|(_, _, value)| value.clone())
    }

    fn epub_string(&self, key: &[u8]) -> Option<EpubString> {
        optional_epub_string(self.value(key))
    }
}

fn push_general_ref_preserving_unknown(
    output: &mut String,
    reference: &quick_xml::events::BytesRef<'_>,
) -> Result<()> {
    let _ = push_general_ref(output, reference)?;
    Ok(())
}

fn read_text_content<R: BufRead>(reader: &mut NsReader<R>, end: &[u8]) -> Result<String> {
    let mut buf = Vec::new();
    let mut content = String::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Text(text) => {
                content.push_str(&text_content(&text)?);
            }
            Event::CData(text) => {
                content.push_str(&cdata_content(&text)?);
            }
            Event::GeneralRef(reference) => {
                push_general_ref_preserving_unknown(&mut content, &reference)?;
            }
            Event::End(end_event) if end_event.name().as_ref() == end => {
                break;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(content)
}

fn is_namespaced_element<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    namespace: &[u8],
    name: &[u8],
) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == namespace)
        && local.as_ref() == name
}

fn is_opf_element<R>(reader: &NsReader<R>, event: &BytesStart<'_>, name: &[u8]) -> bool {
    is_namespaced_element(reader, event, OPF_NS.as_bytes(), name)
}

fn is_namespaced_end<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesEnd<'_>,
    namespace: &[u8],
    name: &[u8],
) -> bool {
    let (resolved, local) = reader.resolver().resolve_element(event.name());
    matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == namespace)
        && local.as_ref() == name
}

fn is_opf_end<R>(
    reader: &NsReader<R>,
    event: &quick_xml::events::BytesEnd<'_>,
    name: &[u8],
) -> bool {
    is_namespaced_end(reader, event, OPF_NS.as_bytes(), name)
}

fn is_dc_element_event<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> bool {
    is_namespaced_element(reader, event, DC_NS.as_bytes(), event.local_name().as_ref())
        && is_dc_element(event.local_name().as_ref())
}

pub(crate) fn parse_package<R: BufRead>(input: R) -> Result<Package> {
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let (root_attrs, empty_root) = loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if !is_opf_element(&reader, &event, PACKAGE.as_bytes()) {
                    return Err(PackageError::RootInvalid);
                }
                break (parse_package_attrs(&reader, &event)?, false);
            }
            Event::Empty(event) => {
                if !is_opf_element(&reader, &event, PACKAGE.as_bytes()) {
                    return Err(PackageError::RootInvalid);
                }
                break (parse_package_attrs(&reader, &event)?, true);
            }
            Event::Eof => return Err(PackageError::RootMissing),
            _ => buf.clear(),
        }
    };

    let mut metadata = None;
    let mut manifest = None;
    let mut spine = None;
    let mut guide = None;
    let mut collections = Vec::new();

    if !empty_root {
        loop {
            buf.clear();
            match reader.read_event_into(&mut buf)? {
                Event::Start(event) => {
                    if is_opf_element(&reader, &event, METADATA.as_bytes()) {
                        let block = parse_metadata_block(&mut reader)?;
                        metadata.get_or_insert_with(Metadata::empty).append(block);
                    } else if is_opf_element(&reader, &event, MANIFEST.as_bytes()) {
                        let block = parse_manifest_block(&mut reader, &event)?;
                        manifest
                            .get_or_insert_with(Manifest::new_empty)
                            .append(block);
                    } else if is_opf_element(&reader, &event, SPINE.as_bytes()) {
                        if spine.is_some() {
                            return Err(PackageError::SpineDuplicate);
                        }
                        spine = Some(parse_spine_block(&mut reader, &event)?);
                    } else if is_opf_element(&reader, &event, GUIDE.as_bytes()) {
                        let block = parse_guide_block(&mut reader)?;
                        guide.get_or_insert_with(Guide::empty).append(block);
                    } else if is_opf_element(&reader, &event, COLLECTION.as_bytes()) {
                        collections.push(parse_collection_block(&mut reader, &event)?);
                    } else {
                        let mut skipped = Vec::new();
                        reader.read_to_end_into(event.name(), &mut skipped)?;
                    }
                }
                Event::Empty(event) => {
                    if is_opf_element(&reader, &event, METADATA.as_bytes()) {
                        metadata.get_or_insert_with(Metadata::empty);
                    } else if is_opf_element(&reader, &event, MANIFEST.as_bytes()) {
                        let attrs = PackageEventAttrs::new(&reader, &event);
                        let block =
                            Manifest::from_parsed(attrs.epub_string(ID.as_bytes()), Vec::new());
                        manifest
                            .get_or_insert_with(Manifest::new_empty)
                            .append(block);
                    } else if is_opf_element(&reader, &event, SPINE.as_bytes()) {
                        let block = parse_spine_attrs(&reader, &event);
                        if spine.replace(block).is_some() {
                            return Err(PackageError::SpineDuplicate);
                        }
                    } else if is_opf_element(&reader, &event, GUIDE.as_bytes()) {
                        guide.get_or_insert_with(Guide::empty);
                    } else if is_opf_element(&reader, &event, COLLECTION.as_bytes()) {
                        collections.push(parse_collection_attrs(&reader, &event));
                    }
                }
                Event::End(event) => {
                    let (resolved, local) = reader.resolver().resolve_element(event.name());
                    if matches!(resolved, ResolveResult::Bound(value) if value.as_ref() == OPF_NS.as_bytes())
                        && local.as_ref() == PACKAGE.as_bytes()
                    {
                        break;
                    }
                }
                Event::Eof => return Err(PackageError::RootInvalid),
                _ => {}
            }
        }
    }

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Text(text) => {
                let bytes: &[u8] = text.as_ref();
                if !bytes.iter().all(u8::is_ascii_whitespace) {
                    return Err(PackageError::RootInvalid);
                }
            }
            Event::Comment(_) | Event::PI(_) => {}
            _ => return Err(PackageError::RootInvalid),
        }
    }

    let version = root_attrs
        .version_attr
        .as_deref()
        .and_then(|value| EpubVersion::from_str(value).ok());

    let metadata = metadata.unwrap_or_else(Metadata::empty);
    let manifest = manifest.unwrap_or_else(Manifest::new_empty);
    let spine = spine.unwrap_or_else(Spine::new_empty);

    Ok(Package {
        unique_identifier_id: root_attrs.unique_identifier,
        version,
        xml_lang: root_attrs.xml_lang,
        id: root_attrs.id,
        dir: root_attrs.dir,
        prefix: root_attrs.prefix,
        metadata,
        manifest,
        spine,
        guide,
        collections,
    })
}

struct PackageAttrs {
    id: Option<EpubString>,
    unique_identifier: Option<EpubString>,
    xml_lang: Option<EpubString>,
    dir: Option<TextDirection>,
    prefix: Option<EpubString>,
    version_attr: Option<EpubString>,
}

fn parse_package_attrs<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Result<PackageAttrs> {
    let attrs = PackageEventAttrs::new(reader, event);
    let id = attrs.epub_string(ID.as_bytes());
    let unique_identifier = attrs.epub_string(UNIQUE_IDENTIFIER.as_bytes());
    let xml_lang = attrs.epub_string(LANG.as_bytes());
    let dir = attrs
        .value(DIR.as_bytes())
        .and_then(|value| TextDirection::from_str(&value).ok());
    let prefix = attrs.epub_string(PREFIX.as_bytes());
    let version_attr = attrs.epub_string(VERSION.as_bytes());
    Ok(PackageAttrs {
        id,
        unique_identifier,
        xml_lang,
        dir,
        prefix,
        version_attr,
    })
}

fn parse_metadata_block<R: BufRead>(reader: &mut NsReader<R>) -> Result<Metadata> {
    let mut metadata = Metadata::empty();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                let name = event.local_name().as_ref().to_vec();
                if is_dc_element_event(reader, &event) {
                    let (element, metas) = parse_dc_element(reader, &event, &name)?;
                    push_dc_element(&mut metadata, &name, element);
                    metas.into_iter().for_each(|meta| metadata.add_meta(meta));
                } else if is_opf_element(reader, &event, META.as_bytes()) {
                    parse_meta_element(reader, &event, &mut metadata)?;
                } else if is_opf_element(reader, &event, LINK.as_bytes())
                    && let Some(link) = parse_link_element(reader, &event)?
                {
                    metadata.add_link(link);
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                } else {
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                }
            }
            Event::Empty(event) => {
                let name = event.local_name().as_ref().to_vec();
                if is_dc_element_event(reader, &event) {
                    let (element, metas) = parse_dc_element_empty(reader, &event, &name)?;
                    push_dc_element(&mut metadata, &name, element);
                    metas.into_iter().for_each(|meta| metadata.add_meta(meta));
                } else if is_opf_element(reader, &event, META.as_bytes()) {
                    parse_meta_empty(reader, &event, &mut metadata)?;
                } else if is_opf_element(reader, &event, LINK.as_bytes())
                    && let Some(link) = parse_link_element(reader, &event)?
                {
                    metadata.add_link(link);
                }
            }
            Event::End(end) if is_opf_end(reader, &end, METADATA.as_bytes()) => break,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(metadata)
}

fn parse_manifest_block<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &BytesStart<'_>,
) -> Result<Manifest> {
    let attrs = PackageEventAttrs::new(reader, event);
    let id = attrs.epub_string(ID.as_bytes());
    let mut items = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_opf_element(reader, &event, ITEM.as_bytes()) {
                    let index = items.len();
                    items.push(parse_manifest_item_event(reader, &event, index)?);
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                } else {
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                }
            }
            Event::Empty(event) => {
                if is_opf_element(reader, &event, ITEM.as_bytes()) {
                    let index = items.len();
                    items.push(parse_manifest_item_event(reader, &event, index)?);
                }
            }
            Event::End(end) if is_opf_end(reader, &end, MANIFEST.as_bytes()) => break,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(Manifest::from_parsed(id, items))
}

fn parse_spine_block<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &BytesStart<'_>,
) -> Result<Spine> {
    let mut spine = parse_spine_attrs(reader, event);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_opf_element(reader, &event, ITEMREF.as_bytes()) {
                    let index = spine.itemrefs().len();
                    spine.add_itemref(parse_itemref_event(reader, &event, index)?);
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                } else {
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                }
            }
            Event::Empty(event) => {
                if is_opf_element(reader, &event, ITEMREF.as_bytes()) {
                    let index = spine.itemrefs().len();
                    spine.add_itemref(parse_itemref_event(reader, &event, index)?);
                }
            }
            Event::End(end) if is_opf_end(reader, &end, SPINE.as_bytes()) => break,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(spine)
}

fn parse_spine_attrs<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Spine {
    let attrs = PackageEventAttrs::new(reader, event);
    Spine::from_parsed(
        attrs.epub_string(ID.as_bytes()),
        attrs
            .value(PAGE_PROGRESSION_DIRECTION.as_bytes())
            .and_then(|value| PageProgressionDirection::from_str(&value).ok()),
        attrs.epub_string(TOC.as_bytes()),
    )
}

fn parse_collection_block<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &BytesStart<'_>,
) -> Result<Collection> {
    let mut stack = vec![parse_collection_attrs(reader, event)];
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_opf_element(reader, &event, METADATA.as_bytes()) {
                    let metadata = parse_metadata_block(reader)?;
                    stack
                        .last_mut()
                        .expect("collection stack has a root")
                        .append_metadata(metadata);
                } else if is_opf_element(reader, &event, LINK.as_bytes()) {
                    if let Some(link) = parse_link_element(reader, &event)? {
                        stack
                            .last_mut()
                            .expect("collection stack has a root")
                            .add_link(link);
                    }
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                } else if is_opf_element(reader, &event, COLLECTION.as_bytes()) {
                    if stack.len() == MAX_COLLECTION_NESTING_DEPTH {
                        return Err(PackageError::CollectionNestingLimitExceeded {
                            limit: MAX_COLLECTION_NESTING_DEPTH,
                        });
                    }
                    stack.push(parse_collection_attrs(reader, &event));
                } else {
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                }
            }
            Event::Empty(event) => {
                if is_opf_element(reader, &event, LINK.as_bytes()) {
                    if let Some(link) = parse_link_element(reader, &event)? {
                        stack
                            .last_mut()
                            .expect("collection stack has a root")
                            .add_link(link);
                    }
                } else if is_opf_element(reader, &event, COLLECTION.as_bytes()) {
                    if stack.len() == MAX_COLLECTION_NESTING_DEPTH {
                        return Err(PackageError::CollectionNestingLimitExceeded {
                            limit: MAX_COLLECTION_NESTING_DEPTH,
                        });
                    }
                    let nested = parse_collection_attrs(reader, &event);
                    stack
                        .last_mut()
                        .expect("collection stack has a root")
                        .add_collection(nested)?;
                }
            }
            Event::End(end) if is_opf_end(reader, &end, COLLECTION.as_bytes()) => {
                let collection = stack.pop().expect("collection stack has a root");
                if let Some(parent) = stack.last_mut() {
                    parent.add_collection(collection)?;
                } else {
                    return Ok(collection);
                }
            }
            Event::Eof => {
                while stack.len() > 1 {
                    let collection = stack.pop().expect("collection stack has a root");
                    stack
                        .last_mut()
                        .expect("collection stack has a root")
                        .add_collection(collection)?;
                }
                return Ok(stack.pop().expect("collection stack has a root"));
            }
            _ => {}
        }
        buf.clear();
    }
}

fn parse_collection_attrs<R>(reader: &NsReader<R>, event: &BytesStart<'_>) -> Collection {
    let attrs = PackageEventAttrs::new(reader, event);
    Collection::from_parsed(
        attrs
            .value(DIR.as_bytes())
            .and_then(|value| TextDirection::from_str(&value).ok()),
        attrs.epub_string(ID.as_bytes()),
        attrs.epub_string(LANG.as_bytes()),
        attrs
            .value(ROLE.as_bytes())
            .and_then(|value| CollectionRole::from_str(&value).ok()),
    )
}

fn parse_guide_block<R: BufRead>(reader: &mut NsReader<R>) -> Result<Guide> {
    let mut references = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                if is_opf_element(reader, &event, REFERENCE.as_bytes())
                    && let Some(reference) = parse_reference_event(reader, &event)?
                {
                    references.push(reference);
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                } else {
                    let mut skipped = Vec::new();
                    reader.read_to_end_into(event.name(), &mut skipped)?;
                }
            }
            Event::Empty(event) => {
                if is_opf_element(reader, &event, REFERENCE.as_bytes())
                    && let Some(reference) = parse_reference_event(reader, &event)?
                {
                    references.push(reference);
                }
            }
            Event::End(end) if is_opf_end(reader, &end, GUIDE.as_bytes()) => break,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(Guide::from_parsed(references))
}

fn parse_reference_event<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
) -> Result<Option<Reference>> {
    let attrs = PackageEventAttrs::new(reader, event);
    let title = attrs.epub_string(TITLE.as_bytes());
    let href = attrs.value(HREF.as_bytes()).map(AuthoredHref::new);
    let reference_type = attrs
        .value(TYPE.as_bytes())
        .and_then(|value| ReferenceType::from_str(&value).ok());
    Ok(Some(Reference::from_parsed(reference_type, title, href)))
}

fn parse_manifest_item_event<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    _index: usize,
) -> Result<ManifestItem> {
    let attrs = PackageEventAttrs::new(reader, event);
    let fallback = attrs.epub_string(FALLBACK.as_bytes());
    let href = attrs.value(HREF.as_bytes()).map(AuthoredHref::new);
    let media_type = attrs
        .epub_string(MEDIA_TYPE.as_bytes())
        .map(MediaType::from_epub_string);
    let media_overlay = attrs.epub_string(MEDIA_OVERLAY.as_bytes());
    let id = attrs.epub_string(ID.as_bytes());
    let properties = attrs
        .value(PROPERTIES.as_bytes())
        .map(|value| parse_manifest_properties(&value))
        .unwrap_or_default();
    Ok(ManifestItem::from_parsed(
        fallback,
        href,
        media_type,
        media_overlay,
        id,
        properties,
    ))
}

fn parse_manifest_properties(value: &str) -> Vec<ManifestPropertyToken> {
    value
        .split_whitespace()
        .filter_map(ManifestPropertyToken::new)
        .collect()
}

fn parse_spine_properties(value: &str) -> Vec<SpinePropertyToken> {
    value
        .split_whitespace()
        .filter_map(SpinePropertyToken::new)
        .collect()
}

fn parse_itemref_event<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    _index: usize,
) -> Result<ItemRef> {
    let attrs = PackageEventAttrs::new(reader, event);
    let idref = attrs.epub_string(IDREF.as_bytes());
    let id = attrs.epub_string(ID.as_bytes());
    let linear = attrs
        .value(LINEAR.as_bytes())
        .and_then(|value| Linear::from_str(&value).ok());
    let properties = attrs
        .value(PROPERTIES.as_bytes())
        .map(|value| parse_spine_properties(&value))
        .unwrap_or_default();
    let linear = linear.unwrap_or(Linear::Yes);
    Ok(ItemRef::from_parsed(id, idref, linear, properties))
}

fn parse_link_element<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
) -> Result<Option<MetadataLink>> {
    let attrs = PackageEventAttrs::new(reader, event);
    let href = attrs.value(HREF.as_bytes()).map(AuthoredHref::new);
    let rel = attrs.value(REL.as_bytes()).and_then(LinkRelToken::new);
    if href.is_none() || rel.is_none() {
        return Ok(None);
    }
    let properties = attrs
        .value(PROPERTIES.as_bytes())
        .map(|properties| {
            properties
                .split_whitespace()
                .filter_map(LinkPropertyToken::new)
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(MetadataLink::from_parsed(
        href,
        rel,
        attrs.epub_string(REFINES.as_bytes()),
        attrs.epub_string(MEDIA_TYPE.as_bytes()),
        attrs.epub_string(ID.as_bytes()),
        attrs.epub_string(HREFLANG.as_bytes()),
        properties,
    )))
}

fn parse_dc_element<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &BytesStart<'_>,
    name: &[u8],
) -> Result<(Element, Vec<Meta>)> {
    let content = read_text_content(reader, event.name().as_ref())?;
    let content = EpubString::new(content);
    parse_dc_element_from_parts(reader, event, name, content)
}

fn parse_dc_element_empty<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    name: &[u8],
) -> Result<(Element, Vec<Meta>)> {
    parse_dc_element_from_parts(reader, event, name, None)
}

fn parse_dc_element_from_parts<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    name: &[u8],
    content: Option<EpubString>,
) -> Result<(Element, Vec<Meta>)> {
    let attrs = PackageEventAttrs::new(reader, event);
    let id = attrs.epub_string(ID.as_bytes());
    let dir = attrs
        .value(DIR.as_bytes())
        .and_then(|value| TextDirection::from_str(&value).ok());
    let xml_lang = attrs.epub_string(LANG.as_bytes());
    let opf2_scheme = optional_epub_string(attrs.opf_value(SCHEME.as_bytes()));
    let opf2_role = optional_epub_string(attrs.opf_value(ROLE.as_bytes()));
    let opf2_file_as = optional_epub_string(attrs.opf_value(b"file_as"));
    let element = Element::from_parsed(
        id,
        dir,
        xml_lang,
        content,
        opf2_scheme,
        opf2_role,
        opf2_file_as,
    );
    let _ = name;
    Ok((element, Vec::new()))
}

fn parse_meta_element<R: BufRead>(
    reader: &mut NsReader<R>,
    event: &BytesStart<'_>,
    metadata: &mut Metadata,
) -> Result<()> {
    let attrs = PackageEventAttrs::new(reader, event);
    let property = attrs
        .epub_string(PROPERTY.as_bytes())
        .map(MetaPropertyToken::from_raw);
    let name = attrs.epub_string(NAME.as_bytes());
    if property.is_some() {
        let content = read_text_content(reader, event.name().as_ref())?;
        let content = EpubString::new(content);
        if let Some(meta) = build_meta_from_attrs(&attrs, property, content)? {
            metadata.add_meta(meta);
        }
    } else if let Some(opf2) = build_opf2_meta(&attrs, name) {
        metadata.add_opf2meta(opf2);
        let mut skipped = Vec::new();
        reader.read_to_end_into(event.name(), &mut skipped)?;
    }
    Ok(())
}

fn parse_meta_empty<R>(
    reader: &NsReader<R>,
    event: &BytesStart<'_>,
    metadata: &mut Metadata,
) -> Result<()> {
    let attrs = PackageEventAttrs::new(reader, event);
    let property = attrs
        .epub_string(PROPERTY.as_bytes())
        .map(MetaPropertyToken::from_raw);
    let name = attrs.epub_string(NAME.as_bytes());
    if property.is_some() {
        if let Some(meta) = build_meta_from_attrs(&attrs, property, None)? {
            metadata.add_meta(meta);
        }
    } else if let Some(opf2) = build_opf2_meta(&attrs, name) {
        metadata.add_opf2meta(opf2);
    }
    Ok(())
}

fn build_meta_from_attrs(
    attrs: &PackageEventAttrs,
    property: Option<MetaPropertyToken>,
    content: Option<EpubString>,
) -> Result<Option<Meta>> {
    if property.is_none() {
        return Ok(None);
    }
    let id = attrs.epub_string(ID.as_bytes());
    let refines = attrs.epub_string(REFINES.as_bytes());
    let scheme = attrs.epub_string(SCHEME.as_bytes());
    let xml_lang = attrs.epub_string(LANG.as_bytes());
    let dir = attrs
        .value(DIR.as_bytes())
        .and_then(|value| TextDirection::from_str(&value).ok());
    Ok(Some(Meta::from_parsed(
        dir, id, property, scheme, xml_lang, refines, content,
    )))
}

fn build_opf2_meta(attrs: &PackageEventAttrs, name: Option<EpubString>) -> Option<Opf2Meta> {
    let content = attrs.epub_string(CONTENT.as_bytes());
    if name.is_some() || content.is_some() {
        Some(Opf2Meta::from_parsed(name, content))
    } else {
        None
    }
}

fn push_dc_element(metadata: &mut Metadata, name: &[u8], element: Element) {
    match name {
        name if name == IDENTIFIER.as_bytes() => metadata.add_identifier(element),
        name if name == TITLE.as_bytes() => metadata.add_title(element),
        name if name == LANGUAGE.as_bytes() => metadata.add_language(element),
        name if name == CONTRIBUTOR.as_bytes() => metadata.add_contributor(element),
        name if name == COVERAGE.as_bytes() => metadata.add_coverage(element),
        name if name == CREATOR.as_bytes() => metadata.add_creator(element),
        name if name == DATE.as_bytes() => metadata.add_date(element),
        name if name == DESCRIPTION.as_bytes() => metadata.add_description(element),
        name if name == FORMAT.as_bytes() => metadata.add_format(element),
        name if name == PUBLISHER.as_bytes() => metadata.add_publisher(element),
        name if name == RELATION.as_bytes() => metadata.add_relation(element),
        name if name == RIGHTS.as_bytes() => metadata.add_rights(element),
        name if name == SOURCE.as_bytes() => metadata.add_source(element),
        name if name == SUBJECT.as_bytes() => metadata.add_subject(element),
        name if name == TYPE.as_bytes() => metadata.add_dc_type(element),
        _ => {}
    }
}

fn is_dc_element(name: &[u8]) -> bool {
    [
        IDENTIFIER,
        TITLE,
        LANGUAGE,
        CONTRIBUTOR,
        COVERAGE,
        CREATOR,
        DATE,
        DESCRIPTION,
        FORMAT,
        PUBLISHER,
        RELATION,
        RIGHTS,
        SOURCE,
        SUBJECT,
        TYPE,
    ]
    .iter()
    .any(|value| name == value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_root_is_required_and_namespace_aware() {
        assert!(matches!(Package::parse(""), Err(PackageError::RootMissing)));
        assert!(matches!(
            Package::parse(r#"<not-package xmlns="http://www.idpf.org/2007/opf"/>"#),
            Err(PackageError::RootInvalid)
        ));
        assert!(matches!(
            Package::parse(r#"<package xmlns="https://example.com/not-opf"/>"#),
            Err(PackageError::RootInvalid)
        ));
        assert!(
            Package::parse(
                r#"<opf:package xmlns:opf="http://www.idpf.org/2007/opf" version="3.0"/>"#
            )
            .is_ok()
        );
    }

    #[test]
    fn malformed_package_attrs_are_omitted_while_valid_attrs_are_preserved() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item bad="one" bad="two" id="nav" href="toc.xhtml" media-type="application/xhtml+xml"/>
            </manifest></package>"#,
        )
        .unwrap();

        let item = package.manifest_item_by_id("nav").unwrap();
        assert_eq!(
            item.href().map(|href| href.to_string()).as_deref(),
            Some("toc.xhtml")
        );
        assert_eq!(
            item.media_type().map(MediaType::as_str),
            Some("application/xhtml+xml")
        );
        let serialized = package.to_normalized_xml().unwrap();
        assert!(!serialized.contains("bad="));
        assert!(serialized.contains("href=\"toc.xhtml\""));
    }

    #[test]
    fn missing_package_sections_have_empty_semantics_without_source_synthesis() {
        let package =
            Package::parse(r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"/>"#)
                .unwrap();

        assert!(package.metadata().title().is_empty());
        assert!(package.metadata().identifier().is_empty());
        assert!(package.metadata().meta().is_empty());
        assert!(package.metadata().link().is_empty());
        assert!(package.manifest().items().is_empty());
        assert!(package.spine().itemrefs().is_empty());
        assert!(package.guide().is_none());
    }

    #[test]
    fn repeated_package_blocks_are_combined_in_source_order() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
                <metadata><dc:title>First</dc:title></metadata>
                <manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/></manifest>
                <guide><reference title="First" href="one.xhtml"/></guide>
                <metadata><dc:title>Second</dc:title></metadata>
                <manifest><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/></manifest>
                <guide><reference title="Second" href="two.xhtml"/></guide>
            </package>"#,
        )
        .unwrap();

        assert_eq!(
            package
                .metadata()
                .title()
                .iter()
                .filter_map(Element::content)
                .map(EpubString::as_str)
                .collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
        assert_eq!(
            package
                .manifest()
                .items()
                .iter()
                .filter_map(ManifestItem::id)
                .map(EpubString::as_str)
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert_eq!(package.guide().unwrap().references().len(), 2);
    }

    #[test]
    fn repeated_spine_is_an_error() {
        let error = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><spine/><spine/></package>"#,
        )
        .unwrap_err();
        assert!(matches!(error, PackageError::SpineDuplicate));
    }

    #[test]
    fn text_entities_are_decoded_and_unknown_entities_are_preserved() {
        for (title, expected) in [("A &amp; B", "A & B"), ("A &unknown; B", "A &unknown; B")] {
            let xml = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>{title}</dc:title></metadata></package>"#
            );
            let package = Package::parse(&xml).unwrap();
            assert_eq!(
                package.metadata().title()[0]
                    .content()
                    .map(EpubString::as_str),
                Some(expected)
            );
        }
    }

    #[test]
    fn parsed_required_scalars_are_never_synthesized() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="junk">
                <manifest><item href="chapter.xhtml"/></manifest>
                <spine><itemref linear="sideways"/></spine>
            </package>"#,
        )
        .unwrap();

        assert_eq!(package.version(), None);
        assert_eq!(package.unique_identifier_id(), None);
        let item = &package.manifest().items()[0];
        assert_eq!(item.id(), None);
        assert_eq!(item.media_type(), None);
        let itemref = &package.spine().itemrefs()[0];
        assert_eq!(itemref.idref(), None);
        assert_eq!(itemref.linear(), Linear::Yes);
        let serialized = package.to_normalized_xml().unwrap();
        assert!(!serialized.lines().nth(1).unwrap().contains("version="));
        assert!(!serialized.contains("unique-identifier="));
        assert!(!serialized.contains("application/octet-stream"));
    }

    #[test]
    fn manifest_required_whitespace_attrs_are_missing() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="   " href="   " media-type="   "/>
            </manifest></package>"#,
        )
        .unwrap();
        let item = package.manifest().items().first().unwrap();

        assert_eq!(item.id(), None);
        assert_eq!(item.href(), None);
        assert_eq!(item.authored_href().map(AuthoredHref::as_str), Some("   "));
        assert_eq!(item.media_type(), None);
    }

    #[test]
    fn authored_hrefs_preserve_missing_empty_and_whitespace_states() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata>
                <link href="" rel="record" />
            </metadata><manifest>
                <item id="missing" media-type="application/xhtml+xml"/>
                <item id="empty" href="" media-type="application/xhtml+xml"/>
                <item id="space" href="   " media-type="application/xhtml+xml"/>
            </manifest><guide><reference type="toc" href="   " title="TOC" /></guide></package>"#,
        )
        .unwrap();

        let missing = package.manifest_item_by_id("missing").unwrap();
        let empty = package.manifest_item_by_id("empty").unwrap();
        let space = package.manifest_item_by_id("space").unwrap();
        assert_eq!(missing.authored_href(), None);
        assert_eq!(empty.authored_href().map(AuthoredHref::as_str), Some(""));
        assert_eq!(space.authored_href().map(AuthoredHref::as_str), Some("   "));
        assert_eq!(empty.href(), None);
        assert_eq!(space.href(), None);
        assert_eq!(
            package.metadata().link()[0]
                .authored_href()
                .map(AuthoredHref::as_str),
            Some("")
        );
        assert_eq!(
            package.guide().unwrap().references()[0]
                .authored_href()
                .map(AuthoredHref::as_str),
            Some("   ")
        );

        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        assert_eq!(reparsed.manifest(), package.manifest());
        assert_eq!(reparsed.metadata().link(), package.metadata().link());
        assert_eq!(reparsed.guide(), package.guide());
    }

    #[test]
    fn foreign_structural_names_are_not_opf_or_dc() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:x="https://example.com/ext">
                <x:metadata><x:title>Not metadata</x:title></x:metadata>
                <metadata><x:title>Not DC</x:title></metadata>
                <x:manifest><x:item id="foreign"/></x:manifest>
            </package>"#,
        )
        .unwrap();
        assert!(package.metadata().title().is_empty());
        assert!(package.manifest().items().is_empty());
    }

    #[test]
    fn collections_parse_at_the_nesting_limit() {
        const DEPTH: usize = MAX_COLLECTION_NESTING_DEPTH;
        let mut xml =
            String::from(r#"<package xmlns="http://www.idpf.org/2007/opf"><collection id="root">"#);
        for _ in 1..DEPTH {
            xml.push_str("<collection>");
        }
        for _ in 0..DEPTH {
            xml.push_str("</collection>");
        }
        xml.push_str("</package>");

        let package = Package::parse(&xml).unwrap();
        let mut collection = package.collections().first().unwrap();
        for _ in 1..DEPTH {
            collection = collection.collections().first().unwrap();
        }
        assert!(collection.collections().is_empty());
        assert!(package.to_normalized_xml().is_ok());
    }

    #[test]
    fn collections_over_the_nesting_limit_return_a_focused_error() {
        let depth = MAX_COLLECTION_NESTING_DEPTH + 1;
        let mut xml = String::from(r#"<package xmlns="http://www.idpf.org/2007/opf"><collection>"#);
        for _ in 1..depth {
            xml.push_str("<collection>");
        }
        for _ in 0..depth {
            xml.push_str("</collection>");
        }
        xml.push_str("</package>");

        assert!(matches!(
            Package::parse(&xml),
            Err(PackageError::CollectionNestingLimitExceeded { limit })
                if limit == MAX_COLLECTION_NESTING_DEPTH
        ));
    }

    #[test]
    fn opf2_attributes_use_resolved_namespaces() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:foreign="https://example.com/foreign" xmlns:legacy="http://www.idpf.org/2007/opf" version="2.0">
                <metadata><dc:creator id="creator" xml:lang="fr" foreign:role="bad-role" foreign:file-as="bad-file-as" foreign:scheme="bad-scheme" legacy:role="aut" legacy:file-as="Author, An" legacy:scheme="good-scheme">An Author</dc:creator></metadata>
            </package>"#,
        )
        .unwrap();
        let creator = &package.metadata().creator()[0];

        assert_eq!(creator.id().map(EpubString::as_str), Some("creator"));
        assert_eq!(creator.xml_lang().map(EpubString::as_str), Some("fr"));
        assert_eq!(creator.opf2_role().map(EpubString::as_str), Some("aut"));
        assert_eq!(
            creator.opf2_file_as().map(EpubString::as_str),
            Some("Author, An")
        );
        assert_eq!(
            creator.opf2_scheme().map(EpubString::as_str),
            Some("good-scheme")
        );
    }

    #[test]
    fn opf2_scheme_and_implicit_xsd_scheme_remain_typed_metadata() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf"><metadata>
                <dc:identifier id="bookid">primary</dc:identifier>
                <dc:identifier opf:scheme="isbn">18623871</dc:identifier>
                <meta property="identifier-type" scheme="xsd:string">uuid</meta>
            </metadata></package>"#,
        )
        .unwrap();

        assert_eq!(package.metadata().identifier().len(), 2);
        assert_eq!(
            package.metadata().identifier()[1]
                .opf2_scheme()
                .map(EpubString::as_str),
            Some("isbn")
        );
        assert_eq!(
            package.metadata().meta()[0]
                .scheme()
                .map(EpubString::as_str),
            Some("xsd:string")
        );
    }

    #[test]
    fn manifest_media_types_preserve_typed_parser_state_without_fallback_policy() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="nav" href="nav.xhtml" media-type="APPLICATION/XHTML+XML; charset=utf-8" properties="nav"/>
                <item id="bad" href="bad.bin" media-type="text/plain/html"/>
                <item id="avif" href="cover.avif" media-type="image/avif"/>
                <item id="jxl" href="cover.jxl" media-type="image/jxl"/>
                <item id="ttf" href="font.ttf" media-type="application/x-font-ttf"/>
            </manifest></package>"#,
        )
        .unwrap();

        assert!(
            package
                .manifest_item_by_id("nav")
                .unwrap()
                .media_type()
                .unwrap()
                .is_xhtml()
        );
        assert!(
            !package
                .manifest_item_by_id("bad")
                .unwrap()
                .media_type()
                .unwrap()
                .is_valid()
        );
        assert!(
            package
                .manifest()
                .items()
                .iter()
                .all(|item| item.fallback().is_none())
        );
        assert!(["avif", "jxl", "ttf"].into_iter().all(|id| {
            package
                .manifest_item_by_id(id)
                .unwrap()
                .media_type()
                .is_some_and(MediaType::is_valid)
        }));
    }

    #[test]
    fn rendition_layout_parser_state_preserves_valid_and_invalid_values() {
        for (value, expected) in [
            ("reflowable", Some(RenditionLayout::Reflowable)),
            ("pre-paginated", Some(RenditionLayout::PrePaginated)),
            ("roll", Some(RenditionLayout::Roll)),
            ("sideways", None),
        ] {
            let xml = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata><meta property="rendition:layout">{value}</meta></metadata></package>"#
            );
            let package = Package::parse(&xml).unwrap();
            assert_eq!(package.metadata().rendition_layout(), expected);
            assert_eq!(
                package.metadata().meta()[0]
                    .content()
                    .map(EpubString::as_str),
                Some(value)
            );
        }
    }

    #[test]
    fn spine_page_spread_is_preserved_without_layout_policy() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chap" properties="rendition:page-spread-left"/></spine></package>"#,
        )
        .unwrap();

        assert_eq!(
            package.spine().itemrefs()[0].properties()[0].known_value(),
            Some(super::super::spine::KnownSpineProperty::PageSpreadLeft)
        );
    }
}
