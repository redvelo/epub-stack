//! Read, inspect, and build OPF package documents.
//!
//! Start with [`Package::parse`] to inspect an existing package, or [`Package::new_minimal`] to
//! create the package model for a new publication. Use [`metadata`] for Dublin Core and OPF
//! metadata, [`manifest`] for resource declarations, [`spine`] for reading order, [`collection`]
//! for grouped resources, and [`legacy`] for EPUB 2 guide and metadata structures. Call
//! [`Package::to_normalized_xml`] when you want new OPF XML from the semantic model.
//!
//! String fields represented by [`EpubString`] trim leading and trailing Unicode whitespace and
//! reject an empty result. Parsing and normalized generation preserve selected authored values,
//! but do not provide a source-preserving XML round trip.

/// EPUB package collection models.
pub mod collection;
mod generate;
/// EPUB 2 package structures retained for inspection and migration.
pub mod legacy;
/// Package manifest models and property vocabulary tokens.
pub mod manifest;
/// Dublin Core and OPF metadata models.
pub mod metadata;
mod parse;
/// Reading-order models and spine property vocabulary tokens.
pub mod spine;

use crate::media_type::MediaType;
use crate::resource::EpubHref;
use crate::semantics::TextDirection;
use crate::string::EpubString;
use collection::Collection;
use legacy::Guide;
use manifest::{KnownManifestProperty, Manifest, ManifestItem};
use metadata::{
    Element, KnownMetaProperty, Meta, MetaPropertyToken, Metadata, MetadataIdLookup,
    MetadataIdTarget,
};
use spine::{ItemRef, Spine};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

#[cfg(test)]
use spine::PageProgressionDirection;

/// An error decoding XML bytes used by an OPF package document.
pub use crate::xml::XmlDecodeError as PackageXmlDecodeError;

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

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
/// A parsed deprecated package `rendition:viewport` source value.
///
/// EPUB 3.4 deprecates this package metadata control. The projection records positive, finite CSS
/// pixel dimensions authored as `width=x, height=y`; it does not define runtime viewport policy.
/// The original complete value remains available from [`metadata::Meta::content`].
pub struct RenditionViewport {
    width: Box<str>,
    height: Box<str>,
}

impl RenditionViewport {
    pub(crate) fn from_values(width: f64, height: f64) -> Self {
        Self {
            width: width.to_string().into_boxed_str(),
            height: height.to_string().into_boxed_str(),
        }
    }

    /// Returns the positive, finite width in CSS pixels.
    pub fn width(&self) -> f64 {
        self.width.parse().expect("validated viewport width")
    }

    /// Returns the positive, finite height in CSS pixels.
    pub fn height(&self) -> f64 {
        self.height.parse().expect("validated viewport height")
    }

    /// Borrows the canonical validated width spelling stored by this projection.
    pub fn width_source(&self) -> &str {
        &self.width
    }

    /// Borrows the canonical validated height spelling stored by this projection.
    pub fn height_source(&self) -> &str {
        &self.height
    }
}

#[derive(Debug, thiserror::Error)]
/// Failure parsing, generating, or atomically mutating an OPF package model.
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
        /// The name of the rejected field.
        field: &'static str,
    },
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
    /// No spine itemref has the requested `idref`.
    #[error("Spine itemref not found: {idref}")]
    SpineItemrefMissing {
        /// The requested manifest ID reference.
        idref: String,
    },
    /// A spine index is outside the itemref list.
    #[error("Spine itemref index not found: {index}")]
    SpineItemrefIndexMissing {
        /// The requested zero-based index.
        index: usize,
    },
    /// A metadata index is outside the selected node collection.
    #[error("Metadata {kind} index not found: {index}")]
    MetadataIndexMissing {
        /// The metadata local name or node kind.
        kind: String,
        /// The requested zero-based index within that kind.
        index: usize,
    },
    /// A metadata replacement has a different Dublin Core element kind.
    #[error("Metadata replacement kind mismatch: target {target}, replacement {replacement}")]
    MetadataKindMismatch {
        /// The local name of the target collection.
        target: String,
        /// The replacement element's local name.
        replacement: String,
    },
    /// An operation requires a non-empty spine.
    #[error("Spine must contain at least one itemref")]
    SpineEmpty,
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
    /// Reading package XML failed.
    #[error("IO error: {source}")]
    Io {
        #[from]
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// Generated XML bytes unexpectedly were not UTF-8.
    #[error("Generated package XML is not UTF-8: {source}")]
    Utf8 {
        #[from]
        /// The UTF-8 conversion failure and generated bytes.
        source: std::string::FromUtf8Error,
    },
    /// Formatting a package modification timestamp failed.
    #[error("Could not format package modification time: {source}")]
    TimeFormat {
        #[from]
        /// The timestamp formatting failure.
        source: time::error::Format,
    },
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
/// The semantic projection of an OPF package document.
///
/// The model owns all represented package data. Parsing preserves source order within modeled
/// repeated groups and authored vocabulary tokens and hrefs where exposed, but it does not retain
/// unknown XML, comments, attribute order, or general lexical formatting.
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
        let title = Element::new(required_package_string(title, "title")?);
        let identifier = Element::builder()
            .id(required_package_string("uid", "element id")?)
            .content(required_package_string(identifier, "identifier")?)
            .build();
        let language_elem = Element::new(required_package_string(&language, "language")?);
        metadata.add_title(title);
        metadata.add_identifier(identifier);
        metadata.add_language(language_elem);
        let modified = modified
            .to_offset(UtcOffset::UTC)
            .replace_nanosecond(0)
            .expect("zero is a valid nanosecond")
            .format(&Rfc3339)?;
        metadata.add_meta(Meta::new(
            MetaPropertyToken::known(KnownMetaProperty::Dctermsmodified),
            required_package_string(modified, "modified")?,
        ));

        let mut manifest = Manifest::new_empty();
        let nav_item = ManifestItem::builder()
            .id(required_package_string("nav", "manifest item id")?)
            .href(required_package_href("nav.xhtml", "manifest item href")?)
            .media_type(required_media_type(
                "application/xhtml+xml",
                "manifest item media-type",
            )?)
            .properties(vec![KnownManifestProperty::Nav.into()])
            .build();
        manifest.add_item(nav_item)?;

        Ok(Self {
            unique_identifier_id: EpubString::new("uid"),
            version: Some(EpubVersion::default()),
            xml_lang: Some(required_package_string(language, "package language")?),
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
                local_name: IDENTIFIER,
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
    pub fn metadata_mut(&mut self) -> &mut Metadata {
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
    pub fn add_manifest_item(&mut self, item: ManifestItem) -> Result<()> {
        self.manifest.add_item(item)
    }

    /// Removes every manifest item with `id` if no spine itemref references it.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ManifestItemInUse`] or [`PackageError::ManifestItemMissing`].
    /// Failure leaves the package unchanged.
    pub fn remove_manifest_item(&mut self, id: impl AsRef<str>) -> Result<()> {
        let id = id.as_ref();
        if self
            .spine
            .itemrefs()
            .iter()
            .any(|itemref| itemref.idref().is_some_and(|idref| idref == id))
        {
            return Err(PackageError::ManifestItemInUse { id: id.to_string() });
        }
        self.manifest.remove_item(id)
    }

    /// Replaces the first manifest item with `id` while preserving its list position.
    ///
    /// # Errors
    ///
    /// Returns an in-use, missing-item, duplicate-ID, or duplicate-href error. Failure leaves the
    /// package unchanged.
    pub fn replace_manifest_item(&mut self, id: impl AsRef<str>, item: ManifestItem) -> Result<()> {
        let id = id.as_ref();
        if item.id().is_some_and(|item_id| item_id != id)
            && self
                .spine
                .itemrefs()
                .iter()
                .any(|itemref| itemref.idref().is_some_and(|idref| idref == id))
        {
            return Err(PackageError::ManifestItemInUse { id: id.to_string() });
        }
        self.manifest.replace_item(id, item)
    }

    /// Appends an owned itemref after confirming that its manifest target exists.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ManifestItemMissing`] when `idref` has no manifest target.
    pub fn add_spine_itemref(&mut self, itemref: ItemRef) -> Result<()> {
        if !self
            .manifest
            .items()
            .iter()
            .any(|item| item.id().is_some() && item.id() == itemref.idref())
        {
            return Err(PackageError::ManifestItemMissing {
                id: itemref.idref().map(ToString::to_string).unwrap_or_default(),
            });
        }
        self.spine.add_itemref(itemref);
        Ok(())
    }

    /// Removes all spine itemrefs matching `idref`.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefMissing`] if none match. Failure leaves the package
    /// unchanged.
    pub fn remove_spine_itemref(&mut self, idref: impl AsRef<str>) -> Result<()> {
        self.spine.remove_itemref(idref)
    }

    /// Removes the spine itemref at a zero-based index.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] without mutation when out of range.
    pub fn remove_spine_itemref_at(&mut self, index: usize) -> Result<()> {
        self.spine.remove_itemref_at(index)
    }

    /// Replaces a spine itemref in place after resolving its manifest target.
    ///
    /// # Errors
    ///
    /// Returns a missing-manifest-target or out-of-range error. Failure is atomic.
    pub fn replace_spine_itemref_at(&mut self, index: usize, itemref: ItemRef) -> Result<()> {
        if !self
            .manifest
            .items()
            .iter()
            .any(|item| item.id().is_some() && item.id() == itemref.idref())
        {
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
    pub fn move_spine_itemref(&mut self, from: usize, to: usize) -> Result<()> {
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
    /// Returns the cover-image manifest item using EPUB 3 then EPUB 2 metadata semantics.
    ///
    /// EPUB 3's `cover-image` manifest property takes precedence over EPUB 2 cover metadata.
    pub fn cover_image_item(&self) -> Option<&ManifestItem> {
        let v3 = self
            .manifest
            .items()
            .iter()
            .find(|item| item.has_property(KnownManifestProperty::CoverImage));
        let cover_id = self.metadata.opf2_cover_id();
        let v2 = cover_id.and_then(|cover_id| self.manifest_item_by_id(cover_id));
        v3.or(v2)
    }
    /// Returns the unique manifest item carrying the `nav` property.
    ///
    /// Returns `None` for zero or multiple matches.
    pub fn nav_item(&self) -> Option<&ManifestItem> {
        let mut matches = self
            .manifest
            .items()
            .iter()
            .filter(|item| item.has_property(KnownManifestProperty::Nav));
        let item = matches.next()?;
        matches.next().is_none().then_some(item)
    }
    /// Returns the unique manifest item referenced by the spine's EPUB 2 `toc` attribute.
    pub fn ncx_item(&self) -> Option<&ManifestItem> {
        let ncx_id = self.spine.toc()?;
        self.manifest_item_by_id(ncx_id)
    }
    /// Returns the manifest item with `id` only when exactly one item matches.
    ///
    /// Duplicate authored IDs produce `None`.
    pub fn manifest_item_by_id(&self, id: impl AsRef<str>) -> Option<&ManifestItem> {
        let mut matches = self
            .manifest
            .items()
            .iter()
            .filter(|item| item.id().is_some_and(|item_id| item_id == id.as_ref()));
        let item = matches.next()?;
        matches.next().is_none().then_some(item)
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
/// The recognized major version of an OPF package document.
///
/// Parsing accepts the configured `2`/`2.0` and `3`/`3.0` spellings; unknown source values are
/// represented as absent on [`Package`]. Display emits normalized dotted versions.
pub enum EpubVersion {
    /// EPUB 2.x, serialized as `2.0`.
    #[strum(to_string = "2.0", serialize = "2")]
    Two,
    /// EPUB 3.x, serialized as `3.0` and used as the construction default.
    #[strum(to_string = "3.0", serialize = "3")]
    #[default]
    Three,
}

fn required_package_string(value: impl AsRef<str>, field: &'static str) -> Result<EpubString> {
    EpubString::try_new(value).map_err(|_| PackageError::EmptyField { field })
}

fn required_media_type(value: impl AsRef<str>, field: &'static str) -> Result<MediaType> {
    EpubString::try_new(value)
        .map(MediaType::from)
        .map_err(|_| PackageError::EmptyField { field })
}

fn required_package_href(value: impl AsRef<str>, field: &'static str) -> Result<EpubHref> {
    EpubHref::try_new(value).map_err(|_| PackageError::EmptyField { field })
}

#[cfg(test)]
mod test {
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
            .id(required_package_string(id, "manifest item id").unwrap())
            .href(required_package_href(href, "manifest item href").unwrap())
            .media_type(
                required_media_type("application/xhtml+xml", "manifest item media-type").unwrap(),
            )
            .build()
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
            package.remove_manifest_item("chapter"),
            Err(PackageError::ManifestItemInUse { id }) if id == "chapter"
        ));
        assert_eq!(package, before);

        let before = package.clone();
        assert!(matches!(
            package.replace_manifest_item(
                "chapter",
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
            .replace_manifest_item("chapter", replacement.clone())
            .unwrap();

        assert_eq!(package.manifest_item_by_id("chapter"), Some(&replacement));
        assert_eq!(
            package.spine().itemrefs()[0]
                .idref()
                .map(EpubString::as_str),
            Some("chapter")
        );
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
                local_name: IDENTIFIER,
                element: &package.metadata().identifier()[0],
            })
        );
        assert_eq!(
            package.metadata().refinement_target("#title"),
            MetadataIdLookup::Unique(MetadataIdTarget::Element {
                local_name: TITLE,
                element: &package.metadata().title()[0],
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
            package.metadata().rendition_layout(),
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

        let identifier = &package.metadata().identifier()[0];
        assert_eq!(identifier.id(), None);
        assert_eq!(
            identifier.opf2_scheme().map(EpubString::as_str),
            Some("ISBN")
        );
        let creator = &package.metadata().creator()[0];
        assert_eq!(creator.id(), None);
        assert_eq!(creator.opf2_role().map(EpubString::as_str), Some("aut"));
        assert_eq!(
            creator.opf2_file_as().map(EpubString::as_str),
            Some("Auteur, A")
        );
        let cover = package.cover_image_item().unwrap();
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
            package.metadata().title()[0].dir(),
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
        assert_eq!(package.metadata().title()[0].dir(), None);
        assert_eq!(package.metadata().meta()[0].dir(), None);
        assert_eq!(package.spine().page_progression_direction(), None);
        assert_eq!(package.collections()[0].dir(), None);
    }
}
