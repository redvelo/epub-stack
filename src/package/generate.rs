use super::collection::Collection;
use super::legacy::{Guide, Opf2Meta};
use super::manifest::{Manifest, ManifestItem, ManifestPropertyToken};
use super::metadata::{Element, LinkPropertyToken, Meta, Metadata, MetadataLink};
use super::spine::{ItemRef, Linear, Spine, SpinePropertyToken};
use super::*;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

impl Package {
    /// Generates a normalized OPF XML projection of the modeled package state.
    ///
    /// Use this to create fresh OPF XML from a [`Package`]. It is not a source-preserving round
    /// trip: unknown XML, comments, original ordering between modeled groups, lexical choices,
    /// and malformed values not retained by the model are omitted or normalized. Use the
    /// publication editing APIs when existing OPF source must be preserved. Collection nesting
    /// is bounded by [`collection::MAX_COLLECTION_NESTING_DEPTH`].
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] if XML writing or final UTF-8 conversion fails.
    pub fn to_normalized_xml(&self) -> Result<String> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 4);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

        let mut package = BytesStart::new(PACKAGE);
        package.push_attribute(("xmlns", OPF_NS));
        package.push_attribute(("xmlns:dc", DC_NS));
        package.push_attribute(("xmlns:opf", OPF_NS));
        if let Some(val) = self.id() {
            package.push_attribute((ID, val.as_str()));
        }
        if let Some(value) = self.unique_identifier_id() {
            package.push_attribute((UNIQUE_IDENTIFIER, value.as_str()));
        }
        if let Some(val) = self.xml_lang() {
            package.push_attribute(("xml:lang", val.as_str()));
        }
        let dir_value = self.dir().map(|dir| dir.to_string());
        if let Some(val) = dir_value.as_deref() {
            package.push_attribute((DIR, val));
        }
        if let Some(val) = self.prefix() {
            package.push_attribute((PREFIX, val.as_str()));
        }
        let version = self.version.map(|version| version.to_string());
        if let Some(version) = version.as_deref() {
            package.push_attribute((VERSION, version));
        }
        writer.write_event(Event::Start(package))?;

        write_metadata(&mut writer, self.metadata())?;
        write_manifest(&mut writer, self.manifest())?;
        write_spine(&mut writer, self.spine())?;
        for collection in self.collections() {
            write_collection(&mut writer, collection)?;
        }
        if let Some(guide) = self.guide() {
            write_guide(&mut writer, guide)?;
        }

        writer.write_event(Event::End(BytesEnd::new(PACKAGE)))?;
        String::from_utf8(writer.into_inner()).map_err(|source| PackageError::Utf8 { source })
    }
}

fn write_metadata(writer: &mut Writer<Vec<u8>>, metadata: &Metadata) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new(METADATA)))?;

    let ordered = [
        ("dc:title", metadata.title()),
        ("dc:language", metadata.language()),
        ("dc:identifier", metadata.identifier()),
        ("dc:creator", metadata.creator()),
        ("dc:contributor", metadata.contributor()),
        ("dc:publisher", metadata.publisher()),
        ("dc:description", metadata.description()),
        ("dc:subject", metadata.subject()),
        ("dc:rights", metadata.rights()),
        ("dc:date", metadata.date()),
        ("dc:format", metadata.format()),
        ("dc:type", metadata.dc_type()),
        ("dc:source", metadata.source()),
        ("dc:relation", metadata.relation()),
        ("dc:coverage", metadata.coverage()),
    ];

    ordered
        .iter()
        .try_for_each(|(tag, elements)| write_dc_elements(writer, tag, elements))?;

    metadata
        .meta()
        .iter()
        .try_for_each(|meta| write_meta(writer, meta))?;
    metadata
        .opf2meta()
        .iter()
        .try_for_each(|meta| write_opf2_meta(writer, meta))?;
    metadata
        .link()
        .iter()
        .try_for_each(|link| write_metadata_link(writer, link))?;

    writer.write_event(Event::End(BytesEnd::new(METADATA)))?;
    Ok(())
}

fn write_dc_elements(writer: &mut Writer<Vec<u8>>, tag: &str, elements: &[Element]) -> Result<()> {
    elements
        .iter()
        .try_for_each(|element| write_dc_element(writer, tag, element))
}

fn write_dc_element(writer: &mut Writer<Vec<u8>>, tag: &str, element: &Element) -> Result<()> {
    let mut node = BytesStart::new(tag);
    if let Some(id) = element.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(lang) = element.xml_lang() {
        node.push_attribute(("xml:lang", lang.as_str()));
    }
    let dir_value = element.dir().map(|dir| dir.to_string());
    if let Some(val) = dir_value.as_deref() {
        node.push_attribute((DIR, val));
    }
    if let Some(scheme) = element.opf2_scheme() {
        node.push_attribute(("opf:scheme", scheme.as_str()));
    }
    if let Some(role) = element.opf2_role() {
        node.push_attribute(("opf:role", role.as_str()));
    }
    if let Some(file_as) = element.opf2_file_as() {
        node.push_attribute(("opf:file-as", file_as.as_str()));
    }
    if let Some(content) = element.content() {
        writer.write_event(Event::Start(node))?;
        writer.write_event(Event::Text(BytesText::new(content.as_str())))?;
        writer.write_event(Event::End(BytesEnd::new(tag)))?;
    } else {
        writer.write_event(Event::Empty(node))?;
    }
    Ok(())
}

fn write_meta(writer: &mut Writer<Vec<u8>>, meta: &Meta) -> Result<()> {
    let mut node = BytesStart::new(META);
    if let Some(id) = meta.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(lang) = meta.xml_lang() {
        node.push_attribute(("xml:lang", lang.as_str()));
    }
    let dir_value = meta.dir().map(|dir| dir.to_string());
    if let Some(val) = dir_value.as_deref() {
        node.push_attribute((DIR, val));
    }
    if let Some(refines) = meta.refines() {
        node.push_attribute((REFINES, refines.as_str()));
    }
    if let Some(property) = meta.property() {
        node.push_attribute((PROPERTY, property.as_str()));
    }
    if let Some(scheme) = meta.scheme() {
        node.push_attribute((SCHEME, scheme.as_str()));
    }
    if let Some(content) = meta.content() {
        writer.write_event(Event::Start(node))?;
        writer.write_event(Event::Text(BytesText::new(content.as_str())))?;
        writer.write_event(Event::End(BytesEnd::new(META)))?;
    } else {
        writer.write_event(Event::Empty(node))?;
    }
    Ok(())
}

fn write_opf2_meta(writer: &mut Writer<Vec<u8>>, meta: &Opf2Meta) -> Result<()> {
    let name = meta.name();
    let content = meta.content();
    if name.is_none() && content.is_none() {
        return Ok(());
    }
    let mut node = BytesStart::new(META);
    if let Some(name) = name {
        node.push_attribute((NAME, name.as_str()));
    }
    if let Some(content) = content {
        node.push_attribute((CONTENT, content.as_str()));
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_metadata_link(writer: &mut Writer<Vec<u8>>, link: &MetadataLink) -> Result<()> {
    let mut node = BytesStart::new(LINK);
    if let Some(id) = link.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(href) = link.authored_href() {
        node.push_attribute((HREF, href.as_str()));
    }
    if let Some(rel) = link.rel() {
        node.push_attribute((REL, rel.as_str()));
    }
    if let Some(hreflang) = link.hreflang() {
        node.push_attribute((HREFLANG, hreflang.as_str()));
    }
    if let Some(refines) = link.refines() {
        node.push_attribute((REFINES, refines.as_str()));
    }
    if let Some(media_type) = link.media_type() {
        node.push_attribute((MEDIA_TYPE, media_type.as_str()));
    }
    if !link.properties().is_empty() {
        let properties = link
            .properties()
            .iter()
            .map(LinkPropertyToken::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        node.push_attribute((PROPERTIES, properties.as_str()));
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_manifest(writer: &mut Writer<Vec<u8>>, manifest: &Manifest) -> Result<()> {
    let mut node = BytesStart::new(MANIFEST);
    if let Some(id) = manifest.id() {
        node.push_attribute((ID, id.as_str()));
    }
    writer.write_event(Event::Start(node))?;
    manifest
        .items()
        .iter()
        .try_for_each(|item| write_manifest_item(writer, item))?;
    writer.write_event(Event::End(BytesEnd::new(MANIFEST)))?;
    Ok(())
}

fn write_manifest_item(writer: &mut Writer<Vec<u8>>, item: &ManifestItem) -> Result<()> {
    let mut node = BytesStart::new(ITEM);
    if let Some(id) = item.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(href) = item.authored_href() {
        node.push_attribute((HREF, href.as_str()));
    }
    if let Some(media_type) = item.media_type() {
        node.push_attribute((MEDIA_TYPE, media_type.as_str()));
    }
    if let Some(fallback) = item.fallback() {
        node.push_attribute((FALLBACK, fallback.as_str()));
    }
    if let Some(overlay) = item.media_overlay() {
        node.push_attribute((MEDIA_OVERLAY, overlay.as_str()));
    }
    if !item.properties().is_empty() {
        let properties = item
            .properties()
            .iter()
            .map(ManifestPropertyToken::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        node.push_attribute((PROPERTIES, properties.as_str()));
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_spine(writer: &mut Writer<Vec<u8>>, spine: &Spine) -> Result<()> {
    let mut node = BytesStart::new(SPINE);
    if let Some(id) = spine.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(toc) = spine.toc() {
        node.push_attribute((TOC, toc.as_str()));
    }
    let dir_value = spine
        .page_progression_direction()
        .map(|direction| direction.to_string());
    if let Some(val) = dir_value.as_deref() {
        node.push_attribute((PAGE_PROGRESSION_DIRECTION, val));
    }
    writer.write_event(Event::Start(node))?;
    spine
        .itemrefs()
        .iter()
        .try_for_each(|itemref| write_itemref(writer, itemref))?;
    writer.write_event(Event::End(BytesEnd::new(SPINE)))?;
    Ok(())
}

fn write_itemref(writer: &mut Writer<Vec<u8>>, itemref: &ItemRef) -> Result<()> {
    let mut node = BytesStart::new(ITEMREF);
    if let Some(id) = itemref.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(idref) = itemref.idref() {
        node.push_attribute((IDREF, idref.as_str()));
    }
    if itemref.linear() == Linear::No {
        let linear = itemref.linear().to_string();
        node.push_attribute((LINEAR, linear.as_str()));
    }
    if !itemref.properties().is_empty() {
        let properties = itemref
            .properties()
            .iter()
            .map(SpinePropertyToken::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        node.push_attribute((PROPERTIES, properties.as_str()));
    }
    writer.write_event(Event::Empty(node))?;
    Ok(())
}

fn write_collection(writer: &mut Writer<Vec<u8>>, collection: &Collection) -> Result<()> {
    let mut node = BytesStart::new(COLLECTION);
    if let Some(id) = collection.id() {
        node.push_attribute((ID, id.as_str()));
    }
    if let Some(role) = collection.role() {
        let role = role.to_string();
        node.push_attribute((ROLE, role.as_str()));
    }
    if let Some(lang) = collection.xml_lang() {
        node.push_attribute(("xml:lang", lang.as_str()));
    }
    let dir_value = collection.dir().map(|dir| dir.to_string());
    if let Some(val) = dir_value.as_deref() {
        node.push_attribute((DIR, val));
    }
    writer.write_event(Event::Start(node))?;

    if let Some(metadata) = collection.metadata() {
        write_metadata(writer, metadata)?;
    }
    collection
        .link()
        .iter()
        .try_for_each(|link| write_metadata_link(writer, link))?;
    for nested in collection.collections() {
        write_collection(writer, nested)?;
    }

    writer.write_event(Event::End(BytesEnd::new(COLLECTION)))?;
    Ok(())
}

fn write_guide(writer: &mut Writer<Vec<u8>>, guide: &Guide) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new(GUIDE)))?;
    for reference in guide.references() {
        let mut node = BytesStart::new(REFERENCE);
        if let Some(reference_type) = reference.reference_type() {
            let reference_type = reference_type.to_string();
            node.push_attribute((TYPE, reference_type.as_str()));
        }
        if let Some(title) = reference.title() {
            node.push_attribute((TITLE, title.as_str()));
        }
        if let Some(href) = reference.authored_href() {
            node.push_attribute((HREF, href.as_str()));
        }
        writer.write_event(Event::Empty(node))?;
    }
    writer.write_event(Event::End(BytesEnd::new(GUIDE)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::manifest::{KnownManifestProperty, ManifestPropertyToken};
    use crate::package::metadata::{
        KnownLinkProperty, KnownMetaProperty, LinkPropertyToken, LinkRelToken, MetaPropertyToken,
    };
    use crate::package::spine::{KnownSpineProperty, SpinePropertyToken};
    const EXAMPLE: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" unique-identifier="bookid" xml:lang="en">
    <metadata><dc:title>Alice</dc:title><dc:identifier id="bookid">test_epubcfi</dc:identifier><dc:language>en</dc:language></metadata>
    <manifest><item id="nav" properties="nav" href="toc.xhtml" media-type="application/xhtml+xml"/></manifest>
    <spine><itemref idref="nav"/></spine>
</package>"#;

    #[test]
    fn package_normalized_xml_round_trips() {
        let package = Package::parse(EXAMPLE).unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        assert_eq!(reparsed, package);
    }

    #[test]
    fn serialization_omits_default_linear_and_keeps_non_default() {
        let mut package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chap"/></spine></package>"#,
        )
        .unwrap();
        package
            .replace_spine_itemref_at(0, ItemRef::new("chap").unwrap())
            .unwrap();

        let xml = package.to_normalized_xml().unwrap();
        assert!(xml.contains("<itemref idref=\"chap\"/>"));
        assert!(!xml.contains("linear=\"yes\""));

        package
            .replace_spine_itemref_at(0, ItemRef::new("chap").unwrap().with_linear(Linear::No))
            .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        assert_eq!(reparsed.spine().itemrefs()[0].linear(), Linear::No);
    }

    #[test]
    fn serialization_uses_media_overlay_attribute_name() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay"/>
                <item id="overlay" href="overlay.smil" media-type="application/smil+xml"/>
            </manifest></package>"#,
        )
        .unwrap();

        let xml = package.to_normalized_xml().unwrap();
        assert!(xml.contains("media-type=\"application/xhtml+xml\""));
        assert!(xml.contains("media-overlay=\"overlay\""));
        assert!(!xml.contains("media_type="));
        assert!(!xml.contains("media_overlay="));
        assert_eq!(Package::parse(&xml).unwrap(), package);
    }

    #[test]
    fn manifest_property_tokens_serialize_and_reparse_in_order() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="nav" properties="nav custom:foo scripted mystery" href="toc.xhtml" media-type="application/xhtml+xml"/>
            </manifest></package>"#,
        )
        .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        let item = reparsed.manifest_item_by_id("nav").unwrap();

        assert!(item.has_property(KnownManifestProperty::Nav));
        assert!(item.has_property(KnownManifestProperty::Scripted));
        assert_eq!(
            item.properties()
                .iter()
                .map(ManifestPropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["nav", "custom:foo", "scripted", "mystery"]
        );
        assert_eq!(item.properties()[1].known_value(), None);
        assert_eq!(item.properties()[3].known_value(), None);
    }

    #[test]
    fn spine_property_tokens_serialize_and_reparse_in_order() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine>
                <itemref idref="chap" properties="rendition:page-spread-center custom:foo"/>
            </spine></package>"#,
        )
        .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        let itemref = &reparsed.spine().itemrefs()[0];

        assert!(itemref.has_property(KnownSpineProperty::PageSpreadCenter));
        assert_eq!(
            itemref
                .properties()
                .iter()
                .map(SpinePropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["rendition:page-spread-center", "custom:foo"]
        );
    }

    #[test]
    fn metadata_property_tokens_serialize_and_reparse_raw_values() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata>
                <meta property="DCTERMS:MODIFIED">2026-01-01T00:00:00Z</meta>
                <meta property="custom:layout-note">kept</meta>
            </metadata></package>"#,
        )
        .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        let properties = reparsed
            .metadata()
            .meta()
            .iter()
            .filter_map(Meta::property)
            .collect::<Vec<_>>();

        assert_eq!(properties[0].as_str(), "DCTERMS:MODIFIED");
        assert_eq!(
            properties[0].known_value(),
            Some(KnownMetaProperty::Dctermsmodified)
        );
        assert_eq!(properties[1].as_str(), "custom:layout-note");
        assert_eq!(properties[1].known_value(), None);
    }

    #[test]
    fn metadata_link_tokens_serialize_and_reparse_as_authored() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata>
                <link href="meta.json" rel="custom-record" properties="onix custom:foo"/>
                <link href="marc.xml" rel="marc21xml-record"/>
                <link href="mods.xml" rel="mods-record"/>
                <link href="onix.xml" rel="onix-record"/>
                <link href="signature.xml" rel="xml-signature"/>
                <link href="xmp.xml" rel="xmp-record"/>
                <link href="record.xml" rel="record"/>
            </metadata></package>"#,
        )
        .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();
        let links = reparsed.metadata().link();

        assert_eq!(
            links[0].rel().map(LinkRelToken::as_str),
            Some("custom-record")
        );
        assert_eq!(links[0].rel().and_then(LinkRelToken::known_value), None);
        assert_eq!(
            links[0]
                .properties()
                .iter()
                .map(LinkPropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["onix", "custom:foo"]
        );
        assert_eq!(
            links[0]
                .properties()
                .iter()
                .map(LinkPropertyToken::known_value)
                .collect::<Vec<_>>(),
            vec![Some(KnownLinkProperty::Onix), None]
        );
        assert_eq!(
            links[1..]
                .iter()
                .filter_map(MetadataLink::rel)
                .map(LinkRelToken::as_str)
                .collect::<Vec<_>>(),
            vec![
                "marc21xml-record",
                "mods-record",
                "onix-record",
                "xml-signature",
                "xmp-record",
                "record"
            ]
        );
    }

    #[test]
    fn rendition_layout_metadata_serializes_without_change() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata>
                <meta property="rendition:layout">roll</meta>
                <meta property="custom:layout-note">kept</meta>
            </metadata></package>"#,
        )
        .unwrap();
        let reparsed = Package::parse(&package.to_normalized_xml().unwrap()).unwrap();

        assert_eq!(
            reparsed.metadata().rendition_layout(),
            Some(RenditionLayout::Roll)
        );
        assert!(reparsed.metadata().meta().iter().any(|meta| {
            meta.property()
                .is_some_and(|property| property.as_str() == "custom:layout-note")
                && meta.content().is_some_and(|content| content == "kept")
        }));
    }

    #[test]
    fn unmodeled_content_is_absent_from_normalized_projection() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:x="https://example.com/ext">
                <bindings><mediaType handler="h"/></bindings>
                <metadata><link href="missing-rel"/><meta property="missing-content"/><x:unknown/></metadata>
                <x:unknown/>
            </package>"#,
        )
        .unwrap();
        let normalized = package.to_normalized_xml().unwrap();
        let reparsed = Package::parse(&normalized).unwrap();

        assert!(!normalized.contains("bindings"));
        assert!(!normalized.contains("x:unknown"));
        assert!(reparsed.metadata().link().is_empty());
        assert_eq!(reparsed.metadata().meta().len(), 1);
        assert_eq!(
            reparsed.metadata().meta()[0]
                .property()
                .map(MetaPropertyToken::as_str),
            Some("missing-content")
        );
        assert!(reparsed.metadata().meta()[0].content().is_none());
    }
}
