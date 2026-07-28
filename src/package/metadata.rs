//! Inspect and edit Dublin Core and OPF metadata.
//!
//! [`Metadata`] groups supported Dublin Core elements, EPUB 3 [`Meta`] and
//! [`MetadataLink`](crate::package::metadata::MetadataLink) nodes, and EPUB 2 metadata. Use
//! [`MetadataElement`](crate::package::metadata::MetadataElement) when code needs to handle
//! Dublin Core kinds uniformly. Property and relationship token types retain their authored
//! spelling after [`EpubString`] trims surrounding Unicode whitespace, while exposing recognized
//! vocabulary values separately.

use super::legacy::Opf2Meta;
use super::{
    CONTRIBUTOR, COVERAGE, CREATOR, DATE, DESCRIPTION, FORMAT, IDENTIFIER, LANGUAGE, LINK, META,
    PUBLISHER, PackageError, RELATION, RIGHTS, RenditionFlow, RenditionLayout,
    RenditionOrientation, RenditionSpread, RenditionViewport, Result, SOURCE, SUBJECT, TITLE, TYPE,
    required_package_string,
};
use crate::resource::{AuthoredHref, EpubHref};
use crate::semantics::TextDirection;
use crate::string::{EpubString, EpubStringEmpty};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned supported Dublin Core element tagged with its OPF local name.
pub enum MetadataElement {
    /// A `dc:identifier` element.
    Identifier(Element),
    /// A `dc:title` element.
    Title(Element),
    /// A `dc:language` element.
    Language(Element),
    /// A `dc:contributor` element.
    Contributor(Element),
    /// A `dc:coverage` element.
    Coverage(Element),
    /// A `dc:creator` element.
    Creator(Element),
    /// A `dc:date` element.
    Date(Element),
    /// A `dc:description` element.
    Description(Element),
    /// A `dc:format` element.
    Format(Element),
    /// A `dc:publisher` element.
    Publisher(Element),
    /// A `dc:relation` element.
    Relation(Element),
    /// A `dc:rights` element.
    Rights(Element),
    /// A `dc:source` element.
    Source(Element),
    /// A `dc:subject` element.
    Subject(Element),
    /// A `dc:type` element.
    Type(Element),
}

impl MetadataElement {
    /// Returns the static Dublin Core local name associated with this variant.
    pub fn local_name(&self) -> &'static str {
        match self {
            Self::Identifier(_) => IDENTIFIER,
            Self::Title(_) => TITLE,
            Self::Language(_) => LANGUAGE,
            Self::Contributor(_) => CONTRIBUTOR,
            Self::Coverage(_) => COVERAGE,
            Self::Creator(_) => CREATOR,
            Self::Date(_) => DATE,
            Self::Description(_) => DESCRIPTION,
            Self::Format(_) => FORMAT,
            Self::Publisher(_) => PUBLISHER,
            Self::Relation(_) => RELATION,
            Self::Rights(_) => RIGHTS,
            Self::Source(_) => SOURCE,
            Self::Subject(_) => SUBJECT,
            Self::Type(_) => TYPE,
        }
    }

    /// Borrows the element payload regardless of its local-name variant.
    pub fn element(&self) -> &Element {
        match self {
            Self::Identifier(element)
            | Self::Title(element)
            | Self::Language(element)
            | Self::Contributor(element)
            | Self::Coverage(element)
            | Self::Creator(element)
            | Self::Date(element)
            | Self::Description(element)
            | Self::Format(element)
            | Self::Publisher(element)
            | Self::Relation(element)
            | Self::Rights(element)
            | Self::Source(element)
            | Self::Subject(element)
            | Self::Type(element) => element,
        }
    }

    /// Wraps `element` in the variant for a supported Dublin Core local name.
    ///
    /// Unknown names return `None`, dropping the supplied `element`. Check the local name first
    /// if the caller needs to keep the element on failure. Matching is case-sensitive and does
    /// not inspect namespaces.
    pub fn from_local_name(local_name: &str, element: Element) -> Option<Self> {
        match local_name {
            IDENTIFIER => Some(Self::Identifier(element)),
            TITLE => Some(Self::Title(element)),
            LANGUAGE => Some(Self::Language(element)),
            CONTRIBUTOR => Some(Self::Contributor(element)),
            COVERAGE => Some(Self::Coverage(element)),
            CREATOR => Some(Self::Creator(element)),
            DATE => Some(Self::Date(element)),
            DESCRIPTION => Some(Self::Description(element)),
            FORMAT => Some(Self::Format(element)),
            PUBLISHER => Some(Self::Publisher(element)),
            RELATION => Some(Self::Relation(element)),
            RIGHTS => Some(Self::Rights(element)),
            SOURCE => Some(Self::Source(element)),
            SUBJECT => Some(Self::Subject(element)),
            TYPE => Some(Self::Type(element)),
            _ => None,
        }
    }
}

macro_rules! vec_getter {
    ($name:ident, $ty:ty) => {
        #[doc = concat!("The modeled `", stringify!($name), "` nodes in source order.")]
        pub fn $name(&self) -> &[$ty] {
            self.$name.as_slice()
        }
    };
}

macro_rules! vec_adder {
    ($field:ident, $method:ident, $ty:ty) => {
        #[doc = concat!("Appends a node to the `", stringify!($field), "` collection.")]
        pub fn $method(&mut self, value: $ty) {
            self.$field.push(value);
        }
    };
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Owned modeled contents of an OPF `metadata` block.
///
/// Each node kind preserves its own source order, but interleaving between kinds, unknown XML,
/// comments, and lexical formatting are not retained. Authored href and vocabulary token
/// spellings remain available through their dedicated raw accessors.
pub struct Metadata {
    identifier: Vec<Element>,
    title: Vec<Element>,
    language: Vec<Element>,
    contributor: Vec<Element>,
    coverage: Vec<Element>,
    creator: Vec<Element>,
    date: Vec<Element>,
    description: Vec<Element>,
    format: Vec<Element>,
    publisher: Vec<Element>,
    relation: Vec<Element>,
    rights: Vec<Element>,
    source: Vec<Element>,
    subject: Vec<Element>,
    dc_type: Vec<Element>,
    meta: Vec<Meta>,
    opf2meta: Vec<Opf2Meta>,
    link: Vec<MetadataLink>,
}

/// An authored package metadata node carrying an `id`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MetadataIdTarget<'a> {
    /// A Dublin Core element and its static local name.
    Element {
        /// The Dublin Core local name.
        local_name: &'static str,
        /// The borrowed element carrying the ID.
        element: &'a Element,
    },
    /// An OPF 3 `meta` element.
    Meta(&'a Meta),
    /// An OPF metadata `link` element.
    Link(&'a MetadataLink),
}

impl MetadataIdTarget<'_> {
    /// Borrows the authored ID from the target.
    pub fn id(&self) -> Option<&EpubString> {
        match self {
            Self::Element { element, .. } => element.id(),
            Self::Meta(meta) => meta.id(),
            Self::Link(link) => link.id(),
        }
    }
}

/// Resolution of an authored ID relationship in package metadata.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MetadataIdLookup<'a> {
    /// No modeled node has the requested authored ID.
    Missing,
    /// Exactly one modeled node has the requested authored ID.
    Unique(MetadataIdTarget<'a>),
    /// More than one modeled node has the requested authored ID.
    Ambiguous,
}

impl Metadata {
    vec_getter!(identifier, Element);
    vec_getter!(title, Element);
    vec_getter!(language, Element);
    vec_getter!(contributor, Element);
    vec_getter!(coverage, Element);
    vec_getter!(creator, Element);
    vec_getter!(date, Element);
    vec_getter!(description, Element);
    vec_getter!(format, Element);
    vec_getter!(publisher, Element);
    vec_getter!(relation, Element);
    vec_getter!(rights, Element);
    vec_getter!(source, Element);
    vec_getter!(subject, Element);
    vec_getter!(dc_type, Element);
    vec_getter!(meta, Meta);
    vec_getter!(opf2meta, Opf2Meta);
    vec_getter!(link, MetadataLink);

    vec_adder!(identifier, add_identifier, Element);
    vec_adder!(title, add_title, Element);
    vec_adder!(language, add_language, Element);
    vec_adder!(contributor, add_contributor, Element);
    vec_adder!(coverage, add_coverage, Element);
    vec_adder!(creator, add_creator, Element);
    vec_adder!(date, add_date, Element);
    vec_adder!(description, add_description, Element);
    vec_adder!(format, add_format, Element);
    vec_adder!(publisher, add_publisher, Element);
    vec_adder!(relation, add_relation, Element);
    vec_adder!(rights, add_rights, Element);
    vec_adder!(source, add_source, Element);
    vec_adder!(subject, add_subject, Element);
    vec_adder!(dc_type, add_dc_type, Element);
    vec_adder!(meta, add_meta, Meta);
    vec_adder!(opf2meta, add_opf2meta, Opf2Meta);
    vec_adder!(link, add_link, MetadataLink);

    /// Iterates over all modeled nodes whose authored ID exactly equals `id`.
    ///
    /// Results include Dublin Core, `meta`, and `link` nodes in grouped model order.
    pub fn authored_id_targets(
        &self,
        id: impl AsRef<str>,
    ) -> impl Iterator<Item = MetadataIdTarget<'_>> + '_ {
        let id = id.as_ref().to_owned();
        let elements = [
            (IDENTIFIER, self.identifier()),
            (TITLE, self.title()),
            (LANGUAGE, self.language()),
            (CONTRIBUTOR, self.contributor()),
            (COVERAGE, self.coverage()),
            (CREATOR, self.creator()),
            (DATE, self.date()),
            (DESCRIPTION, self.description()),
            (FORMAT, self.format()),
            (PUBLISHER, self.publisher()),
            (RELATION, self.relation()),
            (RIGHTS, self.rights()),
            (SOURCE, self.source()),
            (SUBJECT, self.subject()),
            (TYPE, self.dc_type()),
        ]
        .into_iter()
        .flat_map(|(local_name, elements)| {
            elements
                .iter()
                .map(move |element| MetadataIdTarget::Element {
                    local_name,
                    element,
                })
        });
        elements
            .chain(self.meta().iter().map(MetadataIdTarget::Meta))
            .chain(self.link().iter().map(MetadataIdTarget::Link))
            .filter(move |target| {
                target
                    .id()
                    .is_some_and(|target_id| target_id.as_str() == id)
            })
    }

    /// Resolves an authored ID while preserving missing and ambiguous outcomes.
    ///
    /// Matching is exact; duplicate authored IDs produce [`MetadataIdLookup::Ambiguous`].
    pub fn authored_id(&self, id: impl AsRef<str>) -> MetadataIdLookup<'_> {
        let mut matches = self.authored_id_targets(id);
        let Some(target) = matches.next() else {
            return MetadataIdLookup::Missing;
        };
        if matches.next().is_some() {
            MetadataIdLookup::Ambiguous
        } else {
            MetadataIdLookup::Unique(target)
        }
    }

    /// Resolves a `refines` value after removing all leading `#` characters.
    ///
    /// No other URI normalization is performed.
    pub fn refinement_target(&self, refines: impl AsRef<str>) -> MetadataIdLookup<'_> {
        self.authored_id(refines.as_ref().trim_start_matches('#'))
    }

    pub(super) fn append(&mut self, mut other: Self) {
        self.identifier.append(&mut other.identifier);
        self.title.append(&mut other.title);
        self.language.append(&mut other.language);
        self.contributor.append(&mut other.contributor);
        self.coverage.append(&mut other.coverage);
        self.creator.append(&mut other.creator);
        self.date.append(&mut other.date);
        self.description.append(&mut other.description);
        self.format.append(&mut other.format);
        self.publisher.append(&mut other.publisher);
        self.relation.append(&mut other.relation);
        self.rights.append(&mut other.rights);
        self.source.append(&mut other.source);
        self.subject.append(&mut other.subject);
        self.dc_type.append(&mut other.dc_type);
        self.meta.append(&mut other.meta);
        self.opf2meta.append(&mut other.opf2meta);
        self.link.append(&mut other.link);
    }

    /// Returns the first unrefined recognized `rendition:layout` value.
    ///
    /// Property matching and value parsing are ASCII case-insensitive; the stored meta token and
    /// content remain unchanged.
    pub fn rendition_layout(&self) -> Option<RenditionLayout> {
        self.meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(Meta::rendition_layout)
    }

    /// Returns the first unrefined recognized historical `rendition:flow` value.
    ///
    /// No default or runtime flow policy is inferred.
    pub fn rendition_flow(&self) -> Option<RenditionFlow> {
        self.meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(Meta::rendition_flow)
    }

    /// Returns the first unrefined recognized historical `rendition:orientation` value.
    ///
    /// No default or runtime orientation policy is inferred.
    pub fn rendition_orientation(&self) -> Option<RenditionOrientation> {
        self.meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(Meta::rendition_orientation)
    }

    /// Returns the first unrefined recognized historical `rendition:spread` value.
    ///
    /// No default or runtime spread policy is inferred.
    pub fn rendition_spread(&self) -> Option<RenditionSpread> {
        self.meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(Meta::rendition_spread)
    }

    /// Returns the first unrefined recognized deprecated `rendition:viewport` value.
    pub fn rendition_viewport(&self) -> Option<RenditionViewport> {
        self.meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(Meta::rendition_viewport)
    }

    /// Borrows the first EPUB 2 cover manifest ID from modeled `meta` pairs.
    ///
    /// Name matching is ASCII case-insensitive.
    pub fn opf2_cover_id(&self) -> Option<&str> {
        self.opf2meta.iter().find_map(|meta| {
            meta.name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cover"))
                .then(|| meta.content())
                .flatten()
        })
    }

    /// Appends an owned Dublin Core element to its variant-specific collection.
    pub fn add_element(&mut self, element: MetadataElement) {
        match element {
            MetadataElement::Identifier(element) => self.add_identifier(element),
            MetadataElement::Title(element) => self.add_title(element),
            MetadataElement::Language(element) => self.add_language(element),
            MetadataElement::Contributor(element) => self.add_contributor(element),
            MetadataElement::Coverage(element) => self.add_coverage(element),
            MetadataElement::Creator(element) => self.add_creator(element),
            MetadataElement::Date(element) => self.add_date(element),
            MetadataElement::Description(element) => self.add_description(element),
            MetadataElement::Format(element) => self.add_format(element),
            MetadataElement::Publisher(element) => self.add_publisher(element),
            MetadataElement::Relation(element) => self.add_relation(element),
            MetadataElement::Rights(element) => self.add_rights(element),
            MetadataElement::Source(element) => self.add_source(element),
            MetadataElement::Subject(element) => self.add_subject(element),
            MetadataElement::Type(element) => self.add_dc_type(element),
        }
    }

    /// Removes a Dublin Core element by local name and kind-relative index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] for an unsupported local name or
    /// out-of-range index. Failure is atomic; removal shifts later same-kind elements.
    pub fn remove_element_at(&mut self, local_name: &str, index: usize) -> Result<()> {
        let elements =
            self.elements_mut(local_name)
                .ok_or_else(|| PackageError::MetadataIndexMissing {
                    kind: local_name.to_string(),
                    index,
                })?;
        if index >= elements.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: local_name.to_string(),
                index,
            });
        }
        elements.remove(index);
        Ok(())
    }

    /// Replaces a Dublin Core element in place by local name and kind-relative index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataKindMismatch`] when the variant differs, or
    /// [`PackageError::MetadataIndexMissing`] for an unsupported name or index. Failure is
    /// atomic; the replacement is cloned from the consumed wrapper.
    pub fn replace_element_at(
        &mut self,
        local_name: &str,
        index: usize,
        element: MetadataElement,
    ) -> Result<()> {
        if element.local_name() != local_name {
            return Err(PackageError::MetadataKindMismatch {
                target: local_name.to_string(),
                replacement: element.local_name().to_string(),
            });
        }
        let elements =
            self.elements_mut(local_name)
                .ok_or_else(|| PackageError::MetadataIndexMissing {
                    kind: local_name.to_string(),
                    index,
                })?;
        if index >= elements.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: local_name.to_string(),
                index,
            });
        }
        elements[index] = element.element().clone();
        Ok(())
    }

    /// Removes a `meta` node by zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub fn remove_meta_at(&mut self, index: usize) -> Result<()> {
        if index >= self.meta.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: META.to_string(),
                index,
            });
        }
        self.meta.remove(index);
        Ok(())
    }

    /// Replaces a `meta` node in place.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub fn replace_meta_at(&mut self, index: usize, meta: Meta) -> Result<()> {
        if index >= self.meta.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: META.to_string(),
                index,
            });
        }
        self.meta[index] = meta;
        Ok(())
    }

    /// Removes a metadata link by zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub fn remove_link_at(&mut self, index: usize) -> Result<()> {
        if index >= self.link.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: LINK.to_string(),
                index,
            });
        }
        self.link.remove(index);
        Ok(())
    }

    /// Replaces a metadata link in place.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub fn replace_link_at(&mut self, index: usize, link: MetadataLink) -> Result<()> {
        if index >= self.link.len() {
            return Err(PackageError::MetadataIndexMissing {
                kind: LINK.to_string(),
                index,
            });
        }
        self.link[index] = link;
        Ok(())
    }

    fn elements_mut(&mut self, local_name: &str) -> Option<&mut Vec<Element>> {
        match local_name {
            IDENTIFIER => Some(&mut self.identifier),
            TITLE => Some(&mut self.title),
            LANGUAGE => Some(&mut self.language),
            CONTRIBUTOR => Some(&mut self.contributor),
            COVERAGE => Some(&mut self.coverage),
            CREATOR => Some(&mut self.creator),
            DATE => Some(&mut self.date),
            DESCRIPTION => Some(&mut self.description),
            FORMAT => Some(&mut self.format),
            PUBLISHER => Some(&mut self.publisher),
            RELATION => Some(&mut self.relation),
            RIGHTS => Some(&mut self.rights),
            SOURCE => Some(&mut self.source),
            SUBJECT => Some(&mut self.subject),
            TYPE => Some(&mut self.dc_type),
            _ => None,
        }
    }

    /// Creates owned metadata containing one title, identifier, and language.
    ///
    /// Leading and trailing Unicode whitespace is removed from every input. The remaining text
    /// is stored without identifier or language-tag normalization.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::EmptyField`] if any required input is empty or whitespace-only.
    pub fn new_minimal(
        title: impl AsRef<str>,
        identifier: impl AsRef<str>,
        language: impl AsRef<str>,
    ) -> Result<Self> {
        let title = Element::new(required_package_string(title, "title")?);
        let identifier = Element::new(required_package_string(identifier, "identifier")?);
        let language = Element::new(required_package_string(language, "language")?);
        Ok(Self {
            identifier: vec![identifier],
            title: vec![title],
            language: vec![language],
            contributor: Vec::new(),
            coverage: Vec::new(),
            creator: Vec::new(),
            date: Vec::new(),
            description: Vec::new(),
            format: Vec::new(),
            publisher: Vec::new(),
            relation: Vec::new(),
            rights: Vec::new(),
            source: Vec::new(),
            subject: Vec::new(),
            dc_type: Vec::new(),
            meta: Vec::new(),
            opf2meta: Vec::new(),
            link: Vec::new(),
        })
    }

    pub(crate) fn empty() -> Self {
        Self {
            identifier: Vec::new(),
            title: Vec::new(),
            language: Vec::new(),
            contributor: Vec::new(),
            coverage: Vec::new(),
            creator: Vec::new(),
            date: Vec::new(),
            description: Vec::new(),
            format: Vec::new(),
            publisher: Vec::new(),
            relation: Vec::new(),
            rights: Vec::new(),
            source: Vec::new(),
            subject: Vec::new(),
            dc_type: Vec::new(),
            meta: Vec::new(),
            opf2meta: Vec::new(),
            link: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash, bon::Builder)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned Dublin Core metadata element shared by supported DC local names.
///
/// Parsed instances may have no content and preserve modeled EPUB 2 OPF attributes. Unknown
/// attributes, child markup, and source formatting are not retained.
pub struct Element {
    id: Option<EpubString>,
    dir: Option<TextDirection>,
    xml_lang: Option<EpubString>,
    #[builder(required, with = Some)]
    content: Option<EpubString>,
    opf2_scheme: Option<EpubString>,
    opf2_role: Option<EpubString>,
    opf2_file_as: Option<EpubString>,
}

impl Element {
    pub(super) fn from_parsed(
        id: Option<EpubString>,
        dir: Option<TextDirection>,
        xml_lang: Option<EpubString>,
        content: Option<EpubString>,
        opf2_scheme: Option<EpubString>,
        opf2_role: Option<EpubString>,
        opf2_file_as: Option<EpubString>,
    ) -> Self {
        Self {
            id,
            dir,
            xml_lang,
            content,
            opf2_scheme,
            opf2_role,
            opf2_file_as,
        }
    }

    /// Creates an element with owned non-empty text content and no optional attributes.
    pub fn new(content: EpubString) -> Self {
        Self {
            id: None,
            dir: None,
            xml_lang: None,
            content: Some(content),
            opf2_scheme: None,
            opf2_role: None,
            opf2_file_as: None,
        }
    }

    /// Borrows the optional authored ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Borrows `xml:lang` without language-tag normalization.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }
    /// Borrows flattened text content.
    pub fn content(&self) -> Option<&EpubString> {
        self.content.as_ref()
    }
    /// Returns the recognized text direction; invalid parsed tokens are omitted.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }
    /// Borrows the EPUB 2 OPF `scheme` attribute.
    pub fn opf2_scheme(&self) -> Option<&EpubString> {
        self.opf2_scheme.as_ref()
    }
    /// Borrows the EPUB 2 OPF `role` attribute.
    pub fn opf2_role(&self) -> Option<&EpubString> {
        self.opf2_role.as_ref()
    }
    /// Borrows the EPUB 2 OPF `file-as` attribute.
    pub fn opf2_file_as(&self) -> Option<&EpubString> {
        self.opf2_file_as.as_ref()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash, bon::Builder)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned EPUB 3 property-based `meta` element.
///
/// Property tokens preserve authored spelling and an optional known projection. Parsed nodes
/// may omit fields required by the EPUB specification; unknown XML is not retained.
pub struct Meta {
    dir: Option<TextDirection>,
    id: Option<EpubString>,
    /// Required
    #[builder(required, with = Some)]
    property: Option<MetaPropertyToken>,
    scheme: Option<EpubString>,
    xml_lang: Option<EpubString>,
    refines: Option<EpubString>,
    #[builder(required, with = Some)]
    content: Option<EpubString>,
}

impl Meta {
    pub(super) fn from_parsed(
        dir: Option<TextDirection>,
        id: Option<EpubString>,
        property: Option<MetaPropertyToken>,
        scheme: Option<EpubString>,
        xml_lang: Option<EpubString>,
        refines: Option<EpubString>,
        content: Option<EpubString>,
    ) -> Self {
        Self {
            dir,
            id,
            property,
            scheme,
            xml_lang,
            refines,
            content,
        }
    }

    /// Creates a complete unrefined meta node from owned property and content values.
    pub fn new(property: MetaPropertyToken, content: EpubString) -> Self {
        Self {
            dir: None,
            id: None,
            property: Some(property),
            scheme: None,
            xml_lang: None,
            refines: None,
            content: Some(content),
        }
    }

    /// Borrows the optional authored ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Borrows `xml:lang` without language-tag normalization.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }
    /// Borrows the authored refinement reference without URI normalization.
    pub fn refines(&self) -> Option<&EpubString> {
        self.refines.as_ref()
    }
    /// Borrows the source-preserving property token.
    pub fn property(&self) -> Option<&MetaPropertyToken> {
        self.property.as_ref()
    }
    /// Borrows the optional authored scheme.
    pub fn scheme(&self) -> Option<&EpubString> {
        self.scheme.as_ref()
    }
    /// Borrows flattened text content.
    pub fn content(&self) -> Option<&EpubString> {
        self.content.as_ref()
    }
    /// Returns the recognized text direction; invalid parsed tokens are omitted.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }

    /// Projects content as `rendition:layout` when this meta has that known property.
    pub fn rendition_layout(&self) -> Option<RenditionLayout> {
        self.rendition_value(KnownMetaProperty::RenditionLayout)
    }

    /// Projects content as the historical `rendition:flow` source value when applicable.
    pub fn rendition_flow(&self) -> Option<RenditionFlow> {
        self.rendition_value(KnownMetaProperty::RenditionFlow)
    }

    /// Projects content as the historical `rendition:orientation` source value when applicable.
    pub fn rendition_orientation(&self) -> Option<RenditionOrientation> {
        self.rendition_value(KnownMetaProperty::RenditionOrientation)
    }

    /// Projects content as the historical `rendition:spread` source value when applicable.
    pub fn rendition_spread(&self) -> Option<RenditionSpread> {
        self.rendition_value(KnownMetaProperty::RenditionSpread)
    }

    /// Projects content as a deprecated package `rendition:viewport` when applicable.
    ///
    /// Width and height may occur in either order. Malformed, duplicate, non-positive, and
    /// non-finite dimensions are not projected; authored content remains available from
    /// [`Self::content`].
    pub fn rendition_viewport(&self) -> Option<RenditionViewport> {
        if self.property()?.known_value() != Some(KnownMetaProperty::RenditionViewport) {
            return None;
        }

        let mut width = None;
        let mut height = None;
        for component in self.content()?.as_str().split(',') {
            let (key, value) = component.split_once('=')?;
            let value = value.trim().parse::<f64>().ok()?;
            if !value.is_finite() || value <= 0.0 {
                return None;
            }
            if key.trim().eq_ignore_ascii_case("width") {
                if width.replace(value).is_some() {
                    return None;
                }
            } else if key.trim().eq_ignore_ascii_case("height") {
                if height.replace(value).is_some() {
                    return None;
                }
            } else {
                return None;
            }
        }
        Some(RenditionViewport::from_values(width?, height?))
    }

    fn rendition_value<T>(&self, property: KnownMetaProperty) -> Option<T>
    where
        T: FromStr,
    {
        if self.property()?.known_value() != Some(property) {
            return None;
        }
        T::from_str(self.content()?.as_str()).ok()
    }
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// Recognized EPUB metadata property vocabulary terms.
pub enum KnownMetaProperty {
    /// Identifies an alternate-script refinement.
    AlternateScript,
    /// Identifies an authority refinement.
    Authority,
    /// Associates metadata with a collection.
    BelongsToCollection,
    /// Identifies a collection type.
    CollectionType,
    /// Provides display sequence.
    DisplaySeq,
    /// Provides a sorting form.
    FileAs,
    /// Provides a position within a group.
    GroupPosition,
    /// Identifies the identifier scheme or type.
    IdentifierType,
    /// Identifies metadata authority information.
    MetaAuth,
    /// Identifies a contributor or creator role.
    Role,
    #[strum(
        serialize = "rendition:align-x-center",
        to_string = "rendition:align-x-center"
    )]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:align-x-center"))]
    /// Centers content on the horizontal axis.
    RenditionAlignXCenter,
    #[strum(serialize = "rendition:flow", to_string = "rendition:flow")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:flow"))]
    /// Declares rendition flow behavior.
    RenditionFlow,
    #[strum(serialize = "rendition:layout", to_string = "rendition:layout")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:layout"))]
    /// Declares rendition layout behavior.
    RenditionLayout,
    #[strum(
        serialize = "rendition:orientation",
        to_string = "rendition:orientation"
    )]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:orientation"))]
    /// Declares rendition orientation behavior.
    RenditionOrientation,
    #[strum(serialize = "rendition:spread", to_string = "rendition:spread")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread"))]
    /// Declares rendition spread behavior.
    RenditionSpread,
    #[strum(serialize = "rendition:viewport", to_string = "rendition:viewport")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:viewport"))]
    /// Declares rendition viewport dimensions.
    RenditionViewport,
    /// Identifies the source of a resource.
    SourceOf,
    /// Identifies a term.
    Term,
    /// Identifies a title type.
    TitleType,
    #[strum(to_string = "dcterms:modified")]
    #[cfg_attr(feature = "serde", serde(rename = "dcterms:modified"))]
    /// Records the package modification timestamp.
    Dctermsmodified,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A metadata property token retaining authored spelling and an optional known projection.
pub struct MetaPropertyToken {
    raw: EpubString,
    known: Option<KnownMetaProperty>,
}

impl MetaPropertyToken {
    /// Creates a token using the canonical spelling of a known property.
    pub fn known(property: KnownMetaProperty) -> Self {
        let raw = EpubString::new(property.to_string()).expect("known meta property is non-empty");
        Self {
            raw,
            known: Some(property),
        }
    }

    /// Parses a token while preserving its spelling after trimming surrounding whitespace.
    ///
    /// Returns `None` for empty or whitespace-only input. Known recognition is ASCII
    /// case-insensitive.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let raw = EpubString::new(value.as_ref())?;
        let known = KnownMetaProperty::from_str(raw.as_str()).ok();
        Some(Self { raw, known })
    }

    /// Creates a token from authored text after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for empty or whitespace-only input.
    pub fn raw(value: impl AsRef<str>) -> std::result::Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(Self::from_raw)
    }

    /// Classifies an already-trimmed token without changing its stored spelling.
    pub fn from_raw(raw: EpubString) -> Self {
        let known = KnownMetaProperty::from_str(raw.as_str()).ok();
        Self { raw, known }
    }

    /// The stored token.
    pub fn raw_value(&self) -> &EpubString {
        &self.raw
    }

    /// The stored token as `str`.
    pub fn as_str(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the recognized semantic property, if any.
    pub fn known_value(&self) -> Option<KnownMetaProperty> {
        self.known
    }
}

impl From<KnownMetaProperty> for MetaPropertyToken {
    fn from(value: KnownMetaProperty) -> Self {
        Self::known(value)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An OPF metadata `link`, preserving authored href and vocabulary tokens.
///
/// Parsed instances can retain an unusable authored href. Unknown attributes and XML lexical
/// details are not represented.
pub struct MetadataLink {
    href: Option<AuthoredHref>,
    rel: Option<LinkRelToken>,
    refines: Option<EpubString>,
    media_type: Option<EpubString>,
    id: Option<EpubString>,
    hreflang: Option<EpubString>,
    properties: Vec<LinkPropertyToken>,
}

#[bon::bon]
impl MetadataLink {
    pub(super) fn from_parsed(
        href: Option<AuthoredHref>,
        rel: Option<LinkRelToken>,
        refines: Option<EpubString>,
        media_type: Option<EpubString>,
        id: Option<EpubString>,
        hreflang: Option<EpubString>,
        properties: Vec<LinkPropertyToken>,
    ) -> Self {
        Self {
            href,
            rel,
            refines,
            media_type,
            id,
            hreflang,
            properties,
        }
    }

    #[builder]
    /// Creates a complete metadata link from owned typed values.
    ///
    /// The href and vocabulary-token spellings are retained; properties are not deduplicated.
    pub fn new(
        href: EpubHref,
        rel: LinkRelToken,
        refines: Option<EpubString>,
        media_type: Option<EpubString>,
        id: Option<EpubString>,
        hreflang: Option<EpubString>,
        #[builder(default)] properties: Vec<LinkPropertyToken>,
    ) -> Self {
        Self {
            href: Some(AuthoredHref::from(href)),
            rel: Some(rel),
            refines,
            media_type,
            id,
            hreflang,
            properties,
        }
    }

    /// Borrows the optional authored ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Returns an owned usable EPUB href projection.
    ///
    /// Parsed empty or whitespace-only hrefs return `None`; inspect [`Self::authored_href`]
    /// for the source-preserving state.
    pub fn href(&self) -> Option<EpubHref> {
        self.href.as_ref().and_then(AuthoredHref::to_epub_href)
    }
    /// Borrows the source-preserving authored href, including unusable text.
    pub fn authored_href(&self) -> Option<&AuthoredHref> {
        self.href.as_ref()
    }
    /// Borrows the authored refinement reference.
    pub fn refines(&self) -> Option<&EpubString> {
        self.refines.as_ref()
    }
    /// Borrows authored media type text without media-type normalization.
    pub fn media_type(&self) -> Option<&EpubString> {
        self.media_type.as_ref()
    }
    /// Borrows the authored link language without language-tag normalization.
    pub fn hreflang(&self) -> Option<&EpubString> {
        self.hreflang.as_ref()
    }
    /// Borrows the source-preserving relationship token.
    pub fn rel(&self) -> Option<&LinkRelToken> {
        self.rel.as_ref()
    }
    /// Borrows link property tokens in authored order.
    pub fn properties(&self) -> &[LinkPropertyToken] {
        self.properties.as_slice()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A metadata link relationship token with authored spelling and known projection.
pub struct LinkRelToken {
    raw: EpubString,
    known: Option<KnownLinkRel>,
}

impl LinkRelToken {
    /// Creates a token using the canonical spelling of a known relationship.
    pub fn known(rel: KnownLinkRel) -> Self {
        let raw = EpubString::new(rel.to_string()).expect("known rel is non-empty");
        Self {
            raw,
            known: Some(rel),
        }
    }

    /// Parses a relationship while preserving its spelling after trimming surrounding whitespace.
    ///
    /// Returns `None` for empty or whitespace-only input.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let raw = EpubString::new(value.as_ref())?;
        let known = KnownLinkRel::from_str(raw.as_str()).ok();
        Some(Self { raw, known })
    }

    /// Creates a relationship token from authored text after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for empty or whitespace-only input.
    pub fn raw(value: impl AsRef<str>) -> std::result::Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(Self::from_raw)
    }

    /// Classifies an already-trimmed token without changing its stored spelling.
    pub fn from_raw(raw: EpubString) -> Self {
        let known = KnownLinkRel::from_str(raw.as_str()).ok();
        Self { raw, known }
    }

    /// The stored token.
    pub fn raw_value(&self) -> &EpubString {
        &self.raw
    }

    /// The stored token as `str`.
    pub fn as_str(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the recognized relationship, if any.
    pub fn known_value(&self) -> Option<KnownLinkRel> {
        self.known
    }
}

impl From<KnownLinkRel> for LinkRelToken {
    fn from(value: KnownLinkRel) -> Self {
        Self::known(value)
    }
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// A recognized OPF metadata link relationship.
pub enum KnownLinkRel {
    /// An alternate representation.
    Alternate,
    // Use of the marc21xml-record keyword is deprecated. It is replaced by the record keyword with the media-type attribute value "application/marcxml+xml".
    /// A deprecated MARC 21 XML record relationship.
    Marc21xmlRecord,
    // Use of the mods-record keyword is deprecated. It is replaced by the record keyword with the media-type attribute value "application/mods+xml".
    /// A deprecated MODS record relationship.
    ModsRecord,
    // Use of the onix-record keyword is deprecated. It is replaced by the record keyword with the properties attribute value onix.
    /// A deprecated ONIX record relationship.
    OnixRecord,
    /// A generic metadata record.
    Record,
    /// A voicing resource.
    Voicing,
    // deprecated
    /// A deprecated XML signature relationship.
    XmlSignature,
    // deprecated
    /// A deprecated XMP record relationship.
    XmpRecord,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A metadata link property token with authored spelling and known projection.
pub struct LinkPropertyToken {
    raw: EpubString,
    known: Option<KnownLinkProperty>,
}

impl LinkPropertyToken {
    /// Creates a token using the canonical spelling of a known property.
    pub fn known(property: KnownLinkProperty) -> Self {
        let raw = EpubString::new(property.to_string()).expect("known link property is non-empty");
        Self {
            raw,
            known: Some(property),
        }
    }

    /// Parses a property while preserving its spelling after trimming surrounding whitespace.
    ///
    /// Returns `None` for empty or whitespace-only input.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let raw = EpubString::new(value.as_ref())?;
        let known = KnownLinkProperty::from_str(raw.as_str()).ok();
        Some(Self { raw, known })
    }

    /// Creates a property token from authored text after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for empty or whitespace-only input.
    pub fn raw(value: impl AsRef<str>) -> std::result::Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(Self::from_raw)
    }

    /// Classifies an already-trimmed token without changing its stored spelling.
    pub fn from_raw(raw: EpubString) -> Self {
        let known = KnownLinkProperty::from_str(raw.as_str()).ok();
        Self { raw, known }
    }

    /// The stored token.
    pub fn raw_value(&self) -> &EpubString {
        &self.raw
    }

    /// The stored token as `str`.
    pub fn as_str(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the recognized property, if any.
    pub fn known_value(&self) -> Option<KnownLinkProperty> {
        self.known
    }
}

impl From<KnownLinkProperty> for LinkPropertyToken {
    fn from(value: KnownLinkProperty) -> Self {
        Self::known(value)
    }
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
/// A recognized OPF metadata link `properties` token.
pub enum KnownLinkProperty {
    /// Declares an ONIX record.
    Onix,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(value: &str) -> EpubString {
        EpubString::try_new(value).unwrap()
    }

    fn rendition_meta(property: &str, value: &str) -> Meta {
        Meta::new(MetaPropertyToken::raw(property).unwrap(), string(value))
    }

    #[test]
    fn rendition_meta_projections_are_typed_case_insensitive_and_source_preserving() {
        let metas = [
            rendition_meta("RENDITION:LAYOUT", "PRE-PAGINATED"),
            rendition_meta("Rendition:Flow", "SCROLLED-CONTINUOUS"),
            rendition_meta("rendition:ORIENTATION", "LANDSCAPE"),
            rendition_meta("RENDITION:SPREAD", "PORTRAIT"),
        ];

        assert_eq!(
            metas[0].rendition_layout(),
            Some(RenditionLayout::PrePaginated)
        );
        assert_eq!(
            metas[1].rendition_flow(),
            Some(RenditionFlow::ScrolledContinuous)
        );
        assert_eq!(
            metas[2].rendition_orientation(),
            Some(RenditionOrientation::Landscape)
        );
        assert_eq!(metas[3].rendition_spread(), Some(RenditionSpread::Portrait));
        assert_eq!(metas[0].property().unwrap().as_str(), "RENDITION:LAYOUT");
        assert_eq!(metas[0].content().unwrap().as_str(), "PRE-PAGINATED");

        let wrong_property = rendition_meta("custom:layout", "pre-paginated");
        assert_eq!(wrong_property.rendition_layout(), None);
    }

    #[test]
    fn metadata_rendition_getters_skip_refined_and_malformed_values_without_defaults() {
        let mut metadata = Metadata::empty();
        metadata.add_meta(rendition_meta("rendition:layout", "unknown"));
        metadata.add_meta(
            Meta::builder()
                .property(MetaPropertyToken::known(KnownMetaProperty::RenditionLayout))
                .content(string("roll"))
                .refines(string("#chapter"))
                .build(),
        );
        metadata.add_meta(rendition_meta("RENDITION:LAYOUT", "PRE-PAGINATED"));
        metadata.add_meta(rendition_meta("rendition:flow", "scrolled-doc"));
        metadata.add_meta(rendition_meta("rendition:orientation", "portrait"));
        metadata.add_meta(rendition_meta("rendition:spread", "both"));

        assert_eq!(
            metadata.rendition_layout(),
            Some(RenditionLayout::PrePaginated)
        );
        assert_eq!(metadata.rendition_flow(), Some(RenditionFlow::ScrolledDoc));
        assert_eq!(
            metadata.rendition_orientation(),
            Some(RenditionOrientation::Portrait)
        );
        assert_eq!(metadata.rendition_spread(), Some(RenditionSpread::Both));
        assert_eq!(metadata.meta().len(), 6);
        assert_eq!(Metadata::empty().rendition_layout(), None);
    }

    #[test]
    fn rendition_viewport_accepts_either_key_order_and_canonicalizes_valid_numbers() {
        for (value, width, height) in [
            ("width=1200, height=800", 1200.0, 800.0),
            (" HEIGHT = 8e2 , WIDTH = 1200.50 ", 1200.5, 800.0),
        ] {
            let meta = rendition_meta("RENDITION:VIEWPORT", value);
            let viewport = meta.rendition_viewport().unwrap();
            assert_eq!(viewport.width(), width);
            assert_eq!(viewport.height(), height);
            assert_eq!(viewport.width_source(), width.to_string());
            assert_eq!(viewport.height_source(), height.to_string());
            assert_eq!(meta.content().unwrap().as_str(), value.trim());
        }
    }

    #[test]
    fn rendition_viewport_rejects_malformed_or_non_positive_dimensions() {
        let missing_content = Meta::from_parsed(
            None,
            None,
            Some(MetaPropertyToken::known(
                KnownMetaProperty::RenditionViewport,
            )),
            None,
            None,
            None,
            None,
        );
        assert_eq!(missing_content.rendition_viewport(), None);

        for value in [
            "width=1200",
            "width=1200, depth=800",
            "width=1200, width=800, height=600",
            "width=0, height=800",
            "width=-1, height=800",
            "width=NaN, height=800",
            "width=inf, height=800",
            "width=1200px, height=800",
            "width=1200, height=800,",
        ] {
            let meta = Meta::new(
                MetaPropertyToken::known(KnownMetaProperty::RenditionViewport),
                string(value),
            );
            assert_eq!(meta.rendition_viewport(), None, "{value}");
        }
    }

    #[test]
    fn metadata_element_adders_and_getters_share_their_owner_collection() {
        let mut metadata = Metadata::empty();

        macro_rules! assert_element_collection {
            ($add:ident, $get:ident, $value:literal) => {{
                metadata.$add(Element::new(string($value)));
                assert_eq!(
                    metadata.$get().last().and_then(Element::content),
                    EpubString::new($value).as_ref()
                );
            }};
        }

        assert_element_collection!(add_identifier, identifier, "identifier");
        assert_element_collection!(add_title, title, "title");
        assert_element_collection!(add_language, language, "language");
        assert_element_collection!(add_contributor, contributor, "contributor");
        assert_element_collection!(add_coverage, coverage, "coverage");
        assert_element_collection!(add_creator, creator, "creator");
        assert_element_collection!(add_date, date, "date");
        assert_element_collection!(add_description, description, "description");
        assert_element_collection!(add_format, format, "format");
        assert_element_collection!(add_publisher, publisher, "publisher");
        assert_element_collection!(add_relation, relation, "relation");
        assert_element_collection!(add_rights, rights, "rights");
        assert_element_collection!(add_source, source, "source");
        assert_element_collection!(add_subject, subject, "subject");
        assert_element_collection!(add_dc_type, dc_type, "type");
    }

    fn meta(value: &str) -> Meta {
        Meta::new(
            MetaPropertyToken::raw("custom:value").unwrap(),
            string(value),
        )
    }

    fn link(value: &str) -> MetadataLink {
        MetadataLink::builder()
            .href(EpubHref::try_new(value).unwrap())
            .rel(LinkRelToken::known(KnownLinkRel::Record))
            .build()
    }

    #[test]
    fn metadata_mutation_failures_are_typed_and_atomic() {
        let mut metadata = Metadata::empty();
        metadata.add_title(Element::new(string("first")));
        metadata.add_meta(meta("first"));
        metadata.add_link(link("first.json"));

        macro_rules! assert_atomic_error {
            ($operation:expr, $pattern:pat $(if $guard:expr)? ) => {{
                let before = metadata.clone();
                assert!(matches!($operation, Err($pattern) $(if $guard)?));
                assert_eq!(metadata, before);
            }};
        }

        assert_atomic_error!(
            metadata.remove_element_at(TITLE, 1),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == TITLE && index == 1
        );
        assert_atomic_error!(
            metadata.replace_element_at(TITLE, 1, MetadataElement::Title(Element::new(string("replacement")))),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == TITLE && index == 1
        );
        assert_atomic_error!(
            metadata.replace_element_at(TITLE, 0, MetadataElement::Creator(Element::new(string("replacement")))),
            PackageError::MetadataKindMismatch { ref target, ref replacement }
                if target == TITLE && replacement == CREATOR
        );
        assert_atomic_error!(
            metadata.remove_meta_at(1),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == META && index == 1
        );
        assert_atomic_error!(
            metadata.replace_meta_at(1, meta("replacement")),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == META && index == 1
        );
        assert_atomic_error!(
            metadata.remove_link_at(1),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == LINK && index == 1
        );
        assert_atomic_error!(
            metadata.replace_link_at(1, link("replacement.json")),
            PackageError::MetadataIndexMissing { ref kind, index }
                if kind == LINK && index == 1
        );
    }

    #[test]
    fn metadata_replacements_preserve_order_and_removals_close_the_gap() {
        let mut metadata = Metadata::empty();
        for value in ["first", "middle", "last"] {
            metadata.add_title(Element::new(string(value)));
            metadata.add_meta(meta(value));
            metadata.add_link(link(&format!("{value}.json")));
        }

        metadata
            .replace_element_at(
                TITLE,
                1,
                MetadataElement::Title(Element::new(string("replacement"))),
            )
            .unwrap();
        metadata.replace_meta_at(1, meta("replacement")).unwrap();
        metadata
            .replace_link_at(1, link("replacement.json"))
            .unwrap();

        assert_eq!(
            metadata
                .title()
                .iter()
                .filter_map(Element::content)
                .map(EpubString::as_str)
                .collect::<Vec<_>>(),
            ["first", "replacement", "last"]
        );
        assert_eq!(
            metadata
                .meta()
                .iter()
                .filter_map(Meta::content)
                .map(EpubString::as_str)
                .collect::<Vec<_>>(),
            ["first", "replacement", "last"]
        );
        assert_eq!(
            metadata
                .link()
                .iter()
                .filter_map(MetadataLink::href)
                .map(|href| href.as_str().to_string())
                .collect::<Vec<_>>(),
            ["first.json", "replacement.json", "last.json"]
        );

        metadata.remove_element_at(TITLE, 1).unwrap();
        metadata.remove_meta_at(1).unwrap();
        metadata.remove_link_at(1).unwrap();
        assert_eq!(metadata.title().len(), 2);
        assert_eq!(metadata.meta().len(), 2);
        assert_eq!(metadata.link().len(), 2);
        assert_eq!(metadata.title()[1].content().unwrap().as_str(), "last");
        assert_eq!(metadata.meta()[1].content().unwrap().as_str(), "last");
        assert_eq!(metadata.link()[1].href().unwrap().as_str(), "last.json");
    }

    #[test]
    fn element_builder_populates_typed_fields() {
        let element = Element::builder()
            .content(string("content"))
            .id(string("element-id"))
            .dir(TextDirection::Ltr)
            .xml_lang(string("en"))
            .build();

        assert_eq!(element.content(), EpubString::new("content").as_ref());
        assert_eq!(element.id(), EpubString::new("element-id").as_ref());
        assert_eq!(element.dir(), Some(TextDirection::Ltr));
        assert_eq!(element.xml_lang(), EpubString::new("en").as_ref());
    }

    #[test]
    fn metadata_builders_accept_unknown_vocabulary_tokens() {
        let meta = Meta::builder()
            .property(MetaPropertyToken::raw("custom:thing").unwrap())
            .content(string("value"))
            .build();
        assert_eq!(
            meta.property().map(MetaPropertyToken::as_str),
            Some("custom:thing")
        );
        assert_eq!(
            meta.property().and_then(MetaPropertyToken::known_value),
            None
        );

        let link = MetadataLink::builder()
            .href(EpubHref::try_new("meta.json").unwrap())
            .rel(LinkRelToken::raw("custom-record").unwrap())
            .properties(vec![
                KnownLinkProperty::Onix.into(),
                LinkPropertyToken::raw("custom:foo").unwrap(),
            ])
            .build();
        assert_eq!(link.rel().map(LinkRelToken::as_str), Some("custom-record"));
        assert_eq!(
            link.properties()
                .iter()
                .map(LinkPropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["onix", "custom:foo"]
        );
    }

    #[test]
    fn meta_property_tokens_parse_known_values_case_insensitively_and_preserve_unknown_values() {
        let known = MetaPropertyToken::raw("alTERnate-script").unwrap();
        assert_eq!(
            known.known_value(),
            Some(KnownMetaProperty::AlternateScript)
        );
        assert_eq!(known.as_str(), "alTERnate-script");
        assert_eq!(
            KnownMetaProperty::AlternateScript.to_string(),
            "alternate-script"
        );

        let unknown = MetaPropertyToken::raw("dcterms:published").unwrap();
        assert_eq!(unknown.as_str(), "dcterms:published");
        assert_eq!(unknown.known_value(), None);
    }
}
