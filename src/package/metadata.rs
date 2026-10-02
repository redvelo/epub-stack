//! Inspect and edit Dublin Core and OPF metadata.
//!
//! [`Metadata`] groups supported Dublin Core elements, EPUB 3 [`Meta`] and
//! [`MetadataLink`] nodes, and EPUB 2 metadata. Dublin
//! Core elements are keyed by [`DcElement`], so code can handle every kind uniformly. Property
//! and relationship token types retain their authored spelling after [`EpubString`] trims
//! surrounding Unicode whitespace, while exposing recognized vocabulary values separately.
//!
//! Read the book's title and other metadata.
//!
//! ```
//! use epub_stack::{
//!     EpubZip,
//!     package::{
//!         RenditionLayout,
//!         metadata::{DcElement, Meta},
//!     },
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let book = EpubZip::open("fixtures/real/alice-in-wonderland.epub")?.default_rendition()?;
//! let metadata = book.package().metadata();
//!
//! for title in metadata
//!     .elements(DcElement::Title)
//!     .iter()
//!     .filter_map(|title| title.content())
//! {
//!     println!("Title: {title}");
//! }
//!
//! let layout = metadata
//!     .meta()
//!     .iter()
//!     .filter(|meta| meta.refines().is_none())
//!     .find_map(Meta::rendition_layout);
//!
//! match layout {
//!     Some(RenditionLayout::Reflowable) => println!("Reflowable content"),
//!     Some(RenditionLayout::PrePaginated) => println!("Fixed-layout pages"),
//!     Some(RenditionLayout::Roll) => println!("Continuous fixed-layout content"),
//!     None => println!("No recognized layout declared"),
//! }
//! # Ok(())
//! # }
//! ```

use super::legacy::Opf2Meta;
use super::{
    CONTRIBUTOR, COVERAGE, CREATOR, DATE, DESCRIPTION, FORMAT, IDENTIFIER, LANGUAGE,
    MetadataCollection, PUBLISHER, PackageError, RELATION, RIGHTS, RenditionFlow, RenditionLayout,
    RenditionOrientation, RenditionSpread, RenditionViewport, Result, SOURCE, SUBJECT, TITLE, TYPE,
};
use crate::resource::{AuthoredHref, EpubHref};
use crate::semantics::TextDirection;
use crate::string::EpubString;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A supported Dublin Core metadata element.
pub enum DcElement {
    /// `dc:identifier`.
    Identifier,
    /// `dc:title`.
    Title,
    /// `dc:language`.
    Language,
    /// `dc:contributor`.
    Contributor,
    /// `dc:coverage`.
    Coverage,
    /// `dc:creator`.
    Creator,
    /// `dc:date`.
    Date,
    /// `dc:description`.
    Description,
    /// `dc:format`.
    Format,
    /// `dc:publisher`.
    Publisher,
    /// `dc:relation`.
    Relation,
    /// `dc:rights`.
    Rights,
    /// `dc:source`.
    Source,
    /// `dc:subject`.
    Subject,
    /// `dc:type`.
    Type,
}

impl DcElement {
    /// Every supported element in OPF serialization order.
    pub const ALL: [Self; 15] = [
        Self::Title,
        Self::Language,
        Self::Identifier,
        Self::Creator,
        Self::Contributor,
        Self::Publisher,
        Self::Description,
        Self::Subject,
        Self::Rights,
        Self::Date,
        Self::Format,
        Self::Type,
        Self::Source,
        Self::Relation,
        Self::Coverage,
    ];

    /// Returns the Dublin Core local name.
    pub fn local_name(self) -> &'static str {
        match self {
            Self::Identifier => IDENTIFIER,
            Self::Title => TITLE,
            Self::Language => LANGUAGE,
            Self::Contributor => CONTRIBUTOR,
            Self::Coverage => COVERAGE,
            Self::Creator => CREATOR,
            Self::Date => DATE,
            Self::Description => DESCRIPTION,
            Self::Format => FORMAT,
            Self::Publisher => PUBLISHER,
            Self::Relation => RELATION,
            Self::Rights => RIGHTS,
            Self::Source => SOURCE,
            Self::Subject => SUBJECT,
            Self::Type => TYPE,
        }
    }

    /// Returns the element for a case-sensitive Dublin Core local name.
    pub fn from_local_name(local_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|element| element.local_name() == local_name)
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
        pub(crate) fn $method(&mut self, value: $ty) {
            self.$field.push(value);
        }
    };
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The modeled contents of an OPF `metadata` block.
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
    /// A Dublin Core element and the kind it was modeled under.
    Element {
        /// The Dublin Core element kind.
        kind: DcElement,
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
    vec_getter!(meta, Meta);
    vec_getter!(opf2meta, Opf2Meta);
    vec_getter!(link, MetadataLink);

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
        let elements = DcElement::ALL.into_iter().flat_map(move |kind| {
            self.elements(kind)
                .iter()
                .map(move |element| MetadataIdTarget::Element { kind, element })
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
    pub(crate) fn add_element(&mut self, kind: DcElement, element: Element) {
        self.elements_mut(kind).push(element);
    }

    /// Returns the modeled elements of one Dublin Core kind in source order.
    pub fn elements(&self, element: DcElement) -> &[Element] {
        match element {
            DcElement::Identifier => &self.identifier,
            DcElement::Title => &self.title,
            DcElement::Language => &self.language,
            DcElement::Contributor => &self.contributor,
            DcElement::Coverage => &self.coverage,
            DcElement::Creator => &self.creator,
            DcElement::Date => &self.date,
            DcElement::Description => &self.description,
            DcElement::Format => &self.format,
            DcElement::Publisher => &self.publisher,
            DcElement::Relation => &self.relation,
            DcElement::Rights => &self.rights,
            DcElement::Source => &self.source,
            DcElement::Subject => &self.subject,
            DcElement::Type => &self.dc_type,
        }
    }

    /// Removes a Dublin Core element by kind-relative index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub(crate) fn remove_element_at(&mut self, element: DcElement, index: usize) -> Result<()> {
        let elements = self.elements_mut(element);
        if index >= elements.len() {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Element(element),
                index,
            });
        }
        elements.remove(index);
        Ok(())
    }

    /// Replaces a Dublin Core element in place by kind-relative index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub(crate) fn replace_element_at(
        &mut self,
        element: DcElement,
        index: usize,
        value: Element,
    ) -> Result<()> {
        let elements = self.elements_mut(element);
        let Some(slot) = elements.get_mut(index) else {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Element(element),
                index,
            });
        };
        *slot = value;
        Ok(())
    }

    /// Removes a `meta` node by zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MetadataIndexMissing`] without mutation when out of range.
    pub(crate) fn remove_meta_at(&mut self, index: usize) -> Result<()> {
        if index >= self.meta.len() {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Meta,
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
    pub(crate) fn replace_meta_at(&mut self, index: usize, meta: Meta) -> Result<()> {
        if index >= self.meta.len() {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Meta,
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
    pub(crate) fn remove_link_at(&mut self, index: usize) -> Result<()> {
        if index >= self.link.len() {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Link,
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
    pub(crate) fn replace_link_at(&mut self, index: usize, link: MetadataLink) -> Result<()> {
        if index >= self.link.len() {
            return Err(PackageError::MetadataIndexMissing {
                collection: MetadataCollection::Link,
                index,
            });
        }
        self.link[index] = link;
        Ok(())
    }

    fn elements_mut(&mut self, element: DcElement) -> &mut Vec<Element> {
        match element {
            DcElement::Identifier => &mut self.identifier,
            DcElement::Title => &mut self.title,
            DcElement::Language => &mut self.language,
            DcElement::Contributor => &mut self.contributor,
            DcElement::Coverage => &mut self.coverage,
            DcElement::Creator => &mut self.creator,
            DcElement::Date => &mut self.date,
            DcElement::Description => &mut self.description,
            DcElement::Format => &mut self.format,
            DcElement::Publisher => &mut self.publisher,
            DcElement::Relation => &mut self.relation,
            DcElement::Rights => &mut self.rights,
            DcElement::Source => &mut self.source,
            DcElement::Subject => &mut self.subject,
            DcElement::Type => &mut self.dc_type,
        }
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
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A Dublin Core metadata element.
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
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An EPUB 3 property-based `meta` element.
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
    derive(serde::Serialize, serde::Deserialize),
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
    DctermsModified,
}

/// A `meta` property token retaining its authored spelling and optional known projection.
pub type MetaPropertyToken = crate::vocab::VocabToken<KnownMetaProperty>;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
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

/// A link `rel` token retaining its authored spelling and optional known projection.
pub type LinkRelToken = crate::vocab::VocabToken<KnownLinkRel>;

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
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

/// A link `properties` token retaining its authored spelling and optional known projection.
pub type LinkPropertyToken = crate::vocab::VocabToken<KnownLinkProperty>;

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
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

    fn first_unrefined<'a, T>(
        metadata: &'a Metadata,
        project: impl Fn(&'a Meta) -> Option<T> + 'a,
    ) -> Option<T> {
        metadata
            .meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(project)
    }
    use super::*;

    fn string(value: &str) -> EpubString {
        EpubString::try_new(value).unwrap()
    }

    fn rendition_meta(property: &str, value: &str) -> Meta {
        Meta::new(MetaPropertyToken::try_new(property).unwrap(), string(value))
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
                .property(MetaPropertyToken::from(KnownMetaProperty::RenditionLayout))
                .content(string("roll"))
                .refines(string("#chapter"))
                .build(),
        );
        metadata.add_meta(rendition_meta("RENDITION:LAYOUT", "PRE-PAGINATED"));
        metadata.add_meta(rendition_meta("rendition:flow", "scrolled-doc"));
        metadata.add_meta(rendition_meta("rendition:orientation", "portrait"));
        metadata.add_meta(rendition_meta("rendition:spread", "both"));

        assert_eq!(
            first_unrefined(&metadata, Meta::rendition_layout),
            Some(RenditionLayout::PrePaginated)
        );
        assert_eq!(
            first_unrefined(&metadata, Meta::rendition_flow),
            Some(RenditionFlow::ScrolledDoc)
        );
        assert_eq!(
            first_unrefined(&metadata, Meta::rendition_orientation),
            Some(RenditionOrientation::Portrait)
        );
        assert_eq!(
            first_unrefined(&metadata, Meta::rendition_spread),
            Some(RenditionSpread::Both)
        );
        assert_eq!(metadata.meta().len(), 6);
        assert_eq!(
            first_unrefined(&Metadata::empty(), Meta::rendition_layout),
            None
        );
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
            assert_eq!(meta.content().unwrap().as_str(), value.trim());
            assert_eq!(meta.property().unwrap().as_str(), "RENDITION:VIEWPORT");
        }
    }

    #[test]
    fn rendition_viewport_rejects_malformed_or_non_positive_dimensions() {
        let missing_content = Meta::from_parsed(
            None,
            None,
            Some(MetaPropertyToken::from(
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
                MetaPropertyToken::from(KnownMetaProperty::RenditionViewport),
                string(value),
            );
            assert_eq!(meta.rendition_viewport(), None, "{value}");
        }
    }

    #[test]
    fn metadata_elements_are_stored_under_their_own_kind() {
        let mut metadata = Metadata::empty();

        for kind in DcElement::ALL {
            let value = kind.local_name();
            metadata.add_element(kind, Element::new(string(value)));
            assert_eq!(
                metadata.elements(kind).last().and_then(Element::content),
                EpubString::new(value).as_ref(),
                "{value}"
            );
            assert_eq!(metadata.elements(kind).len(), 1, "{value}");
        }
    }

    fn meta(value: &str) -> Meta {
        Meta::new(
            MetaPropertyToken::try_new("custom:value").unwrap(),
            string(value),
        )
    }

    fn link(value: &str) -> MetadataLink {
        MetadataLink::builder()
            .href(EpubHref::try_new(value).unwrap())
            .rel(LinkRelToken::from(KnownLinkRel::Record))
            .build()
    }

    #[test]
    fn metadata_mutation_failures_are_typed_and_atomic() {
        let mut metadata = Metadata::empty();
        metadata.add_element(DcElement::Title, Element::new(string("first")));
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
            metadata.remove_element_at(DcElement::Title, 1),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Element(DcElement::Title) && index == 1
        );
        assert_atomic_error!(
            metadata.replace_element_at(DcElement::Title, 1, Element::new(string("replacement"))),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Element(DcElement::Title) && index == 1
        );
        assert_atomic_error!(
            metadata.remove_meta_at(1),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Meta && index == 1
        );
        assert_atomic_error!(
            metadata.replace_meta_at(1, meta("replacement")),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Meta && index == 1
        );
        assert_atomic_error!(
            metadata.remove_link_at(1),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Link && index == 1
        );
        assert_atomic_error!(
            metadata.replace_link_at(1, link("replacement.json")),
            PackageError::MetadataIndexMissing { collection, index }
                if collection == MetadataCollection::Link && index == 1
        );
    }

    #[test]
    fn metadata_replacements_preserve_order_and_removals_close_the_gap() {
        let mut metadata = Metadata::empty();
        for value in ["first", "middle", "last"] {
            metadata.add_element(DcElement::Title, Element::new(string(value)));
            metadata.add_meta(meta(value));
            metadata.add_link(link(&format!("{value}.json")));
        }

        metadata
            .replace_element_at(DcElement::Title, 1, Element::new(string("replacement")))
            .unwrap();
        metadata.replace_meta_at(1, meta("replacement")).unwrap();
        metadata
            .replace_link_at(1, link("replacement.json"))
            .unwrap();

        assert_eq!(
            metadata
                .elements(DcElement::Title)
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

        metadata.remove_element_at(DcElement::Title, 1).unwrap();
        metadata.remove_meta_at(1).unwrap();
        metadata.remove_link_at(1).unwrap();
        assert_eq!(metadata.elements(DcElement::Title).len(), 2);
        assert_eq!(metadata.meta().len(), 2);
        assert_eq!(metadata.link().len(), 2);
        assert_eq!(
            metadata.elements(DcElement::Title)[1]
                .content()
                .unwrap()
                .as_str(),
            "last"
        );
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
            .property(MetaPropertyToken::try_new("custom:thing").unwrap())
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
            .rel(LinkRelToken::try_new("custom-record").unwrap())
            .properties(vec![
                KnownLinkProperty::Onix.into(),
                LinkPropertyToken::try_new("custom:foo").unwrap(),
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
        let known = MetaPropertyToken::try_new("alTERnate-script").unwrap();
        assert_eq!(
            known.known_value(),
            Some(KnownMetaProperty::AlternateScript)
        );
        assert_eq!(known.as_str(), "alTERnate-script");
        assert_eq!(
            KnownMetaProperty::AlternateScript.to_string(),
            "alternate-script"
        );

        let unknown = MetaPropertyToken::try_new("dcterms:published").unwrap();
        assert_eq!(unknown.as_str(), "dcterms:published");
        assert_eq!(unknown.known_value(), None);
    }
}
