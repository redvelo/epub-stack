//! Read, inspect, and build OPF package documents.
//!
//! Start with [`Package::parse`] to inspect an existing package, or [`Package::new_minimal`] to
//! create the package model for a new publication. Use [`metadata`] for Dublin Core and OPF
//! metadata, [`manifest`] for resource declarations, [`spine`] for reading order, [`collection`]
//! for grouped resources, and [`legacy`] for EPUB 2 structures.
//!
//! String fields represented by [`EpubString`] trim leading and trailing Unicode whitespace and
//! reject an empty result. Manifest IDs and their relationship attributes instead preserve exact
//! decoded source text and use XML whitespace only when validating or matching. Parsing and
//! normalized generation preserve selected authored values, but do not provide a source-preserving
//! XML round trip.

pub mod collection;
mod generate;
pub mod legacy;
pub mod manifest;
pub mod metadata;
mod parse;
mod rendition;
pub mod spine;

use crate::media_type::MediaType;
use crate::resource::{EpubHref, ResourceSelection};
use crate::semantics::TextDirection;
use crate::string::EpubString;
use collection::Collection;
use legacy::Guide;
use manifest::{KnownManifestProperty, Manifest, ManifestItem};
use metadata::{
    DcElement, Element, KnownMetaProperty, Meta, MetaPropertyToken, Metadata, MetadataIdLookup,
    MetadataIdTarget,
};
use spine::{ItemRef, Spine};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

#[cfg(test)]
use spine::PageProgressionDirection;

pub use rendition::{
    ReadingOrderPresentation, RenditionCandidate, RenditionSetting, RenditionValueSource,
};

/// Formats a `dcterms:modified` value as whole-second UTC RFC 3339 text.
pub(crate) fn dcterms_modified(modified: OffsetDateTime) -> Result<EpubString> {
    let modified = modified
        .to_offset(UtcOffset::UTC)
        .replace_nanosecond(0)
        .expect("zero is a valid nanosecond")
        .format(&Rfc3339)?;
    required_package_string(modified, PackageField::Modified)
}

pub(crate) const DC_NS: &str = "http://purl.org/dc/elements/1.1/";
pub(crate) const OPF_NS: &str = "http://www.idpf.org/2007/opf";
pub(crate) const ID: &str = "id";
pub(crate) const VERSION: &str = "version";
pub(crate) const IDREF: &str = "idref";
pub(crate) const HREF: &str = "href";
pub(crate) const DIR: &str = "dir";
pub(crate) const LANG: &str = "lang";
pub(crate) const PACKAGE: &str = "package";
pub(crate) const METADATA: &str = "metadata";
pub(crate) const MANIFEST: &str = "manifest";
pub(crate) const SPINE: &str = "spine";
pub(crate) const GUIDE: &str = "guide";
pub(crate) const COLLECTION: &str = "collection";
pub(crate) const ITEM: &str = "item";
pub(crate) const ITEMREF: &str = "itemref";
pub(crate) const IDENTIFIER: &str = "identifier";
pub(crate) const TITLE: &str = "title";
pub(crate) const LANGUAGE: &str = "language";
pub(crate) const CONTRIBUTOR: &str = "contributor";
pub(crate) const COVERAGE: &str = "coverage";
pub(crate) const CREATOR: &str = "creator";
pub(crate) const DATE: &str = "date";
pub(crate) const DESCRIPTION: &str = "description";
pub(crate) const FORMAT: &str = "format";
pub(crate) const PUBLISHER: &str = "publisher";
pub(crate) const RELATION: &str = "relation";
pub(crate) const RIGHTS: &str = "rights";
pub(crate) const SOURCE: &str = "source";
pub(crate) const SUBJECT: &str = "subject";
pub(crate) const TYPE: &str = "type";
pub(crate) const META: &str = "meta";
pub(crate) const LINK: &str = "link";
pub(crate) const PROPERTY: &str = "property";
pub(crate) const PROPERTIES: &str = "properties";
pub(crate) const MEDIA_TYPE: &str = "media-type";
pub(crate) const MEDIA_OVERLAY: &str = "media-overlay";
pub(crate) const ROLE: &str = "role";
pub(crate) const LINEAR: &str = "linear";
pub(crate) const REFERENCE: &str = "reference";
pub(crate) const REFINES: &str = "refines";
pub(crate) const HREFLANG: &str = "hreflang";
pub(crate) const REL: &str = "rel";
pub(crate) const NAME: &str = "name";
pub(crate) const CONTENT: &str = "content";
pub(crate) const SCHEME: &str = "scheme";
pub(crate) const FALLBACK: &str = "fallback";
pub(crate) const UNIQUE_IDENTIFIER: &str = "unique-identifier";
pub(crate) const TOC: &str = "toc";
pub(crate) const PAGE_PROGRESSION_DIRECTION: &str = "page-progression-direction";
pub(crate) const PREFIX: &str = "prefix";
type Result<T> = std::result::Result<T, PackageError>;

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, Hash, strum_macros::Display, strum_macros::EnumString,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// A recognized EPUB rendition layout value.
pub enum RenditionLayout {
    /// Content participates in dynamic pagination or scrolling.
    Reflowable,
    /// Content uses author-defined page geometry.
    PrePaginated,
    /// Content uses the EPUB 3.4 continuous `roll` layout.
    Roll,
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, Hash, strum_macros::Display, strum_macros::EnumString,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// An authored `rendition:flow` value retained for historical EPUB inspection.
///
/// EPUB 3.4 classifies this rendition control as outdated. It represents source metadata, not
/// application or reading-system runtime policy.
pub enum RenditionFlow {
    /// The author expressed no flow preference.
    Auto,
    /// The author preferred dynamic pagination.
    Paginated,
    /// The author preferred continuous scrolling across spine items.
    ScrolledContinuous,
    /// The author preferred separately scrolled spine items.
    ScrolledDoc,
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, Hash, strum_macros::Display, strum_macros::EnumString,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// An authored `rendition:orientation` value retained for historical EPUB inspection.
///
/// EPUB 3.4 classifies this rendition control as outdated. It represents source metadata, not
/// application or reading-system runtime policy.
pub enum RenditionOrientation {
    /// The author left orientation selection to the reading system.
    Auto,
    /// The author preferred landscape orientation.
    Landscape,
    /// The author preferred portrait orientation.
    Portrait,
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, Hash, strum_macros::Display, strum_macros::EnumString,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// An authored `rendition:spread` value retained for historical EPUB inspection.
///
/// EPUB 3.4 classifies this rendition control as outdated. It represents source metadata, not
/// application or reading-system runtime policy.
pub enum RenditionSpread {
    /// The author left synthetic-spread selection to the reading system.
    Auto,
    /// The author preferred spreads in both orientations.
    Both,
    /// The author preferred spreads only in landscape orientation.
    Landscape,
    /// The author preferred no synthetic spreads.
    None,
    /// The author preferred spreads only in portrait orientation.
    Portrait,
}

#[derive(Debug, PartialEq, Clone, Copy)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A parsed deprecated package `rendition:viewport` source value.
///
/// EPUB 3.4 deprecates this package metadata control. The projection records positive, finite CSS
/// pixel dimensions authored as `width=x, height=y`; it does not define runtime viewport policy.
/// The original complete value remains available from [`metadata::Meta::content`].
pub struct RenditionViewport {
    width: f64,
    height: f64,
}

impl RenditionViewport {
    pub(crate) fn from_values(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// Returns the positive, finite width in CSS pixels.
    pub fn width(&self) -> f64 {
        self.width
    }

    /// Returns the positive, finite height in CSS pixels.
    pub fn height(&self) -> f64 {
        self.height
    }
}

/// A named field in the OPF package model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PackageField {
    /// The `element id` field.
    ElementId,
    /// The `identifier` field.
    Identifier,
    /// The `itemref id` field.
    ItemRefId,
    /// The `itemref idref` field.
    ItemRefIdref,
    /// The `language` field.
    Language,
    /// The `manifest item href` field.
    ManifestItemHref,
    /// The `manifest item id` field.
    ManifestItemId,
    /// The `manifest item media-type` field.
    ManifestItemMediaType,
    /// The `modified` field.
    Modified,
    /// The `package language` field.
    PackageLanguage,
    /// The `title` field.
    Title,
}

impl PackageField {
    /// Returns the field name used in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ElementId => "element id",
            Self::Identifier => "identifier",
            Self::ItemRefId => "itemref id",
            Self::ItemRefIdref => "itemref idref",
            Self::Language => "language",
            Self::ManifestItemHref => "manifest item href",
            Self::ManifestItemId => "manifest item id",
            Self::ManifestItemMediaType => "manifest item media-type",
            Self::Modified => "modified",
            Self::PackageLanguage => "package language",
            Self::Title => "title",
        }
    }
}

impl std::fmt::Display for PackageField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An indexable metadata node collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MetadataCollection {
    /// Dublin Core elements of one kind.
    Element(
        /// The Dublin Core element kind.
        DcElement,
    ),
    /// The `meta` node list.
    Meta,
    /// The `link` node list.
    Link,
}

impl MetadataCollection {
    /// Returns the collection name used in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Element(element) => element.local_name(),
            Self::Meta => META,
            Self::Link => LINK,
        }
    }
}

impl std::fmt::Display for MetadataCollection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure parsing, generating, or atomically mutating an OPF package model.
#[non_exhaustive]
pub enum PackageError {
    /// No XML document element was found.
    #[error("Package document has no root element")]
    RootMissing,
    /// The document element was not a single OPF `package` element.
    #[error("Package document root must be package in the OPF namespace")]
    RootInvalid,
    /// More than one OPF `spine` element was encountered.
    #[error("Package document contains more than one spine")]
    SpineDuplicate,
    /// A required programmatic input was empty or whitespace-only.
    #[error("Package field is empty: {field}")]
    EmptyField {
        /// The rejected field.
        field: PackageField,
    },
    /// A programmatic manifest ID or IDREF is invalid.
    #[error(transparent)]
    InvalidManifestId(#[from] InvalidManifestId),
    /// A manifest mutation would orphan a spine reference.
    #[error("Manifest item still referenced by spine: {id}")]
    ManifestItemInUse {
        /// The owned manifest ID still referenced by the spine.
        id: String,
    },
    /// No manifest item has the requested ID.
    #[error("Manifest item not found: {id}")]
    ManifestItemMissing {
        /// The requested manifest ID.
        id: String,
    },
    /// A manifest item already has the proposed ID.
    #[error("Manifest item id already exists: {id}")]
    ManifestIdDuplicate {
        /// The duplicate manifest ID.
        id: String,
    },
    /// A manifest item already has the proposed authored href.
    #[error("Manifest item href already exists: {href}")]
    ManifestHrefDuplicate {
        /// The duplicate authored href.
        href: String,
    },
    /// A manifest index is outside the item list.
    #[error("Manifest item index not found: {index}")]
    ManifestItemIndexMissing {
        /// The requested zero-based index.
        index: usize,
    },
    /// A spine index is outside the itemref list.
    #[error("Spine itemref index not found: {index}")]
    SpineItemrefIndexMissing {
        /// The requested zero-based index.
        index: usize,
    },
    /// A metadata index is outside the selected node collection.
    #[error("Metadata {collection} index not found: {index}")]
    MetadataIndexMissing {
        /// The indexed node collection.
        collection: MetadataCollection,
        /// The requested zero-based index within that collection.
        index: usize,
    },
    /// A collection tree exceeds the bounded parser/generator depth.
    #[error("Collection nesting exceeds the maximum depth of {limit}")]
    CollectionNestingLimitExceeded {
        /// The maximum accepted number of collection elements on one branch.
        limit: usize,
    },
    /// XML parsing or writing failed.
    #[error("XML error: {source}")]
    Xml {
        #[from]
        /// The underlying XML failure.
        source: quick_xml::Error,
    },
    /// Writing generated package XML failed.
    #[error("IO error: {source}")]
    Io {
        #[from]
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// Formatting a package modification timestamp failed.
    #[error("Could not format package modification time: {source}")]
    TimeFormat {
        #[from]
        /// The timestamp formatting failure.
        source: time::error::Format,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid manifest ID: {value:?}")]
/// A manifest ID or IDREF that is not an XML `NCName` after XML whitespace handling.
pub struct InvalidManifestId {
    value: String,
}

impl InvalidManifestId {
    /// Returns the rejected caller-supplied text.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Normalizes authored manifest ID or IDREF text the way the resource index does.
///
/// Surrounding XML whitespace is trimmed and the result must be an XML `NCName`. Use this to
/// resolve authored IDREFs outside the crate, for example to check facts received from another
/// process against an authored manifest.
///
/// # Errors
///
/// Returns [`InvalidManifestId`] when the trimmed text is not an `NCName`.
pub fn normalize_manifest_id(value: &str) -> std::result::Result<&str, InvalidManifestId> {
    let normalized = value.trim_matches(is_xml_whitespace);
    if is_ncname(normalized) {
        Ok(normalized)
    } else {
        Err(InvalidManifestId {
            value: value.to_string(),
        })
    }
}

pub(crate) fn manifest_ids_equal(left: &str, right: &str) -> bool {
    match (normalize_manifest_id(left), normalize_manifest_id(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_xml_whitespace(ch: char) -> bool {
    matches!(ch, '\u{9}' | '\u{A}' | '\u{D}' | ' ')
}

fn is_ncname(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_ncname_start(first) && chars.all(is_ncname_char)
}

fn is_ncname_start(ch: char) -> bool {
    matches!(ch,
        'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
        | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}'
    )
}

fn is_ncname_char(ch: char) -> bool {
    is_ncname_start(ch)
        || matches!(ch,
            '-' | '.' | '0'..='9' | '\u{B7}'
            | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}'
        )
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The semantic projection of an OPF package document.
///
/// Preserves order within modeled groups, authored tokens, and hrefs, but not unknown XML or
/// source formatting.
pub struct Package {
    unique_identifier_id: Option<EpubString>,
    version: Option<EpubVersion>,
    xml_lang: Option<EpubString>,
    id: Option<EpubString>,
    dir: Option<TextDirection>,
    prefix: Option<EpubString>,
    metadata: Metadata,
    manifest: Manifest,
    spine: Spine,
    guide: Option<Guide>,
    collections: Vec<Collection>,
}

impl Package {
    /// Borrows the package element's optional `id`.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Borrows the package element's optional `xml:lang` without language-tag normalization.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }

    /// Borrows the authored EPUB vocabulary prefix declaration.
    pub fn prefix(&self) -> Option<&EpubString> {
        self.prefix.as_ref()
    }

    /// Returns the recognized package text direction; unknown authored tokens are not modeled.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }

    /// Creates the package model used by a new, otherwise empty EPUB 3 publication.
    ///
    /// The package declares only `nav.xhtml`; callers can add content and reading-order
    /// entries through package operations or [`crate::EpubEdit`].
    /// The identifier, title, and language are trimmed, but are otherwise stored as supplied:
    /// this constructor does not normalize identifier syntax or language tags. The modification
    /// time is converted to whole-second UTC RFC 3339.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::EmptyField`] for an empty required string, a time-formatting
    /// error, or a manifest consistency error while adding the generated navigation item.
    pub fn new_minimal(
        identifier: impl AsRef<str>,
        title: impl AsRef<str>,
        language: impl AsRef<str>,
        modified: OffsetDateTime,
    ) -> Result<Self> {
        let mut metadata = Metadata::empty();
        let title = Element::new(required_package_string(title, PackageField::Title)?);
        let identifier = Element::builder()
            .id(required_package_string("uid", PackageField::ElementId)?)
            .content(required_package_string(
                identifier,
                PackageField::Identifier,
            )?)
            .build();
        let language_elem =
            Element::new(required_package_string(&language, PackageField::Language)?);
        metadata.add_element(DcElement::Title, title);
        metadata.add_element(DcElement::Identifier, identifier);
        metadata.add_element(DcElement::Language, language_elem);
        metadata.add_meta(Meta::new(
            MetaPropertyToken::from(KnownMetaProperty::DctermsModified),
            dcterms_modified(modified)?,
        ));

        let mut manifest = Manifest::new_empty();
        let nav_item = ManifestItem::builder()
            .id(required_package_string(
                "nav",
                PackageField::ManifestItemId,
            )?)
            .href(required_package_href(
                "nav.xhtml",
                PackageField::ManifestItemHref,
            )?)
            .media_type(required_media_type(
                "application/xhtml+xml",
                PackageField::ManifestItemMediaType,
            )?)
            .properties(vec![KnownManifestProperty::Nav.into()])
            .build()?;
        manifest.add_item(nav_item)?;

        Ok(Self {
            unique_identifier_id: EpubString::new("uid"),
            version: Some(EpubVersion::default()),
            xml_lang: Some(required_package_string(
                language,
                PackageField::PackageLanguage,
            )?),
            id: None,
            dir: None,
            prefix: None,
            metadata,
            manifest,
            spine: Spine::new_empty(),
            guide: None,
            collections: Vec::new(),
        })
    }

    /// Borrows the `unique-identifier` attribute's target ID.
    pub fn unique_identifier_id(&self) -> Option<&EpubString> {
        self.unique_identifier_id.as_ref()
    }
    /// Returns the recognized EPUB major package version.
    ///
    /// Unknown parsed version spellings are represented as `None`.
    pub fn version(&self) -> Option<EpubVersion> {
        self.version
    }

    /// Borrows the unique identifier's content when its authored ID resolves exactly once.
    ///
    /// Missing, ambiguous, or non-identifier targets return `None`.
    pub fn unique_identifier(&self) -> Option<&EpubString> {
        match self.unique_identifier_target() {
            MetadataIdLookup::Unique(MetadataIdTarget::Element {
                kind: DcElement::Identifier,
                element,
            }) => element.content(),
            _ => None,
        }
    }

    /// Resolves the package `unique-identifier` relationship without hiding ambiguity.
    pub fn unique_identifier_target(&self) -> MetadataIdLookup<'_> {
        self.unique_identifier_id()
            .map_or(MetadataIdLookup::Missing, |id| {
                self.metadata.authored_id(id)
            })
    }
    /// Borrows this package's owned metadata model.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Mutably borrows this package's owned metadata model.
    ///
    /// Direct metadata edits do not preserve original OPF source layout.
    pub(crate) fn metadata_mut(&mut self) -> &mut Metadata {
        &mut self.metadata
    }

    /// Borrows this package's owned manifest model.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Adds an owned manifest item after normalizing duplicate property tokens.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-ID or duplicate-authored-href error. Failure leaves the package
    /// unchanged.
    pub(crate) fn add_manifest_item(&mut self, item: ManifestItem) -> Result<()> {
        self.manifest.add_item(item)
    }

    pub(crate) fn remove_manifest_item_at(&mut self, index: usize) -> Result<()> {
        let Some(item) = self.manifest.items().get(index) else {
            return Err(PackageError::ManifestItemIndexMissing { index });
        };
        if let Some(id) = item.id().and_then(|id| normalize_manifest_id(id).ok())
            && self.spine.itemrefs().iter().any(|itemref| {
                itemref
                    .idref()
                    .and_then(|idref| normalize_manifest_id(idref).ok())
                    == Some(id)
            })
        {
            return Err(PackageError::ManifestItemInUse { id: id.to_string() });
        }
        self.manifest
            .remove_item_at(index)
            .expect("validated manifest index");
        Ok(())
    }

    pub(crate) fn replace_manifest_item_at(
        &mut self,
        index: usize,
        item: ManifestItem,
    ) -> Result<()> {
        let Some(existing) = self.manifest.items().get(index) else {
            return Err(PackageError::ManifestItemIndexMissing { index });
        };
        if !existing
            .id()
            .zip(item.id())
            .is_some_and(|(existing_id, replacement_id)| {
                manifest_ids_equal(existing_id, replacement_id)
            })
            && let Some(id) = existing.id().and_then(|id| normalize_manifest_id(id).ok())
            && self.spine.itemrefs().iter().any(|itemref| {
                itemref
                    .idref()
                    .and_then(|idref| normalize_manifest_id(idref).ok())
                    == Some(id)
            })
        {
            return Err(PackageError::ManifestItemInUse { id: id.to_string() });
        }
        self.manifest.replace_item_at(index, item)
    }

    /// Appends an owned itemref after confirming that its manifest target exists.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ManifestItemMissing`] when `idref` has no manifest target.
    pub(crate) fn add_spine_itemref(&mut self, itemref: ItemRef) -> Result<()> {
        if !self.manifest.items().iter().any(|item| {
            item.id()
                .zip(itemref.idref())
                .is_some_and(|(id, idref)| manifest_ids_equal(id, idref))
        }) {
            return Err(PackageError::ManifestItemMissing {
                id: itemref.idref().map(ToString::to_string).unwrap_or_default(),
            });
        }
        self.spine.add_itemref(itemref);
        Ok(())
    }

    /// Removes the spine itemref at a zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] without mutation when out of range.
    pub(crate) fn remove_spine_itemref_at(&mut self, index: usize) -> Result<()> {
        self.spine.remove_itemref_at(index)
    }

    /// Replaces a spine itemref in place after resolving its manifest target.
    ///
    /// # Errors
    ///
    /// Returns a missing-manifest-target or out-of-range error. Failure is atomic.
    pub(crate) fn replace_spine_itemref_at(
        &mut self,
        index: usize,
        itemref: ItemRef,
    ) -> Result<()> {
        if !self.manifest.items().iter().any(|item| {
            item.id()
                .zip(itemref.idref())
                .is_some_and(|(id, idref)| manifest_ids_equal(id, idref))
        }) {
            return Err(PackageError::ManifestItemMissing {
                id: itemref.idref().map(ToString::to_string).unwrap_or_default(),
            });
        }
        self.spine.replace_itemref_at(index, itemref)
    }

    /// Moves a spine itemref between final zero-based positions.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] if either index is out of range.
    /// Failure leaves the package unchanged.
    pub(crate) fn move_spine_itemref(&mut self, from: usize, to: usize) -> Result<()> {
        self.spine.move_itemref(from, to)
    }

    /// Borrows this package's reading-order model.
    pub fn spine(&self) -> &Spine {
        &self.spine
    }
    /// Borrows the optional EPUB 2 guide.
    pub fn guide(&self) -> Option<&Guide> {
        self.guide.as_ref()
    }
    /// Borrows top-level collections in authored order.
    pub fn collections(&self) -> &[Collection] {
        self.collections.as_slice()
    }
    /// Iterates manifest declarations whose valid ID equals `id`.
    ///
    /// Parsed invalid IDs remain in the manifest but never match this query.
    pub fn manifest_items_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> std::result::Result<impl Iterator<Item = &'a ManifestItem> + 'a, InvalidManifestId> {
        let id = normalize_manifest_id(id)?;
        Ok(self.manifest.items().iter().filter(move |item| {
            item.id()
                .and_then(|item_id| normalize_manifest_id(item_id).ok())
                == Some(id)
        }))
    }

    pub(crate) fn manifest_item_by_id(&self, id: &str) -> Option<&ManifestItem> {
        let id = normalize_manifest_id(id).ok()?;
        let mut matches = self.manifest.items().iter().filter(|item| {
            item.id()
                .and_then(|item_id| normalize_manifest_id(item_id).ok())
                == Some(id)
        });
        let item = matches.next()?;
        matches.next().is_none().then_some(item)
    }

    pub(crate) fn nav_item(&self) -> Option<&ManifestItem> {
        self.selected_item(&self.resource_selections().1)
    }

    pub(crate) fn ncx_item(&self) -> Option<&ManifestItem> {
        self.selected_item(&self.resource_selections().2)
    }

    fn selected_item(&self, selection: &ResourceSelection) -> Option<&ManifestItem> {
        match selection {
            ResourceSelection::Selected {
                declaration: Some(declaration),
                ..
            } => self.manifest.items().get(declaration.index()),
            _ => None,
        }
    }

    /// Returns cover, EPUB NAV, and NCX selections without resolved resources.
    pub(crate) fn resource_selections(
        &self,
    ) -> (ResourceSelection, ResourceSelection, ResourceSelection) {
        use crate::resource::{ManifestOrdinal, SelectionSource};

        let property_candidates = |property| {
            self.manifest
                .items()
                .iter()
                .enumerate()
                .filter(|(_, item)| item.has_property(property))
                .map(|(index, _)| ManifestOrdinal::from_index(index))
                .collect::<Vec<_>>()
        };
        let select_candidates =
            |source, candidates: Vec<ManifestOrdinal>| match candidates.as_slice() {
                [] => ResourceSelection::Absent,
                [declaration] => ResourceSelection::Selected {
                    source,
                    declaration: Some(*declaration),
                    resource: None,
                },
                _ => ResourceSelection::Ambiguous { source, candidates },
            };
        let select_id = |source, authored_id: &str| {
            let candidates = normalize_manifest_id(authored_id)
                .ok()
                .map(|id| {
                    self.manifest
                        .items()
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| {
                            item.id()
                                .and_then(|value| normalize_manifest_id(value).ok())
                                == Some(id)
                        })
                        .map(|(index, _)| ManifestOrdinal::from_index(index))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if candidates.is_empty() {
                ResourceSelection::UnresolvedAuthoredId {
                    source,
                    authored_id: authored_id.to_string(),
                }
            } else {
                select_candidates(source, candidates)
            }
        };

        let epub_nav = select_candidates(
            SelectionSource::EpubNavProperty,
            property_candidates(KnownManifestProperty::Nav),
        );
        let cover_v3 = property_candidates(KnownManifestProperty::CoverImage);
        let cover = if cover_v3.is_empty() {
            self.metadata
                .opf2_cover_id()
                .map_or(ResourceSelection::Absent, |id| {
                    select_id(SelectionSource::Opf2CoverMetadata, id)
                })
        } else {
            select_candidates(SelectionSource::CoverImageProperty, cover_v3)
        };
        let ncx = self.spine.toc().map_or(ResourceSelection::Absent, |id| {
            select_id(SelectionSource::SpineToc, id)
        });
        (cover, epub_nav, ncx)
    }
}

#[derive(
    Debug,
    PartialEq,
    Eq,
    Clone,
    Copy,
    strum_macros::Display,
    strum_macros::EnumString,
    Hash,
    Default,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The recognized major version of an OPF package document.
///
/// Parsing accepts `2`/`2.0` and `3`/`3.0`; unknown source values are
/// represented as absent on [`Package`]. Display emits normalized dotted versions.
pub enum EpubVersion {
    /// EPUB 2.x, serialized as `2.0`.
    #[strum(to_string = "2.0", serialize = "2")]
    #[cfg_attr(feature = "serde", serde(rename = "2.0"))]
    Two,
    /// EPUB 3.x, serialized as `3.0` and used as the construction default.
    #[strum(to_string = "3.0", serialize = "3")]
    #[cfg_attr(feature = "serde", serde(rename = "3.0"))]
    #[default]
    Three,
}

fn required_package_string(value: impl AsRef<str>, field: PackageField) -> Result<EpubString> {
    EpubString::try_new(value).map_err(|_| PackageError::EmptyField { field })
}

fn required_media_type(value: impl AsRef<str>, field: PackageField) -> Result<MediaType> {
    EpubString::try_new(value)
        .map(MediaType::from)
        .map_err(|_| PackageError::EmptyField { field })
}

fn required_package_href(value: impl AsRef<str>, field: PackageField) -> Result<EpubHref> {
    EpubHref::try_new(value).map_err(|_| PackageError::EmptyField { field })
}

#[cfg(test)]
mod test {

    fn first_unrefined<'a, T>(
        metadata: &'a metadata::Metadata,
        project: impl Fn(&'a Meta) -> Option<T> + 'a,
    ) -> Option<T> {
        metadata
            .meta()
            .iter()
            .filter(|meta| meta.refines().is_none())
            .find_map(project)
    }
    use super::*;
    use crate::semantics::TextDirection;
    use std::str::FromStr;

    #[test]
    fn rendition_source_enums_use_canonical_spellings_and_case_insensitive_parsing() {
        macro_rules! assert_values {
            ($type:ty, [$(($variant:path, $spelling:literal)),+ $(,)?]) => {
                $(
                    assert_eq!($variant.to_string(), $spelling);
                    assert_eq!(
                        <$type>::from_str(&$spelling.to_ascii_uppercase()),
                        Ok($variant)
                    );
                )+
            };
        }

        assert_values!(
            RenditionLayout,
            [
                (RenditionLayout::Reflowable, "reflowable"),
                (RenditionLayout::PrePaginated, "pre-paginated"),
                (RenditionLayout::Roll, "roll"),
            ]
        );
        assert_values!(
            RenditionFlow,
            [
                (RenditionFlow::Auto, "auto"),
                (RenditionFlow::Paginated, "paginated"),
                (RenditionFlow::ScrolledContinuous, "scrolled-continuous"),
                (RenditionFlow::ScrolledDoc, "scrolled-doc"),
            ]
        );
        assert_values!(
            RenditionOrientation,
            [
                (RenditionOrientation::Auto, "auto"),
                (RenditionOrientation::Landscape, "landscape"),
                (RenditionOrientation::Portrait, "portrait"),
            ]
        );
        assert_values!(
            RenditionSpread,
            [
                (RenditionSpread::Auto, "auto"),
                (RenditionSpread::Both, "both"),
                (RenditionSpread::Landscape, "landscape"),
                (RenditionSpread::None, "none"),
                (RenditionSpread::Portrait, "portrait"),
            ]
        );
    }

    fn detached_package() -> Package {
        Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
                <manifest>
                    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
                    <item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/>
                </manifest>
                <spine><itemref idref="chapter"/></spine>
            </package>"#,
        )
        .unwrap()
    }

    fn detached_manifest_item(id: &str, href: &str) -> ManifestItem {
        ManifestItem::builder()
            .id(required_package_string(id, PackageField::ManifestItemId).unwrap())
            .href(required_package_href(href, PackageField::ManifestItemHref).unwrap())
            .media_type(
                required_media_type("application/xhtml+xml", PackageField::ManifestItemMediaType)
                    .unwrap(),
            )
            .build()
            .unwrap()
    }

    #[test]
    fn detached_package_spine_mutations_require_manifest_targets_atomically() {
        let mut package = detached_package();

        let before = package.clone();
        assert!(matches!(
            package.add_spine_itemref(ItemRef::new("missing").unwrap()),
            Err(PackageError::ManifestItemMissing { id }) if id == "missing"
        ));
        assert_eq!(package, before);

        let before = package.clone();
        assert!(matches!(
            package.replace_spine_itemref_at(0, ItemRef::new("missing").unwrap()),
            Err(PackageError::ManifestItemMissing { id }) if id == "missing"
        ));
        assert_eq!(package, before);
    }

    #[test]
    fn detached_package_manifest_mutations_reject_items_in_use_atomically() {
        let mut package = detached_package();

        let before = package.clone();
        assert!(matches!(
            package.remove_manifest_item_at(0),
            Err(PackageError::ManifestItemInUse { id }) if id == "chapter"
        ));
        assert_eq!(package, before);

        let before = package.clone();
        assert!(matches!(
            package.replace_manifest_item_at(
                0,
                detached_manifest_item("replacement", "replacement.xhtml"),
            ),
            Err(PackageError::ManifestItemInUse { id }) if id == "chapter"
        ));
        assert_eq!(package, before);
    }

    #[test]
    fn detached_package_allows_same_id_manifest_replacement_in_use() {
        let mut package = detached_package();
        let replacement = detached_manifest_item("chapter", "replacement.xhtml");

        package
            .replace_manifest_item_at(0, replacement.clone())
            .unwrap();

        assert_eq!(package.manifest_item_by_id("chapter"), Some(&replacement));
        assert_eq!(package.spine().itemrefs()[0].idref(), Some("chapter"));
    }

    #[test]
    fn metadata_relationship_lookup_reports_missing_targets() {
        let package = Package::parse(
            r##"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="missing">
    <metadata>
        <dc:title id="title">T</dc:title><dc:identifier id="other">id</dc:identifier><dc:language>en</dc:language>
        <meta property="title-type" refines="#absent">main</meta>
    </metadata>
    <manifest/><spine/>
</package>"##,
        )
        .unwrap();

        assert_eq!(
            package.unique_identifier_target(),
            MetadataIdLookup::Missing
        );
        assert_eq!(
            package.metadata().refinement_target("#absent"),
            MetadataIdLookup::Missing
        );
        assert_eq!(package.unique_identifier(), None);
    }

    #[test]
    fn metadata_relationship_lookup_returns_unique_source_target() {
        let package = Package::parse(
            r##"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
    <metadata>
        <dc:title id="title">T</dc:title><dc:identifier id="uid">book-id</dc:identifier><dc:language>en</dc:language>
        <meta property="title-type" refines="#title">main</meta>
    </metadata>
    <manifest/><spine/>
</package>"##,
        )
        .unwrap();

        assert_eq!(
            package.unique_identifier_target(),
            MetadataIdLookup::Unique(MetadataIdTarget::Element {
                kind: DcElement::Identifier,
                element: &package.metadata().elements(DcElement::Identifier)[0],
            })
        );
        assert_eq!(
            package.metadata().refinement_target("#title"),
            MetadataIdLookup::Unique(MetadataIdTarget::Element {
                kind: DcElement::Title,
                element: &package.metadata().elements(DcElement::Title)[0],
            })
        );
        assert_eq!(
            package.unique_identifier().map(EpubString::as_str),
            Some("book-id")
        );
    }

    #[test]
    fn metadata_relationship_lookup_reports_ambiguous_authored_ids() {
        let package = Package::parse(
            r##"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="duplicate">
    <metadata>
        <dc:title id="duplicate">T</dc:title>
        <dc:identifier id="duplicate">first</dc:identifier><dc:identifier id="duplicate">second</dc:identifier>
        <dc:language>en</dc:language>
        <meta property="title-type" refines="#duplicate">main</meta>
    </metadata>
    <manifest/><spine/>
</package>"##,
        )
        .unwrap();

        assert_eq!(
            package.unique_identifier_target(),
            MetadataIdLookup::Ambiguous
        );
        assert_eq!(
            package.metadata().refinement_target("#duplicate"),
            MetadataIdLookup::Ambiguous
        );
        assert_eq!(
            package.metadata().authored_id_targets("duplicate").count(),
            3
        );
        assert_eq!(package.unique_identifier(), None);
    }

    #[test]
    fn spine_page_spread_allows_pre_paginated_layout() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
    <metadata>
        <dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language>
        <meta property="rendition:layout">pre-paginated</meta>
    </metadata>
    <manifest><item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
    <spine><itemref idref="chap" properties="rendition:page-spread-center" /></spine>
</package>"#,
        )
        .unwrap();

        assert_eq!(
            first_unrefined(package.metadata(), Meta::rendition_layout),
            Some(RenditionLayout::PrePaginated)
        );
    }

    #[test]
    fn repeated_manifest_id_lookup_requires_one_match() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="same" href="one" media-type="text/plain"/>
                <item id="same" href="two" media-type="text/plain"/>
            </manifest></package>"#,
        )
        .unwrap();
        assert_eq!(package.manifest().items().len(), 2);
        assert_eq!(package.manifest_item_by_id("same"), None);
    }

    #[test]
    fn repeated_nav_item_lookup_requires_one_match() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
                <item id="one" href="one" media-type="application/xhtml+xml" properties="nav"/>
                <item id="two" href="two" media-type="application/xhtml+xml" properties="nav"/>
            </manifest></package>"#,
        )
        .unwrap();
        assert_eq!(package.nav_item(), None);
    }

    #[test]
    fn opf2_fields_remain_legacy_state_without_generated_ids_or_cover_properties() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf" version="2.0">
                <metadata>
                    <dc:identifier opf:scheme="ISBN">123</dc:identifier>
                    <dc:creator opf:role="aut" opf:file-as="Auteur, A">A Auteur</dc:creator>
                    <meta name="cover" content="cover"/>
                </metadata>
                <manifest><item id="cover" href="cover.jpg" media-type="image/jpeg"/></manifest>
            </package>"#,
        )
        .unwrap();

        let identifier = &package.metadata().elements(DcElement::Identifier)[0];
        assert_eq!(identifier.id(), None);
        assert_eq!(
            identifier.opf2_scheme().map(EpubString::as_str),
            Some("ISBN")
        );
        let creator = &package.metadata().elements(DcElement::Creator)[0];
        assert_eq!(creator.id(), None);
        assert_eq!(creator.opf2_role().map(EpubString::as_str), Some("aut"));
        assert_eq!(
            creator.opf2_file_as().map(EpubString::as_str),
            Some("Auteur, A")
        );
        let cover = package
            .manifest_items_by_id("cover")
            .unwrap()
            .next()
            .unwrap();
        assert!(!cover.has_property(KnownManifestProperty::CoverImage));
    }

    #[test]
    fn direction_vocabularies_are_owner_specific_and_round_trip() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" dir="auto">
                <metadata>
                    <dc:title dir="rtl">Title</dc:title>
                    <meta property="custom:value" dir="ltr">value</meta>
                </metadata>
                <manifest/>
                <spine page-progression-direction="default"/>
                <collection dir="auto"/>
            </package>"#,
        )
        .unwrap();

        assert_eq!(package.dir(), Some(TextDirection::Auto));
        assert_eq!(
            package.metadata().elements(DcElement::Title)[0].dir(),
            Some(TextDirection::Rtl)
        );
        assert_eq!(package.metadata().meta()[0].dir(), Some(TextDirection::Ltr));
        assert_eq!(
            package.spine().page_progression_direction(),
            Some(PageProgressionDirection::Default)
        );
        assert_eq!(package.collections()[0].dir(), Some(TextDirection::Auto));

        let normalized = package.to_normalized_xml().unwrap();
        assert!(normalized.contains("page-progression-direction=\"default\""));
        assert!(!normalized.contains("page-direction-progression"));
        assert_eq!(Package::parse(&normalized).unwrap(), package);
    }

    #[test]
    fn owner_invalid_direction_tokens_are_not_projected() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" dir="default">
                <metadata><dc:title dir="default">Title</dc:title><meta property="custom:value" dir="default">value</meta></metadata>
                <manifest/>
                <spine page-progression-direction="auto"/>
                <collection dir="default"/>
            </package>"#,
        )
        .unwrap();

        assert_eq!(package.dir(), None);
        assert_eq!(package.metadata().elements(DcElement::Title)[0].dir(), None);
        assert_eq!(package.metadata().meta()[0].dir(), None);
        assert_eq!(package.spine().page_progression_direction(), None);
        assert_eq!(package.collections()[0].dir(), None);
    }
}
