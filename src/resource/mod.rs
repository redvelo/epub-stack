//! Find, identify, and resolve resources used by an EPUB publication.
//!
//! Use [`ResourceIndex`] to browse package declarations, local publication files, and reading
//! order. [`EpubPath`] identifies a local resource, while the href APIs resolve relative links
//! found in EPUB documents.

/// Serializable resource topology detached from borrowed graph views.
pub mod facts;
/// Storage adapters and bounded inventories used when opening a publication.
pub mod provider;

/// Canonical authored media type representation.
pub use crate::media_type::MediaType;
/// A live resource handle obtained from a publication.
pub use crate::publication::Resource;

use crate::package::{
    Package, PackageSelection, RenditionFlow, RenditionLayout, RenditionOrientation,
    RenditionSpread,
    manifest::{KnownManifestProperty, ManifestPropertyToken},
    metadata::{KnownMetaProperty, Meta},
    normalize_manifest_id,
    spine::{
        ItemRef, KnownSpineProperty, Linear, PageProgressionDirection, PageSpread,
        SpinePropertyToken,
    },
};
use crate::string::EpubString;
use provider::{ProviderReadError, ResourceProviderIndex};
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
/// Failure to select exactly one resource or manifest declaration.
pub enum ResourceLookupError {
    /// A manifest-ID query received invalid XML `NCName` syntax.
    #[error(transparent)]
    InvalidManifestId(#[from] crate::package::InvalidManifestId),
    /// No declaration has the requested valid manifest ID.
    #[error("manifest ID did not resolve: {id}")]
    ManifestIdNotFound {
        /// Valid normalized manifest ID that had no match.
        id: String,
    },
    /// More than one declaration has the requested valid manifest ID.
    #[error("manifest ID resolved ambiguously: {id}")]
    AmbiguousManifestId {
        /// Valid normalized manifest ID.
        id: String,
        /// Matching declaration positions.
        candidates: Vec<ManifestOrdinal>,
    },
    /// No indexed value matched the selector.
    #[error("resource selector did not resolve: {0:?}")]
    NotFound(
        /// Selector that had no match.
        ResourceSelector,
    ),
    /// More than one declaration or resource matched a selector requiring uniqueness.
    #[error("resource selector resolved ambiguously: {selector:?}")]
    Ambiguous {
        /// Selector that produced multiple matches.
        selector: ResourceSelector,
        /// Matching index identities.
        candidates: Vec<ResourceLookupCandidate>,
    },
    /// A uniquely selected manifest declaration has no resolved resource target.
    #[error("manifest declaration does not resolve to a resource: {0:?}")]
    UnresolvedDeclaration(
        /// Unique declaration whose href did not resolve.
        ManifestOrdinal,
    ),
}

#[derive(Debug, thiserror::Error)]
/// Failure to read bytes or text for a selected resource.
pub enum ResourceReadError {
    /// The resource address does not identify provider-local bytes.
    #[error("resource is not local: {address:?}")]
    NonLocal {
        /// Non-local address that was selected.
        address: ResourceAddress,
    },
    /// The provider's stable view does not contain the indexed local path.
    #[error("resource is missing from the provider: {path}")]
    Missing {
        /// Canonical local path that was requested.
        path: EpubPath,
    },
    /// Filesystem-style I/O failed while reading local bytes.
    #[error("resource read failed for {path}: {source}")]
    Io {
        /// Canonical local path that was requested.
        path: EpubPath,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// A non-I/O provider backend failed while reading local bytes.
    #[error("resource provider backend failed while reading {path}: {source}")]
    Backend {
        /// Canonical local path that was requested.
        path: EpubPath,
        /// Backend-specific failure.
        source: Box<dyn std::error::Error>,
    },
    /// The complete resource bytes were not valid UTF-8.
    #[error("resource is not valid UTF-8: {path}")]
    InvalidUtf8 {
        /// Canonical local path that was read.
        path: EpubPath,
        /// UTF-8 conversion failure containing the original bytes.
        source: std::string::FromUtf8Error,
    },
}

impl From<ProviderReadError> for ResourceReadError {
    fn from(error: ProviderReadError) -> Self {
        match error {
            ProviderReadError::MissingResource { path } => Self::Missing { path },
            ProviderReadError::IoPath { path, source } => Self::Io { path, source },
            ProviderReadError::Backend { path, source } => Self::Backend { path, source },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// A canonical provider-relative EPUB resource path.
///
/// Identity is exact UTF-8 path spelling. Canonical paths are non-empty and relative, use
/// `/`, contain no empty, dot, query, fragment, control, or scheme-like first segments, and
/// are never silently normalized or repaired.
pub struct EpubPath(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// Reason a path cannot serve as canonical EPUB resource identity.
pub enum EpubPathError {
    /// The platform path cannot be represented as UTF-8.
    #[error("EPUB path is not valid UTF-8")]
    NonUtf8,
    /// The path has no bytes.
    #[error("EPUB path is empty")]
    Empty,
    /// The path starts at a filesystem or URL root.
    #[error("EPUB path must be relative")]
    Absolute,
    /// The path uses a backslash rather than `/`.
    #[error("EPUB path contains a backslash")]
    Backslash,
    /// The path includes a URL query or fragment delimiter.
    #[error("EPUB path contains a query or fragment")]
    QueryOrFragment,
    /// The path contains a leading, trailing, or repeated `/` segment.
    #[error("EPUB path contains an empty segment")]
    EmptySegment,
    /// The path contains `.` or `..` as a segment.
    #[error("EPUB path contains a dot segment")]
    DotSegment,
    /// The first segment contains `:` and could be interpreted as a URI scheme.
    #[error("EPUB path has a scheme-like first segment")]
    SchemeLikeFirstSegment,
    /// The path contains a Unicode control character.
    #[error("EPUB path contains a control character")]
    ControlCharacter,
}

impl EpubPath {
    /// Creates a canonical EPUB path from a platform path.
    ///
    /// This validates exact spelling and does not normalize separators or dot segments.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, EpubPathError> {
        let value = path.as_ref().to_str().ok_or(EpubPathError::NonUtf8)?;
        Self::parse(value)
    }

    /// Parses a canonical EPUB path from UTF-8 text without repairing it.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, EpubPathError> {
        let value = value.as_ref();
        validate_epub_path(value)?;
        Ok(Self(value.to_string()))
    }

    /// Returns the exact canonical UTF-8 path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Borrows the canonical value as a platform [`Path`].
    pub fn as_path(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl FromStr for EpubPath {
    type Err = EpubPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl AsRef<Path> for EpubPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for EpubPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<EpubPath> for PathBuf {
    fn from(value: EpubPath) -> Self {
        PathBuf::from(value.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// Exact authored href text, including empty or malformed values.
///
/// This fidelity type performs no trimming, validation, normalization, or repair.
pub struct AuthoredHref(String);

impl AuthoredHref {
    /// Preserves an authored href exactly as supplied.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact authored text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns a syntactically valid non-empty href, or `None` without repairing the text.
    pub fn to_epub_href(&self) -> Option<EpubHref> {
        EpubHref::try_new(self.as_str()).ok()
    }
}

impl fmt::Display for AuthoredHref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// Reason authored text cannot be represented as a syntactically valid EPUB href.
pub enum EpubHrefError {
    /// The href is empty.
    #[error("EPUB href is empty")]
    Empty,
    /// The href contains forbidden whitespace, controls, backslashes, or malformed escapes.
    #[error("EPUB href has invalid syntax")]
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// A non-empty href with checked lexical syntax.
///
/// This type validates but does not resolve, normalize, percent-decode, or establish resource
/// existence. Use [`ResourceIndex::resolve_href_from`] for source-relative resolution.
pub struct EpubHref(String);

impl EpubHref {
    /// Validates exact href text without trimming or repairing it.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, EpubHrefError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(EpubHrefError::Empty);
        }
        if !valid_href_syntax(value) {
            return Err(EpubHrefError::Invalid);
        }
        Ok(Self(value.to_string()))
    }

    /// Returns the exact validated href text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<EpubHref> for AuthoredHref {
    fn from(value: EpubHref) -> Self {
        Self(value.0)
    }
}

impl From<&EpubHref> for AuthoredHref {
    fn from(value: &EpubHref) -> Self {
        Self(value.as_str().to_string())
    }
}

impl fmt::Display for EpubHref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(tag = "kind", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Resolved identity class for a declared or provider resource.
pub enum ResourceAddress {
    /// A canonical path in the publication provider.
    Local(
        /// Canonical provider-relative path.
        EpubPath,
    ),
    /// An HTTP(S) or scheme-relative URL.
    Remote(
        /// Exact remote URL text excluding any separately resolved fragment.
        String,
    ),
    /// A `data:` URL retained as authored.
    Data(
        /// Exact authored data URL.
        String,
    ),
    /// A syntactically valid URL using another scheme.
    External(
        /// Exact authored URL using another scheme.
        String,
    ),
    /// Authored text that could not be resolved as an address.
    Invalid(
        /// Exact authored text that could not be resolved.
        String,
    ),
}

impl ResourceAddress {
    /// Returns the provider path for a local address.
    pub fn local_path(&self) -> Option<&EpubPath> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_) | Self::Data(_) | Self::External(_) | Self::Invalid(_) => None,
        }
    }

    /// Returns the URL for an HTTP(S) or scheme-relative remote address.
    pub fn remote_url(&self) -> Option<&str> {
        match self {
            Self::Local(_) | Self::Data(_) | Self::External(_) | Self::Invalid(_) => None,
            Self::Remote(url) => Some(url),
        }
    }

    /// Returns the address text suitable for display or source inspection.
    pub fn display_value(&self) -> &str {
        match self {
            Self::Local(path) => path.as_str(),
            Self::Remote(value)
            | Self::Data(value)
            | Self::External(value)
            | Self::Invalid(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ResourceRow(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ManifestRow(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ReadingOrderRow(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("row does not identify an entry in this resource snapshot")]
pub(crate) struct IndexRowError;

/// A snapshot-local ordinal is outside its corresponding collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("ordinal is outside this snapshot")]
pub struct OrdinalOutOfBounds;

/// The topology collection that cannot be represented by 32-bit ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceTopologyKind {
    /// Distinct physical resources.
    Resources,
    /// Manifest declarations.
    ManifestDeclarations,
    /// Reading-order occurrences.
    ReadingOrderOccurrences,
}

/// A resource topology has an index that cannot be represented by its public ordinal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resource topology {kind:?} exceeds 32-bit ordinals")]
pub struct ResourceIndexError {
    kind: ResourceTopologyKind,
}

impl ResourceIndexError {
    /// Returns the collection that exceeded the ordinal range.
    pub fn kind(self) -> ResourceTopologyKind {
        self.kind
    }
}

macro_rules! ordinal {
    ($name:ident, $docs:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
        #[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
        #[doc = $docs]
        pub struct $name(pub u32);

        impl $name {
            pub(crate) fn from_index(index: usize) -> Self {
                Self(u32::try_from(index).expect("resource topology count was checked"))
            }

            pub(crate) fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

ordinal!(
    ResourceOrdinal,
    "A zero-based resource position in one snapshot."
);
ordinal!(
    ManifestOrdinal,
    "A zero-based manifest declaration position in one snapshot."
);

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ManifestOrdinal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        <u32 as serde::Deserialize>::deserialize(deserializer).map(Self)
    }
}

ordinal!(
    ReadingOrderOrdinal,
    "A zero-based reading-order occurrence position in one snapshot."
);

impl From<ResourceRow> for ResourceOrdinal {
    fn from(row: ResourceRow) -> Self {
        Self::from_index(row.0)
    }
}

impl From<ResourceOrdinal> for ResourceRow {
    fn from(ordinal: ResourceOrdinal) -> Self {
        Self(ordinal.index())
    }
}

impl From<ManifestRow> for ManifestOrdinal {
    fn from(row: ManifestRow) -> Self {
        Self::from_index(row.0)
    }
}

impl From<ManifestOrdinal> for ManifestRow {
    fn from(ordinal: ManifestOrdinal) -> Self {
        Self(ordinal.index())
    }
}

impl From<ReadingOrderRow> for ReadingOrderOrdinal {
    fn from(row: ReadingOrderRow) -> Self {
        Self::from_index(row.0)
    }
}

impl From<ReadingOrderOrdinal> for ReadingOrderRow {
    fn from(ordinal: ReadingOrderOrdinal) -> Self {
        Self(ordinal.index())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Exact authored manifest IDREF text, including malformed or unresolved values.
pub struct AuthoredIdRef(String);

impl AuthoredIdRef {
    /// Preserves authored IDREF text exactly as supplied.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Returns the exact authored text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Authored state of a manifest item's `id` attribute.
pub enum ManifestIdValue {
    /// A present ID accepted as an XML Schema `ID`.
    Valid(
        /// Validated identifier.
        String,
    ),
    /// No `id` attribute was present.
    Missing,
    /// A present ID was preserved but did not satisfy XML Schema `ID` syntax.
    Invalid(
        /// Exact malformed authored ID.
        String,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolution state of one manifest declaration's `href`.
pub(crate) enum DeclarationTargetRow {
    /// The href resolved to canonical resource identity.
    Resource(
        /// Resolved resource identity.
        ResourceRow,
    ),
    /// The declaration had no `href` attribute.
    MissingHref,
    /// The exact href was present but could not resolve to an accepted address.
    InvalidHref(
        /// Exact authored href that failed resolution.
        AuthoredHref,
    ),
}

/// Resolution state of one manifest declaration in the current snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationTarget<'a> {
    /// The href resolved to a physical resource.
    Resource(ResourceOrdinal),
    /// The declaration had no `href` attribute.
    MissingHref,
    /// The authored href could not resolve to an accepted address.
    InvalidHref(&'a AuthoredHref),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One authored manifest item, kept distinct from its resolved resource.
///
/// Multiple declarations may resolve to the same physical resource while retaining their own
/// IDs, href spelling, media types, properties, and relationships.
struct ManifestDeclarationRow {
    id: ManifestIdValue,
    href: Option<AuthoredHref>,
    target: DeclarationTargetRow,
    media_type: Option<MediaType>,
    properties: Vec<ManifestPropertyToken>,
    fallback: Option<AuthoredIdRef>,
    media_overlay: Option<AuthoredIdRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestIdrefResolution<'a> {
    Invalid,
    Missing,
    Unique(ManifestRow),
    Ambiguous(&'a [ManifestRow]),
}

impl ManifestDeclarationRow {
    pub(crate) fn target_row(&self) -> &DeclarationTargetRow {
        &self.target
    }
    /// Returns the authored ID state.
    pub(crate) fn id(&self) -> &ManifestIdValue {
        &self.id
    }
    /// Returns the exact authored href when the attribute was present.
    pub(crate) fn href(&self) -> Option<&AuthoredHref> {
        self.href.as_ref()
    }
    /// Returns the declaration's resource-resolution state.
    pub(crate) fn target(&self) -> DeclarationTarget<'_> {
        match &self.target {
            DeclarationTargetRow::Resource(row) => {
                DeclarationTarget::Resource(ResourceOrdinal::from_index(row.0))
            }
            DeclarationTargetRow::MissingHref => DeclarationTarget::MissingHref,
            DeclarationTargetRow::InvalidHref(href) => DeclarationTarget::InvalidHref(href),
        }
    }
    /// Returns the raw-preserving media type declaration when present.
    ///
    /// A returned [`MediaType`] may contain malformed MIME syntax; use
    /// [`MediaType::is_valid`] before relying on parsed MIME facts.
    pub(crate) fn media_type(&self) -> Option<&MediaType> {
        self.media_type.as_ref()
    }
    /// Returns manifest property tokens in authored order.
    pub(crate) fn properties(&self) -> &[ManifestPropertyToken] {
        &self.properties
    }
    /// Returns the exact authored fallback IDREF when present.
    pub(crate) fn fallback(&self) -> Option<&AuthoredIdRef> {
        self.fallback.as_ref()
    }
    /// Returns the exact authored media-overlay IDREF when present.
    pub(crate) fn media_overlay(&self) -> Option<&AuthoredIdRef> {
        self.media_overlay.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Whether a resolved resource address has corresponding provider bytes.
pub enum ProviderPresence {
    /// The local path appeared in the complete provider index.
    Present,
    /// The local path did not appear in the complete provider index.
    Missing,
    /// Provider membership does not apply to a non-local address.
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Path and size metadata available without reading resource content.
///
/// This does not contain detected format, fingerprints, or content inspection.
pub struct ResourceMetadata {
    size_bytes: Option<u64>,
    file_extension: Option<String>,
}

impl ResourceMetadata {
    /// Returns the provider-reported logical byte length, when known.
    pub fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }
    /// Returns the lowercased local path extension, when present and UTF-8.
    pub fn file_extension(&self) -> Option<&str> {
        self.file_extension.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One resolved resource identity in a [`ResourceIndex`].
///
/// Provider-only resources have no declarations. Duplicate manifest declarations at the
/// same address share this record, and declaration-derived predicates mean that at least one
/// associated declaration supplied that classification.
struct ResourceRecordRow {
    address: ResourceAddress,
    declarations: Vec<ManifestRow>,
    presence: ProviderPresence,
    metadata: ResourceMetadata,
    has_xhtml: bool,
    has_stylesheet: bool,
    has_svg: bool,
    scripted: bool,
}

impl ResourceRecordRow {
    pub(crate) fn declaration_rows(&self) -> &[ManifestRow] {
        &self.declarations
    }
    /// Returns this resource's position in the current snapshot.
    /// Returns the canonical resolved address.
    pub(crate) fn address(&self) -> &ResourceAddress {
        &self.address
    }
    /// Returns the provider path when this is local.
    pub(crate) fn local_path(&self) -> Option<&EpubPath> {
        self.address.local_path()
    }
    /// Returns the URL when this is an HTTP(S) or scheme-relative remote resource.
    pub(crate) fn remote_url(&self) -> Option<&str> {
        self.address.remote_url()
    }
    /// Returns every manifest declaration resolving to this resource, in manifest order.
    /// Returns provider membership for this address.
    pub(crate) fn presence(&self) -> ProviderPresence {
        self.presence
    }
    /// Returns path and provider metadata available without reading content.
    pub(crate) fn metadata(&self) -> &ResourceMetadata {
        &self.metadata
    }
    /// Reports whether any declaration has valid XHTML MIME essence.
    pub(crate) fn has_xhtml_declaration(&self) -> bool {
        self.has_xhtml
    }
    /// Reports whether any declaration has valid CSS MIME essence.
    pub(crate) fn has_stylesheet_declaration(&self) -> bool {
        self.has_stylesheet
    }
    /// Reports whether any declaration has valid SVG MIME essence.
    pub(crate) fn has_svg_declaration(&self) -> bool {
        self.has_svg
    }
    /// Reports whether any declaration has the manifest `scripted` property.
    pub(crate) fn is_scripted(&self) -> bool {
        self.scripted
    }
    /// Reports whether at least one manifest declaration resolves to this resource.
    pub(crate) fn is_manifest_resource(&self) -> bool {
        !self.declarations.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolution state of one reading-order occurrence.
pub(crate) enum ReadingOrderTargetRow {
    /// The IDREF uniquely selected a declaration.
    Declaration {
        /// Selected manifest declaration.
        declaration: ManifestRow,
        /// Its resolved resource, absent when that declaration has no usable href.
        resource: Option<ResourceRow>,
    },
    /// The spine itemref had no `idref` attribute.
    MissingIdref,
    /// The authored IDREF matched no valid manifest ID.
    MissingManifestId,
    /// The authored IDREF matched multiple manifest declarations.
    AmbiguousManifestId {
        /// Matching declarations in manifest order.
        candidates: Vec<ManifestRow>,
    },
}

/// Resolution state of one reading-order occurrence in the current snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingOrderTarget {
    /// The IDREF uniquely selected a declaration.
    Declaration {
        /// Selected declaration position.
        declaration: ManifestOrdinal,
        /// Resolved physical resource, when the declaration has a usable href.
        resource: Option<ResourceOrdinal>,
    },
    /// The spine itemref had no `idref` attribute.
    MissingIdref,
    /// The authored IDREF matched no valid manifest ID.
    MissingManifestId,
    /// The authored IDREF matched multiple manifest declarations.
    AmbiguousManifestId {
        /// Matching declaration positions in manifest order.
        candidates: Vec<ManifestOrdinal>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The authored source of a rendition presentation candidate.
pub enum RenditionValueSource {
    /// The candidate came from an unrefined package metadata `meta` element.
    PackageMetadata,
    /// The candidate came from a spine `itemref` property token.
    ItemRefProperty,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// One authored rendition value and its optional recognized semantic projection.
///
/// Package metadata can omit content, so [`Self::authored_value`] and [`Self::value`] are
/// independent. Itemref candidates retain the complete property token spelling as their authored
/// value.
pub struct RenditionCandidate<T> {
    source: RenditionValueSource,
    authored_value: Option<EpubString>,
    value: Option<T>,
}

impl<T> RenditionCandidate<T> {
    /// Returns where this candidate was authored.
    pub fn source(&self) -> RenditionValueSource {
        self.source
    }

    /// Borrows the authored metadata content or complete itemref property token.
    pub fn authored_value(&self) -> Option<&EpubString> {
        self.authored_value.as_ref()
    }

    /// Borrows the recognized typed value, if the authored value was recognized.
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(tag = "state", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Authored state of one rendition presentation setting.
///
/// Candidate count is retained without deduplication: repeated declarations are ambiguous even
/// when they project to the same typed value. No reading-system defaults are synthesized.
pub enum RenditionSetting<T> {
    /// No applicable itemref property or package metadata was authored.
    Unspecified,
    /// Exactly one applicable candidate was authored.
    Specified(
        /// The unique authored candidate.
        RenditionCandidate<T>,
    ),
    /// More than one applicable candidate was authored, in source order.
    Ambiguous(
        /// All authored candidates responsible for the ambiguity.
        Vec<RenditionCandidate<T>>,
    ),
}

impl<T> RenditionSetting<T> {
    /// Returns the recognized value only when exactly one candidate was authored and recognized.
    pub fn value(&self) -> Option<&T> {
        self.candidate().and_then(RenditionCandidate::value)
    }

    /// Returns the unique candidate, or `None` when unspecified or ambiguous.
    pub fn candidate(&self) -> Option<&RenditionCandidate<T>> {
        match self {
            Self::Specified(candidate) => Some(candidate),
            Self::Unspecified | Self::Ambiguous(_) => None,
        }
    }

    /// Borrows all authored candidates in source order.
    pub fn candidates(&self) -> &[RenditionCandidate<T>] {
        match self {
            Self::Unspecified => &[],
            Self::Specified(candidate) => std::slice::from_ref(candidate),
            Self::Ambiguous(candidates) => candidates,
        }
    }

    /// Reports whether no applicable source value was authored.
    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    fn from_candidates(candidates: Vec<RenditionCandidate<T>>) -> Self {
        match candidates.len() {
            0 => Self::Unspecified,
            1 => Self::Specified(candidates.into_iter().next().expect("one candidate")),
            _ => Self::Ambiguous(candidates),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Occurrence-owned authored presentation state for one reading-order entry.
///
/// Rendition settings retain provenance, ambiguity, malformed source values, and itemref
/// precedence. Page progression is copied from the spine and preserves an authored `default`;
/// absent values and reading-system defaults remain absent.
pub struct ReadingOrderPresentation {
    layout: RenditionSetting<RenditionLayout>,
    flow: RenditionSetting<RenditionFlow>,
    orientation: RenditionSetting<RenditionOrientation>,
    spread: RenditionSetting<RenditionSpread>,
    page_spread: RenditionSetting<PageSpread>,
    page_progression_direction: Option<PageProgressionDirection>,
}

impl ReadingOrderPresentation {
    /// Returns the authored rendition layout state.
    pub fn layout(&self) -> &RenditionSetting<RenditionLayout> {
        &self.layout
    }

    /// Returns the authored historical rendition flow state.
    pub fn flow(&self) -> &RenditionSetting<RenditionFlow> {
        &self.flow
    }

    /// Returns the authored historical rendition orientation state.
    pub fn orientation(&self) -> &RenditionSetting<RenditionOrientation> {
        &self.orientation
    }

    /// Returns the authored historical rendition spread state.
    pub fn spread(&self) -> &RenditionSetting<RenditionSpread> {
        &self.spread
    }

    /// Returns the itemref-only synthetic page-spread placement state.
    pub fn page_spread(&self) -> &RenditionSetting<PageSpread> {
        &self.page_spread
    }

    /// Returns the spine-wide authored page progression direction, preserving `default`.
    pub fn page_progression_direction(&self) -> Option<PageProgressionDirection> {
        self.page_progression_direction
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One occurrence in authored spine order.
///
/// Occurrences remain distinct even when they repeat the same IDREF or resource.
struct ReadingOrderEntryRow {
    index: usize,
    idref: Option<AuthoredIdRef>,
    target: ReadingOrderTargetRow,
    linear: Linear,
    properties: Vec<SpinePropertyToken>,
    presentation: ReadingOrderPresentation,
}

impl ReadingOrderEntryRow {
    pub(crate) fn target_row(&self) -> &ReadingOrderTargetRow {
        &self.target
    }
    /// Returns this occurrence's position in the current snapshot.
    /// Returns the zero-based authored spine position.
    pub(crate) fn index(&self) -> usize {
        self.index
    }
    /// Returns the exact authored IDREF when present.
    pub(crate) fn idref(&self) -> Option<&AuthoredIdRef> {
        self.idref.as_ref()
    }
    /// Returns the occurrence's declaration/resource resolution state.
    pub(crate) fn target(&self) -> ReadingOrderTarget {
        match &self.target {
            ReadingOrderTargetRow::Declaration {
                declaration,
                resource,
            } => ReadingOrderTarget::Declaration {
                declaration: ManifestOrdinal::from_index(declaration.0),
                resource: resource.map(|row| ResourceOrdinal::from_index(row.0)),
            },
            ReadingOrderTargetRow::MissingIdref => ReadingOrderTarget::MissingIdref,
            ReadingOrderTargetRow::MissingManifestId => ReadingOrderTarget::MissingManifestId,
            ReadingOrderTargetRow::AmbiguousManifestId { candidates } => {
                ReadingOrderTarget::AmbiguousManifestId {
                    candidates: candidates
                        .iter()
                        .map(|row| ManifestOrdinal::from_index(row.0))
                        .collect(),
                }
            }
        }
    }
    /// Returns effective linearity; missing or malformed source defaults to [`Linear::Yes`].
    pub(crate) fn linear(&self) -> Linear {
        self.linear
    }
    /// Returns itemref property tokens in authored order.
    pub(crate) fn properties(&self) -> &[SpinePropertyToken] {
        &self.properties
    }
    /// Returns this occurrence's owned presentation snapshot.
    pub(crate) fn presentation(&self) -> &ReadingOrderPresentation {
        &self.presentation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An identity included in an ambiguous resource lookup result.
pub enum ResourceLookupCandidate {
    /// A resolved resource identity.
    Resource(
        /// Candidate resource identity.
        ResourceOrdinal,
    ),
    /// A manifest declaration identity.
    Manifest(
        /// Candidate manifest declaration identity.
        ManifestOrdinal,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionRows {
    Absent,
    Selected {
        source: facts::SelectionSource,
        declaration: Option<ManifestRow>,
        resource: Option<ResourceRow>,
    },
    UnresolvedAuthoredId {
        source: facts::SelectionSource,
        authored_id: String,
    },
    Ambiguous {
        source: facts::SelectionSource,
        candidates: Vec<ManifestRow>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceSelectionRows {
    package: SelectionRows,
    cover: SelectionRows,
    epub_nav: SelectionRows,
    ncx: SelectionRows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// The resource inventory and lookup table for one publication state.
///
/// Manifest declarations, resolved resources, and reading-order occurrences have separate
/// snapshot-local ordinals. Ordinals are topology positions without a runtime generation token
/// and therefore must not be mixed between snapshots. Construction uses complete provider
/// enumeration and performs no broad content
/// parsing. Use [`crate::Epub::analyze`] when content-derived facts are needed.
pub struct ResourceIndex {
    declarations: Vec<ManifestDeclarationRow>,
    resources: Vec<ResourceRecordRow>,
    reading_order: Vec<ReadingOrderEntryRow>,
    by_id: HashMap<String, Vec<ManifestRow>>,
    by_address: HashMap<ResourceAddress, ResourceRow>,
    package: ResourceRow,
    selections: ResourceSelectionRows,
}

/// A borrowed physical resource in one resource-index snapshot.
#[derive(Clone, Copy)]
pub struct ResourceRef<'a> {
    index: &'a ResourceIndex,
    row: ResourceRow,
}

impl std::fmt::Debug for ResourceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceRef")
            .field("ordinal", &self.ordinal())
            .field("address", self.address())
            .finish()
    }
}

impl PartialEq for ResourceRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.row == other.row
    }
}

impl Eq for ResourceRef<'_> {}

impl<'a> ResourceRef<'a> {
    pub(crate) fn key(self) -> ResourceRow {
        self.row
    }
    pub(crate) fn row(self) -> ResourceRow {
        self.row
    }
    fn record(self) -> &'a ResourceRecordRow {
        &self.index.resources[self.row.0]
    }

    /// Returns this resource's position in the snapshot.
    pub fn ordinal(self) -> ResourceOrdinal {
        self.row.into()
    }

    /// Returns the canonical resolved address.
    pub fn address(self) -> &'a ResourceAddress {
        self.record().address()
    }

    /// Returns the provider path when this resource is local.
    pub fn local_path(self) -> Option<&'a EpubPath> {
        self.record().local_path()
    }

    /// Returns the URL when this is an HTTP(S) or scheme-relative resource.
    pub fn remote_url(self) -> Option<&'a str> {
        self.record().remote_url()
    }

    /// Iterates manifest declarations resolving to this resource in manifest order.
    pub fn declarations(self) -> impl ExactSizeIterator<Item = ManifestDeclarationRef<'a>> + 'a {
        self.record()
            .declaration_rows()
            .iter()
            .copied()
            .map(move |row| ManifestDeclarationRef {
                index: self.index,
                row,
            })
    }

    /// Returns provider membership for this address.
    pub fn presence(self) -> ProviderPresence {
        self.record().presence()
    }

    /// Returns path and provider metadata available without reading content.
    pub fn metadata(self) -> &'a ResourceMetadata {
        self.record().metadata()
    }

    /// Reports whether any declaration identifies XHTML.
    pub fn has_xhtml_declaration(self) -> bool {
        self.record().has_xhtml_declaration()
    }

    /// Reports whether any declaration identifies CSS.
    pub fn has_stylesheet_declaration(self) -> bool {
        self.record().has_stylesheet_declaration()
    }

    /// Reports whether any declaration identifies SVG.
    pub fn has_svg_declaration(self) -> bool {
        self.record().has_svg_declaration()
    }

    /// Reports whether any declaration carries the `scripted` property.
    pub fn is_scripted(self) -> bool {
        self.record().is_scripted()
    }

    /// Reports whether at least one manifest declaration resolves here.
    pub fn is_manifest_resource(self) -> bool {
        self.record().is_manifest_resource()
    }
}

/// A borrowed manifest declaration in one resource-index snapshot.
#[derive(Clone, Copy)]
pub struct ManifestDeclarationRef<'a> {
    index: &'a ResourceIndex,
    row: ManifestRow,
}

impl std::fmt::Debug for ManifestDeclarationRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManifestDeclarationRef")
            .field("ordinal", &self.ordinal())
            .field("id", self.id())
            .finish()
    }
}

impl PartialEq for ManifestDeclarationRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.row == other.row
    }
}

impl Eq for ManifestDeclarationRef<'_> {}

impl<'a> ManifestDeclarationRef<'a> {
    pub(crate) fn key(self) -> ManifestRow {
        self.row
    }

    pub(crate) fn target_row(self) -> &'a DeclarationTargetRow {
        self.record().target_row()
    }
    fn record(self) -> &'a ManifestDeclarationRow {
        &self.index.declarations[self.row.0]
    }

    /// Returns this declaration's position in the snapshot.
    pub fn ordinal(self) -> ManifestOrdinal {
        self.row.into()
    }

    /// Returns the authored ID state.
    pub fn id(self) -> &'a ManifestIdValue {
        self.record().id()
    }

    /// Returns the exact authored href when present.
    pub fn href(self) -> Option<&'a AuthoredHref> {
        self.record().href()
    }

    /// Returns this declaration's resolution state.
    pub fn target(self) -> DeclarationTarget<'a> {
        self.record().target()
    }

    /// Returns the resolved physical resource, when available.
    pub fn resource(self) -> Option<ResourceRef<'a>> {
        match self.record().target_row() {
            DeclarationTargetRow::Resource(row) => Some(ResourceRef {
                index: self.index,
                row: *row,
            }),
            DeclarationTargetRow::MissingHref | DeclarationTargetRow::InvalidHref(_) => None,
        }
    }

    /// Returns the declared media type when present.
    pub fn media_type(self) -> Option<&'a MediaType> {
        self.record().media_type()
    }

    /// Returns manifest property tokens in authored order.
    pub fn properties(self) -> &'a [ManifestPropertyToken] {
        self.record().properties()
    }

    /// Returns the exact authored fallback IDREF when present.
    pub fn fallback(self) -> Option<&'a AuthoredIdRef> {
        self.record().fallback()
    }

    /// Returns the exact authored media-overlay IDREF when present.
    pub fn media_overlay(self) -> Option<&'a AuthoredIdRef> {
        self.record().media_overlay()
    }
}

/// A borrowed occurrence in authored reading order.
#[derive(Clone, Copy)]
pub struct ReadingOrderOccurrenceRef<'a> {
    index: &'a ResourceIndex,
    row: ReadingOrderRow,
}

impl std::fmt::Debug for ReadingOrderOccurrenceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadingOrderOccurrenceRef")
            .field("ordinal", &self.ordinal())
            .field("idref", &self.idref())
            .finish()
    }
}

impl PartialEq for ReadingOrderOccurrenceRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.row == other.row
    }
}

impl Eq for ReadingOrderOccurrenceRef<'_> {}

impl<'a> ReadingOrderOccurrenceRef<'a> {
    pub(crate) fn target_row(self) -> &'a ReadingOrderTargetRow {
        self.record().target_row()
    }
    fn record(self) -> &'a ReadingOrderEntryRow {
        &self.index.reading_order[self.row.0]
    }

    /// Returns this occurrence's position in the snapshot.
    pub fn ordinal(self) -> ReadingOrderOrdinal {
        self.row.into()
    }

    /// Returns the zero-based authored spine position.
    pub fn index(self) -> usize {
        self.record().index()
    }

    /// Returns the exact authored IDREF when present.
    pub fn idref(self) -> Option<&'a AuthoredIdRef> {
        self.record().idref()
    }

    /// Returns the occurrence's declaration/resource resolution state.
    pub fn target(self) -> ReadingOrderTarget {
        self.record().target()
    }

    /// Returns the uniquely selected declaration, when available.
    pub fn declaration(self) -> Option<ManifestDeclarationRef<'a>> {
        match self.record().target_row() {
            ReadingOrderTargetRow::Declaration { declaration, .. } => {
                Some(ManifestDeclarationRef {
                    index: self.index,
                    row: *declaration,
                })
            }
            ReadingOrderTargetRow::MissingIdref
            | ReadingOrderTargetRow::MissingManifestId
            | ReadingOrderTargetRow::AmbiguousManifestId { .. } => None,
        }
    }

    /// Returns the resolved physical resource, when available.
    pub fn resource(self) -> Option<ResourceRef<'a>> {
        match self.record().target_row() {
            ReadingOrderTargetRow::Declaration {
                resource: Some(row),
                ..
            } => Some(ResourceRef {
                index: self.index,
                row: *row,
            }),
            _ => None,
        }
    }

    /// Returns effective linearity.
    pub fn linear(self) -> Linear {
        self.record().linear()
    }

    /// Returns itemref property tokens in authored order.
    pub fn properties(self) -> &'a [SpinePropertyToken] {
        self.record().properties()
    }

    /// Returns this occurrence's presentation snapshot.
    pub fn presentation(self) -> &'a ReadingOrderPresentation {
        self.record().presentation()
    }
}

impl ResourceIndex {
    pub(crate) fn new(
        package: &Package,
        package_path: impl AsRef<Path>,
        provider_index: &ResourceProviderIndex,
    ) -> Result<Self, ResourceIndexError> {
        let package_path = EpubPath::new(package_path).expect("package path is canonical");
        let package_address = ResourceAddress::Local(package_path.clone());
        let package_key = ResourceRow(0);
        let package_entry = provider_index.get(&package_path);
        let mut resources = vec![ResourceRecordRow {
            address: package_address.clone(),
            declarations: Vec::new(),
            presence: if package_entry.is_some() {
                ProviderPresence::Present
            } else {
                ProviderPresence::Missing
            },
            metadata: metadata_for_address(
                &package_address,
                package_entry.and_then(|entry| entry.size_bytes()),
            ),
            has_xhtml: false,
            has_stylesheet: false,
            has_svg: false,
            scripted: false,
        }];
        let mut by_address = HashMap::from([(package_address, package_key)]);
        let mut declarations = Vec::with_capacity(package.manifest().items().len());
        let mut by_id = HashMap::<String, Vec<ManifestRow>>::new();

        for item in package.manifest().items() {
            let key = ManifestRow(declarations.len());
            let id_value = match item.id() {
                Some(value) => normalize_manifest_id(value)
                    .map(|value| ManifestIdValue::Valid(value.to_string()))
                    .unwrap_or_else(|_| ManifestIdValue::Invalid(value.to_string())),
                None => ManifestIdValue::Missing,
            };
            if let ManifestIdValue::Valid(value) = &id_value {
                by_id.entry(value.clone()).or_default().push(key);
            }
            let href = item.authored_href().cloned();
            let target = match href.as_ref() {
                None => DeclarationTargetRow::MissingHref,
                Some(href) => {
                    match address_for_parsed_href(&parse_href(href.clone()), &package_path) {
                        Some(address) if !matches!(address, ResourceAddress::Invalid(_)) => {
                            let resource_key = if let Some(key) = by_address.get(&address).copied()
                            {
                                key
                            } else {
                                let key = ResourceRow(resources.len());
                                let provider_entry = address
                                    .local_path()
                                    .and_then(|path| provider_index.get(path));
                                let presence = match &address {
                                    ResourceAddress::Local(_) => {
                                        if provider_entry.is_some() {
                                            ProviderPresence::Present
                                        } else {
                                            ProviderPresence::Missing
                                        }
                                    }
                                    ResourceAddress::Remote(_)
                                    | ResourceAddress::Data(_)
                                    | ResourceAddress::External(_) => {
                                        ProviderPresence::NotApplicable
                                    }
                                    ResourceAddress::Invalid(_) => unreachable!(),
                                };
                                resources.push(ResourceRecordRow {
                                    address: address.clone(),
                                    declarations: Vec::new(),
                                    presence,
                                    metadata: metadata_for_address(
                                        &address,
                                        provider_entry.and_then(|entry| entry.size_bytes()),
                                    ),
                                    has_xhtml: false,
                                    has_stylesheet: false,
                                    has_svg: false,
                                    scripted: false,
                                });
                                by_address.insert(address, key);
                                key
                            };
                            DeclarationTargetRow::Resource(resource_key)
                        }
                        _ => DeclarationTargetRow::InvalidHref(href.clone()),
                    }
                }
            };
            if let DeclarationTargetRow::Resource(resource_key) = target {
                let record = &mut resources[resource_key.0];
                record.declarations.push(key);
                record.has_xhtml |= item.media_type().is_some_and(MediaType::is_xhtml);
                record.has_stylesheet |= item.media_type().is_some_and(MediaType::is_css);
                record.has_svg |= item.media_type().is_some_and(MediaType::is_svg);
                record.scripted |= item.has_property(KnownManifestProperty::Scripted);
            }
            declarations.push(ManifestDeclarationRow {
                id: id_value,
                href,
                target,
                media_type: item.media_type().cloned(),
                properties: item.properties().to_vec(),
                fallback: item.fallback().map(AuthoredIdRef::new),
                media_overlay: item.media_overlay().map(AuthoredIdRef::new),
            });
        }

        for entry in provider_index.publication_entries() {
            let address = ResourceAddress::Local(entry.path().clone());
            if by_address.contains_key(&address) {
                continue;
            }
            let key = ResourceRow(resources.len());
            resources.push(ResourceRecordRow {
                address: address.clone(),
                declarations: Vec::new(),
                presence: ProviderPresence::Present,
                metadata: metadata_for_address(&address, entry.size_bytes()),
                has_xhtml: false,
                has_stylesheet: false,
                has_svg: false,
                scripted: false,
            });
            by_address.insert(address, key);
        }

        ensure_ordinal_count(resources.len(), ResourceTopologyKind::Resources)?;
        ensure_ordinal_count(
            declarations.len(),
            ResourceTopologyKind::ManifestDeclarations,
        )?;
        ensure_ordinal_count(
            package.spine().itemrefs().len(),
            ResourceTopologyKind::ReadingOrderOccurrences,
        )?;

        let reading_order = package
            .spine()
            .itemrefs()
            .iter()
            .enumerate()
            .map(|(index, itemref)| {
                let idref = itemref.idref().map(AuthoredIdRef::new);
                let matching_id = idref
                    .as_ref()
                    .and_then(|value| normalize_manifest_id(value.as_str()).ok());
                let target = match matching_id.and_then(|value| by_id.get(value)) {
                    None if idref.is_none() => ReadingOrderTargetRow::MissingIdref,
                    None => ReadingOrderTargetRow::MissingManifestId,
                    Some(keys) if keys.len() > 1 => ReadingOrderTargetRow::AmbiguousManifestId {
                        candidates: keys.clone(),
                    },
                    Some(keys) => {
                        let declaration = keys[0];
                        let resource = match declarations[declaration.0].target {
                            DeclarationTargetRow::Resource(key) => Some(key),
                            DeclarationTargetRow::MissingHref
                            | DeclarationTargetRow::InvalidHref(_) => None,
                        };
                        ReadingOrderTargetRow::Declaration {
                            declaration,
                            resource,
                        }
                    }
                };
                ReadingOrderEntryRow {
                    index,
                    idref,
                    target,
                    linear: itemref.linear(),
                    properties: itemref.properties().to_vec(),
                    presentation: reading_order_presentation(package, itemref),
                }
            })
            .collect();

        let select_candidates = |source, candidates: Vec<ManifestRow>| match candidates.as_slice() {
            [] => SelectionRows::Absent,
            [declaration] => SelectionRows::Selected {
                source,
                declaration: Some(*declaration),
                resource: match declarations[declaration.0].target {
                    DeclarationTargetRow::Resource(resource) => Some(resource),
                    DeclarationTargetRow::MissingHref | DeclarationTargetRow::InvalidHref(_) => {
                        None
                    }
                },
            },
            _ => SelectionRows::Ambiguous { source, candidates },
        };
        let project_selection = |selection: PackageSelection| match selection {
            PackageSelection::Absent => SelectionRows::Absent,
            PackageSelection::Selected {
                source,
                declaration,
            } => select_candidates(source, vec![ManifestRow(declaration)]),
            PackageSelection::UnresolvedAuthoredId {
                source,
                authored_id,
            } => SelectionRows::UnresolvedAuthoredId {
                source,
                authored_id,
            },
            PackageSelection::Ambiguous { source, candidates } => SelectionRows::Ambiguous {
                source,
                candidates: candidates.into_iter().map(ManifestRow).collect(),
            },
        };
        let package_selections = package.resource_selections();
        let nav = project_selection(package_selections.epub_nav);
        let cover = project_selection(package_selections.cover);
        let ncx = project_selection(package_selections.ncx);
        let selections = ResourceSelectionRows {
            package: SelectionRows::Selected {
                source: facts::SelectionSource::PackagePath,
                declaration: None,
                resource: Some(package_key),
            },
            cover,
            epub_nav: nav,
            ncx,
        };
        Ok(Self {
            declarations,
            resources,
            reading_order,
            by_id,
            by_address,
            package: package_key,
            selections,
        })
    }

    /// Returns the number of distinct resolved resources.
    ///
    /// This includes the package document, manifest resources, and provider-only publication
    /// resources; duplicate declarations at one address count once.
    pub fn len(&self) -> usize {
        self.resources.len()
    }
    /// Reports whether the resource inventory is empty.
    ///
    /// A normally constructed publication index contains at least its package resource.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
    /// Returns all manifest declarations in authored order.
    pub fn declarations(&self) -> impl ExactSizeIterator<Item = ManifestDeclarationRef<'_>> {
        (0..self.declarations.len()).map(|index| ManifestDeclarationRef {
            index: self,
            row: ManifestRow(index),
        })
    }
    /// Returns all distinct resource records in deterministic index order.
    pub fn resources(&self) -> impl ExactSizeIterator<Item = ResourceRef<'_>> {
        (0..self.resources.len()).map(|index| ResourceRef {
            index: self,
            row: ResourceRow(index),
        })
    }
    /// Iterates every authored spine occurrence in order.
    pub fn reading_order(&self) -> impl ExactSizeIterator<Item = ReadingOrderOccurrenceRef<'_>> {
        (0..self.reading_order.len()).map(|index| ReadingOrderOccurrenceRef {
            index: self,
            row: ReadingOrderRow(index),
        })
    }
    /// Iterates resources having at least one manifest declaration.
    pub fn manifest_resources(&self) -> impl Iterator<Item = ResourceRef<'_>> {
        self.resources()
            .filter(|resource| resource.is_manifest_resource())
    }
    pub(crate) fn validate_resource_row(
        &self,
        row: impl Into<ResourceRow>,
    ) -> Result<(), IndexRowError> {
        let row = row.into();
        self.resources.get(row.0).map(|_| ()).ok_or(IndexRowError)
    }
    /// Borrows a resource at a snapshot-local ordinal.
    pub fn resource(
        &self,
        ordinal: ResourceOrdinal,
    ) -> Result<ResourceRef<'_>, OrdinalOutOfBounds> {
        (ordinal.index() < self.resources.len())
            .then_some(ResourceRef {
                index: self,
                row: ordinal.into(),
            })
            .ok_or(OrdinalOutOfBounds)
    }
    /// Borrows a declaration at a snapshot-local ordinal.
    pub fn declaration(
        &self,
        ordinal: ManifestOrdinal,
    ) -> Result<ManifestDeclarationRef<'_>, OrdinalOutOfBounds> {
        (ordinal.index() < self.declarations.len())
            .then_some(ManifestDeclarationRef {
                index: self,
                row: ordinal.into(),
            })
            .ok_or(OrdinalOutOfBounds)
    }
    /// Borrows a reading-order occurrence at a snapshot-local ordinal.
    pub fn occurrence(
        &self,
        ordinal: ReadingOrderOrdinal,
    ) -> Result<ReadingOrderOccurrenceRef<'_>, OrdinalOutOfBounds> {
        (ordinal.index() < self.reading_order.len())
            .then_some(ReadingOrderOccurrenceRef {
                index: self,
                row: ordinal.into(),
            })
            .ok_or(OrdinalOutOfBounds)
    }
    /// Iterates declarations whose valid manifest ID exactly equals `value`.
    ///
    /// Invalid authored IDs are preserved on declarations but are not indexed by this query.
    pub fn declarations_with_id<'a>(
        &'a self,
        value: &'a str,
    ) -> Result<
        impl Iterator<Item = ManifestDeclarationRef<'a>> + 'a,
        crate::package::InvalidManifestId,
    > {
        let value = normalize_manifest_id(value)?;
        Ok(self
            .by_id
            .get(value)
            .into_iter()
            .flatten()
            .map(|row| ManifestDeclarationRef {
                index: self,
                row: *row,
            }))
    }
    pub(crate) fn resolve_manifest_idref(&self, value: &str) -> ManifestIdrefResolution<'_> {
        let Ok(value) = normalize_manifest_id(value) else {
            return ManifestIdrefResolution::Invalid;
        };
        match self.by_id.get(value).map(Vec::as_slice).unwrap_or_default() {
            [] => ManifestIdrefResolution::Missing,
            [row] => ManifestIdrefResolution::Unique(*row),
            rows => ManifestIdrefResolution::Ambiguous(rows),
        }
    }
    /// Iterates the resource at an exact resolved address.
    ///
    /// Address identity is unique, so this iterator yields zero or one record; duplicate
    /// declarations remain available through [`ResourceRef::declarations`].
    pub fn resources_at<'a>(
        &'a self,
        address: &ResourceAddress,
    ) -> impl Iterator<Item = ResourceRef<'a>> + 'a {
        self.by_address
            .get(address)
            .map(|row| ResourceRef {
                index: self,
                row: *row,
            })
            .into_iter()
    }
    /// Selects exactly one declaration by valid manifest ID.
    ///
    /// The lookup reports duplicate IDs as ambiguity and does not require the declaration's
    /// href to resolve to a resource.
    pub fn find_unique_by_id(
        &self,
        value: &str,
    ) -> Result<ManifestDeclarationRef<'_>, ResourceLookupError> {
        let value = normalize_manifest_id(value)?;
        let keys = self.by_id.get(value).map(Vec::as_slice).unwrap_or_default();
        match keys {
            [] => Err(ResourceLookupError::ManifestIdNotFound {
                id: value.to_string(),
            }),
            [row] => Ok(ManifestDeclarationRef {
                index: self,
                row: *row,
            }),
            keys => Err(ResourceLookupError::AmbiguousManifestId {
                id: value.to_string(),
                candidates: keys
                    .iter()
                    .copied()
                    .map(|row| ManifestOrdinal::from_index(row.0))
                    .collect(),
            }),
        }
    }
    /// Selects exactly one declaration by ID and returns its resolved resource.
    ///
    /// A unique declaration with a missing or invalid href produces
    /// [`ResourceLookupError::UnresolvedDeclaration`].
    pub fn find_unique_resource_by_id(
        &self,
        value: &str,
    ) -> Result<ResourceRef<'_>, ResourceLookupError> {
        let value = normalize_manifest_id(value)?;
        let rows = self.by_id.get(value).map(Vec::as_slice).unwrap_or_default();
        let mut resolved = rows
            .iter()
            .filter_map(|row| match self.declarations[row.0].target {
                DeclarationTargetRow::Resource(resource) => Some(resource),
                DeclarationTargetRow::MissingHref | DeclarationTargetRow::InvalidHref(_) => None,
            });
        let Some(first) = resolved.next() else {
            return match rows {
                [] => Err(ResourceLookupError::ManifestIdNotFound {
                    id: value.to_string(),
                }),
                [row] => Err(ResourceLookupError::UnresolvedDeclaration(
                    ManifestOrdinal::from_index(row.0),
                )),
                _ => Err(ResourceLookupError::AmbiguousManifestId {
                    id: value.to_string(),
                    candidates: rows
                        .iter()
                        .map(|row| ManifestOrdinal::from_index(row.0))
                        .collect(),
                }),
            };
        };
        if resolved.all(|row| row == first) && rows.iter().all(|row| matches!(self.declarations[row.0].target, DeclarationTargetRow::Resource(resource) if resource == first)) {
            Ok(ResourceRef { index: self, row: first })
        } else {
            Err(ResourceLookupError::AmbiguousManifestId {
                id: value.to_string(),
                candidates: rows.iter().map(|row| ManifestOrdinal::from_index(row.0)).collect(),
            })
        }
    }
    /// Returns the loaded package document resource.
    pub fn package(&self) -> ResourceRef<'_> {
        ResourceRef {
            index: self,
            row: self.package,
        }
    }
    /// Returns the package-selected EPUB navigation resource, when one resolved.
    pub fn epub_nav(&self) -> Option<ResourceRef<'_>> {
        self.selected_resource(&self.selections.epub_nav)
    }
    /// Returns the package-selected NCX resource, when one resolved.
    pub fn ncx(&self) -> Option<ResourceRef<'_>> {
        self.selected_resource(&self.selections.ncx)
    }
    /// Returns the package-selected cover image resource, when one resolved.
    pub fn cover_image(&self) -> Option<ResourceRef<'_>> {
        self.selected_resource(&self.selections.cover)
    }
    pub(crate) fn epub_nav_declaration(&self) -> Option<ManifestOrdinal> {
        self.selected_declaration(&self.selections.epub_nav)
    }
    pub(crate) fn ncx_declaration(&self) -> Option<ManifestOrdinal> {
        self.selected_declaration(&self.selections.ncx)
    }
    /// Resolves authored href text relative to the package document.
    ///
    /// Resolution classifies syntax and canonical address identity but does not read bytes or
    /// establish resource or fragment existence.
    pub fn resolve_manifest_href(&self, href: impl AsRef<str>) -> ResolvedHref {
        self.resolve_href_from(href, self.package().local_path().expect("package is local"))
    }
    /// Resolves a link found in `source` to the address an application should target.
    ///
    /// The resolver percent-decodes local paths, removes relative dot segments during
    /// source-relative resolution, and never repairs malformed authored syntax. Fragment
    /// existence remains unknown because resolution does not parse target content.
    pub fn resolve_href_from(&self, href: impl AsRef<str>, source: &EpubPath) -> ResolvedHref {
        target_for_href(&AuthoredHref::new(href.as_ref()), source)
    }
    /// Selects one indexed resource using a typed selector.
    ///
    /// Href selectors first resolve against their stated base. Syntactically valid addresses
    /// that are absent from the inventory return [`ResourceLookupError::NotFound`].
    pub fn select(
        &self,
        selector: &ResourceSelector,
    ) -> Result<ResourceRef<'_>, ResourceLookupError> {
        let key = match selector {
            ResourceSelector::Path(path) => {
                self.by_address.get(&ResourceAddress::Local(path.clone()))
            }
            ResourceSelector::Address(address) => self.by_address.get(address),
            ResourceSelector::ManifestHref(href) => {
                return self.select_resolved(selector, self.resolve_manifest_href(href.as_str()));
            }
            ResourceSelector::HrefFrom { href, source } => {
                return self
                    .select_resolved(selector, self.resolve_href_from(href.as_str(), source));
            }
            ResourceSelector::Package => Some(&self.package),
            ResourceSelector::EpubNav => {
                return self.select_structural(selector, &self.selections.epub_nav);
            }
            ResourceSelector::CoverImage => {
                return self.select_structural(selector, &self.selections.cover);
            }
        };
        key.map(|row| ResourceRef {
            index: self,
            row: *row,
        })
        .ok_or_else(|| ResourceLookupError::NotFound(selector.clone()))
    }
    fn select_resolved(
        &self,
        selector: &ResourceSelector,
        resolved: ResolvedHref,
    ) -> Result<ResourceRef<'_>, ResourceLookupError> {
        let address = match resolved {
            ResolvedHref::Resource(address)
            | ResolvedHref::Fragment {
                resource: address, ..
            } => address,
            ResolvedHref::RemoteUrl(value) => ResourceAddress::Remote(value),
            ResolvedHref::Data(value) => ResourceAddress::Data(value),
            ResolvedHref::External(value) => ResourceAddress::External(value),
            ResolvedHref::Invalid(_)
            | ResolvedHref::MissingPath(_)
            | ResolvedHref::MissingManifestId(_)
            | ResolvedHref::AmbiguousAddress { .. } => {
                return Err(ResourceLookupError::NotFound(selector.clone()));
            }
        };
        self.by_address
            .get(&address)
            .map(|row| ResourceRef {
                index: self,
                row: *row,
            })
            .ok_or_else(|| ResourceLookupError::NotFound(selector.clone()))
    }

    fn selected_resource(&self, selection: &SelectionRows) -> Option<ResourceRef<'_>> {
        match selection {
            SelectionRows::Selected {
                resource: Some(row),
                ..
            } => Some(ResourceRef {
                index: self,
                row: *row,
            }),
            _ => None,
        }
    }

    fn selected_declaration(&self, selection: &SelectionRows) -> Option<ManifestOrdinal> {
        match selection {
            SelectionRows::Selected {
                declaration: Some(row),
                ..
            } => Some((*row).into()),
            _ => None,
        }
    }

    fn select_structural(
        &self,
        selector: &ResourceSelector,
        selection: &SelectionRows,
    ) -> Result<ResourceRef<'_>, ResourceLookupError> {
        match selection {
            SelectionRows::Selected {
                resource: Some(row),
                ..
            } => Ok(ResourceRef {
                index: self,
                row: *row,
            }),
            SelectionRows::Selected {
                declaration: Some(row),
                resource: None,
                ..
            } => Err(ResourceLookupError::UnresolvedDeclaration(
                ManifestOrdinal::from_index(row.0),
            )),
            SelectionRows::Ambiguous { candidates, .. } => Err(ResourceLookupError::Ambiguous {
                selector: selector.clone(),
                candidates: candidates
                    .iter()
                    .map(|row| ResourceLookupCandidate::Manifest((*row).into()))
                    .collect(),
            }),
            SelectionRows::Absent
            | SelectionRows::UnresolvedAuthoredId { .. }
            | SelectionRows::Selected {
                declaration: None,
                resource: None,
                ..
            } => Err(ResourceLookupError::NotFound(selector.clone())),
        }
    }
}

fn ensure_ordinal_count(
    count: usize,
    kind: ResourceTopologyKind,
) -> Result<(), ResourceIndexError> {
    if count
        .checked_sub(1)
        .is_none_or(|last| u32::try_from(last).is_ok())
    {
        Ok(())
    } else {
        Err(ResourceIndexError { kind })
    }
}

fn reading_order_presentation(package: &Package, itemref: &ItemRef) -> ReadingOrderPresentation {
    ReadingOrderPresentation {
        layout: rendition_setting(
            package,
            itemref,
            KnownMetaProperty::RenditionLayout,
            Meta::rendition_layout,
            |property| match property {
                KnownSpineProperty::RenditionLayoutPrePaginated => {
                    Some(RenditionLayout::PrePaginated)
                }
                KnownSpineProperty::RenditionLayoutReflowable => Some(RenditionLayout::Reflowable),
                _ => None,
            },
        ),
        flow: rendition_setting(
            package,
            itemref,
            KnownMetaProperty::RenditionFlow,
            Meta::rendition_flow,
            |property| match property {
                KnownSpineProperty::RenditionFlowAuto => Some(RenditionFlow::Auto),
                KnownSpineProperty::RenditionFlowPaginated => Some(RenditionFlow::Paginated),
                KnownSpineProperty::RenditionFlowScrolledContinuous => {
                    Some(RenditionFlow::ScrolledContinuous)
                }
                KnownSpineProperty::RenditionFlowScrolledDoc => Some(RenditionFlow::ScrolledDoc),
                _ => None,
            },
        ),
        orientation: rendition_setting(
            package,
            itemref,
            KnownMetaProperty::RenditionOrientation,
            Meta::rendition_orientation,
            |property| match property {
                KnownSpineProperty::RenditionOrientationAuto => Some(RenditionOrientation::Auto),
                KnownSpineProperty::RenditionOrientationLandscape => {
                    Some(RenditionOrientation::Landscape)
                }
                KnownSpineProperty::RenditionOrientationPortrait => {
                    Some(RenditionOrientation::Portrait)
                }
                _ => None,
            },
        ),
        spread: rendition_setting(
            package,
            itemref,
            KnownMetaProperty::RenditionSpread,
            Meta::rendition_spread,
            |property| match property {
                KnownSpineProperty::RenditionSpreadAuto => Some(RenditionSpread::Auto),
                KnownSpineProperty::RenditionSpreadBoth => Some(RenditionSpread::Both),
                KnownSpineProperty::RenditionSpreadLandscape => Some(RenditionSpread::Landscape),
                KnownSpineProperty::RenditionSpreadNone => Some(RenditionSpread::None),
                KnownSpineProperty::RenditionSpreadPortrait => Some(RenditionSpread::Portrait),
                _ => None,
            },
        ),
        page_spread: RenditionSetting::from_candidates(itemref_candidates(
            itemref,
            |property| match property {
                KnownSpineProperty::PageSpreadLeft => Some(PageSpread::Left),
                KnownSpineProperty::PageSpreadRight => Some(PageSpread::Right),
                KnownSpineProperty::PageSpreadCenter => Some(PageSpread::Center),
                KnownSpineProperty::UnprefixedPageSpreadLeft => Some(PageSpread::Left),
                KnownSpineProperty::UnprefixedPageSpreadRight => Some(PageSpread::Right),
                _ => None,
            },
            &["rendition:page-spread-", "page-spread-"],
        )),
        page_progression_direction: package.spine().page_progression_direction(),
    }
}

fn rendition_setting<T>(
    package: &Package,
    itemref: &ItemRef,
    property: KnownMetaProperty,
    meta_value: impl Fn(&Meta) -> Option<T>,
    itemref_value: impl Fn(KnownSpineProperty) -> Option<T>,
) -> RenditionSetting<T> {
    let family = format!("{property}-");
    let itemref_candidates = itemref_candidates(itemref, itemref_value, &[family.as_str()]);
    if !itemref_candidates.is_empty() {
        return RenditionSetting::from_candidates(itemref_candidates);
    }

    RenditionSetting::from_candidates(
        package
            .metadata()
            .meta()
            .iter()
            .filter(|meta| {
                meta.refines().is_none()
                    && meta.property().and_then(|token| token.known_value()) == Some(property)
            })
            .map(|meta| RenditionCandidate {
                source: RenditionValueSource::PackageMetadata,
                authored_value: meta.content().cloned(),
                value: meta_value(meta),
            })
            .collect(),
    )
}

fn itemref_candidates<T>(
    itemref: &ItemRef,
    value: impl Fn(KnownSpineProperty) -> Option<T>,
    families: &[&str],
) -> Vec<RenditionCandidate<T>> {
    itemref
        .properties()
        .iter()
        .filter_map(|token| {
            let value = token.known_value().and_then(&value);
            if value.is_none()
                && !families
                    .iter()
                    .any(|family| starts_ascii_case_insensitive(token.as_str(), family))
            {
                return None;
            }
            Some(RenditionCandidate {
                source: RenditionValueSource::ItemRefProperty,
                authored_value: Some(token.raw_value().clone()),
                value,
            })
        })
        .collect()
}

fn starts_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn metadata_for_address(address: &ResourceAddress, size_bytes: Option<u64>) -> ResourceMetadata {
    let file_extension = address
        .local_path()
        .and_then(|path| path.as_path().extension())
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    ResourceMetadata {
        size_bytes,
        file_extension,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Syntactic classification of exact authored href text.
///
/// Parsing does not establish resource existence. Every variant retains the original text;
/// valid targets retain checked href syntax and fragments are percent-decoded as UTF-8.
pub enum ParsedHref {
    /// A source-relative local reference.
    Local {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked target text, including any query but excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// An HTTP(S) or scheme-relative URL.
    Remote {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked remote target excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A `data:` URL.
    Data {
        /// Exact authored data URL.
        original: AuthoredHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A valid non-HTTP, non-data URI with a scheme.
    ExternalScheme {
        /// Exact authored href.
        original: AuthoredHref,
        /// Checked target excluding the fragment.
        target: EpubHref,
        /// Percent-decoded fragment, including an explicitly empty fragment.
        fragment: Option<String>,
    },
    /// A fragment reference to the source document itself.
    SameDocument {
        /// Exact authored href.
        original: AuthoredHref,
        /// Percent-decoded fragment, which may be empty for `#`.
        fragment: String,
    },
    /// An exactly empty authored value.
    Empty {
        /// Empty authored href retained for source fidelity.
        original: AuthoredHref,
    },
    /// Non-empty authored text with rejected lexical or URL/path syntax.
    Invalid {
        /// Exact malformed authored href.
        original: AuthoredHref,
    },
}

impl ParsedHref {
    /// Returns the exact authored href retained by every variant.
    pub fn original(&self) -> &AuthoredHref {
        match self {
            Self::Local { original, .. }
            | Self::Remote { original, .. }
            | Self::Data { original, .. }
            | Self::ExternalScheme { original, .. }
            | Self::SameDocument { original, .. }
            | Self::Empty { original }
            | Self::Invalid { original } => original,
        }
    }

    /// Returns a non-empty percent-decoded fragment when one exists.
    ///
    /// This intentionally maps an explicitly empty fragment such as `#` to `None`.
    pub fn fragment(&self) -> Option<&str> {
        match self {
            Self::Local { fragment, .. }
            | Self::Remote { fragment, .. }
            | Self::ExternalScheme { fragment, .. } => {
                fragment.as_deref().filter(|fragment| !fragment.is_empty())
            }
            Self::SameDocument { fragment, .. } => {
                (!fragment.is_empty()).then_some(fragment.as_str())
            }
            Self::Data { fragment, .. } => {
                fragment.as_deref().filter(|fragment| !fragment.is_empty())
            }
            Self::Empty { .. } | Self::Invalid { .. } => None,
        }
    }

    /// Revalidates and returns the complete original href when it has valid lexical syntax.
    ///
    /// This can return a value for semantic classifications such as [`Self::Empty`] only when
    /// the original itself satisfies [`EpubHref`] syntax; empty and invalid originals return
    /// `None`.
    pub fn original_valid_href(&self) -> Option<EpubHref> {
        self.original().to_epub_href()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of resolving authored href text relative to a canonical source path.
///
/// This is syntactic/address resolution, not a byte read. Cheap resolution does not establish
/// target or fragment existence, so fragment `exists` is normally `None` here. Missing and
/// ambiguity variants are also used by higher-level resolution workflows.
pub enum ResolvedHref {
    /// A local or declared remote resource address without a fragment.
    Resource(
        /// Resolved local or remote address.
        ResourceAddress,
    ),
    /// A resource address plus a non-empty decoded fragment.
    Fragment {
        /// Resolved resource address.
        resource: ResourceAddress,
        /// Percent-decoded non-empty fragment.
        fragment: String,
        /// Known fragment existence, or `None` when target content was not inspected.
        exists: Option<bool>,
    },
    /// An HTTP(S) or scheme-relative URL not represented as a resource result.
    RemoteUrl(
        /// Resolved remote URL.
        String,
    ),
    /// A retained `data:` URL.
    Data(
        /// Exact authored data URL.
        String,
    ),
    /// A retained URL using another scheme.
    External(
        /// Exact authored URL using another scheme.
        String,
    ),
    /// A canonical local target absent from complete resource inventory.
    MissingPath(
        /// Canonical local target that was absent.
        EpubPath,
    ),
    /// An authored manifest IDREF with no matching declaration.
    MissingManifestId(
        /// Exact authored IDREF with no match.
        String,
    ),
    /// An address associated with multiple candidate manifest IDs.
    AmbiguousAddress {
        /// Resolved address shared by the candidates.
        address: ResourceAddress,
        /// Candidate manifest IDs.
        candidates: Vec<ManifestOrdinal>,
    },
    /// Exact authored text that could not be resolved.
    Invalid(
        /// Exact authored text that failed resolution.
        String,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A typed request to select one resource from a [`ResourceIndex`].
pub enum ResourceSelector {
    /// Select by exact canonical local path.
    Path(
        /// Exact canonical local path.
        EpubPath,
    ),
    /// Select by exact resolved address.
    Address(
        /// Exact resolved address.
        ResourceAddress,
    ),
    /// Resolve a valid href relative to the package document, then select its address.
    ManifestHref(
        /// Checked href to resolve from the package document.
        EpubHref,
    ),
    /// Resolve a valid href relative to a canonical source document, then select its address.
    HrefFrom {
        /// Checked authored href.
        href: EpubHref,
        /// Canonical source document path used as the resolution base.
        source: EpubPath,
    },
    /// Select the loaded package document.
    Package,
    /// Select the package-selected EPUB navigation document.
    EpubNav,
    /// Select the package-selected cover image.
    CoverImage,
}

impl From<EpubPath> for ResourceSelector {
    fn from(value: EpubPath) -> Self {
        Self::Path(value)
    }
}

impl From<ResourceAddress> for ResourceSelector {
    fn from(value: ResourceAddress) -> Self {
        Self::Address(value)
    }
}

impl ResourceSelector {
    /// Creates an exact canonical local-path selector.
    pub fn path(value: impl AsRef<Path>) -> Option<Self> {
        EpubPath::new(value).ok().map(Self::Path)
    }

    /// Creates a package-relative href selector after lexical validation.
    pub fn manifest_href(value: impl AsRef<str>) -> Option<Self> {
        EpubHref::try_new(value).ok().map(Self::ManifestHref)
    }

    /// Creates a source-relative href selector after validating both values.
    pub fn href_from(href: impl AsRef<str>, source: impl AsRef<Path>) -> Option<Self> {
        Some(Self::HrefFrom {
            href: EpubHref::try_new(href).ok()?,
            source: EpubPath::new(source).ok()?,
        })
    }
}

/// Classifies exact authored href text without establishing target existence.
///
/// Local path and fragment percent escapes must decode as UTF-8. Encoded path separators,
/// malformed escapes, whitespace, controls, backslashes, absolute local paths, and local
/// scheme-like first segments are rejected rather than repaired.
pub fn parse_href(href: AuthoredHref) -> ParsedHref {
    let original = href.clone();
    let value = href.as_str();
    if value.is_empty() {
        return ParsedHref::Empty { original };
    }
    if !valid_href_syntax(value) {
        return ParsedHref::Invalid { original };
    }
    let (target, raw_fragment) = value
        .split_once('#')
        .map(|(target, fragment)| (target, Some(fragment)))
        .unwrap_or((value, None));
    let fragment = match raw_fragment {
        Some(fragment) => {
            let Ok(fragment) = percent_encoding::percent_decode_str(fragment).decode_utf8() else {
                return ParsedHref::Invalid { original };
            };
            Some(fragment.into_owned())
        }
        None => None,
    };
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if value.starts_with('#') {
        ParsedHref::SameDocument {
            original,
            fragment: fragment.unwrap_or_default(),
        }
    } else if target.starts_with('?') {
        ParsedHref::Local {
            original,
            target: EpubHref::try_new(target).expect("href syntax was checked"),
            fragment,
        }
    } else if target.starts_with("//")
        || starts_with_ascii_case_insensitive(target, "http://")
        || starts_with_ascii_case_insensitive(target, "https://")
    {
        match valid_remote_url(target)
            .then(|| EpubHref::try_new(target))
            .transpose()
        {
            Ok(Some(target)) => ParsedHref::Remote {
                original,
                target,
                fragment,
            },
            Ok(None) | Err(_) => ParsedHref::Invalid { original },
        }
    } else if starts_with_ascii_case_insensitive(target, "data:") {
        ParsedHref::Data { original, fragment }
    } else if has_scheme(target) {
        match EpubHref::try_new(target) {
            Ok(target) => ParsedHref::ExternalScheme {
                original,
                target,
                fragment,
            },
            Err(_) => ParsedHref::Invalid { original },
        }
    } else if path.starts_with('/')
        || path
            .split('/')
            .next()
            .is_some_and(|segment| segment.contains(':'))
        || contains_encoded_separator(path)
        || decoded_local_path(path).is_none()
    {
        ParsedHref::Invalid { original }
    } else {
        match EpubHref::try_new(target) {
            Ok(target) => ParsedHref::Local {
                original,
                target,
                fragment,
            },
            Err(_) => ParsedHref::Invalid { original },
        }
    }
}

fn target_for_href(href: &AuthoredHref, source: &EpubPath) -> ResolvedHref {
    let parsed = parse_href(href.clone());
    let address = address_for_parsed_href(&parsed, source);
    let invalid_value = value_for_invalid_href(&parsed);
    match (address, parsed.fragment().map(str::to_string)) {
        (Some(ResourceAddress::Local(path)), Some(fragment)) => ResolvedHref::Fragment {
            resource: ResourceAddress::Local(path),
            fragment,
            exists: None,
        },
        (Some(ResourceAddress::Remote(url)), Some(fragment)) => ResolvedHref::Fragment {
            resource: ResourceAddress::Remote(url),
            fragment,
            exists: None,
        },
        (Some(ResourceAddress::Local(path)), None) => {
            ResolvedHref::Resource(ResourceAddress::Local(path))
        }
        (Some(ResourceAddress::Remote(url)), None) => {
            ResolvedHref::Resource(ResourceAddress::Remote(url))
        }
        (Some(ResourceAddress::Data(value)), _) => ResolvedHref::Data(value),
        (Some(ResourceAddress::External(value)), _) => ResolvedHref::External(value),
        (Some(ResourceAddress::Invalid(value)), _) => ResolvedHref::Invalid(value),
        (None, _) => ResolvedHref::Invalid(invalid_value),
    }
}

fn value_for_invalid_href(parsed: &ParsedHref) -> String {
    parsed.original().as_str().to_string()
}

fn address_for_parsed_href(parsed: &ParsedHref, source: &EpubPath) -> Option<ResourceAddress> {
    let root_dir = source_dir(source);
    match parsed {
        ParsedHref::Remote { target, .. } => {
            Some(ResourceAddress::Remote(target.as_str().to_string()))
        }
        ParsedHref::Data { original, .. } => {
            Some(ResourceAddress::Data(original.as_str().to_string()))
        }
        ParsedHref::ExternalScheme { target, .. } => {
            Some(ResourceAddress::External(target.as_str().to_string()))
        }
        ParsedHref::Local { target, .. } => {
            local_path_for_target(target, &root_dir, source).map(ResourceAddress::Local)
        }
        ParsedHref::SameDocument { .. } => Some(ResourceAddress::Local(source.clone())),
        ParsedHref::Empty { original } | ParsedHref::Invalid { original } => {
            Some(ResourceAddress::Invalid(original.as_str().to_string()))
        }
    }
}

pub(crate) fn resolve_local_href_from_source(
    href: &AuthoredHref,
    source: &EpubPath,
) -> Option<(EpubPath, Option<String>)> {
    let ParsedHref::Local {
        target, fragment, ..
    } = parse_href(href.clone())
    else {
        return None;
    };
    let base = source_dir(source);
    local_path_for_target(&target, &base, source).map(|path| (path, fragment))
}

fn local_path_for_target(target: &EpubHref, base: &Path, source: &EpubPath) -> Option<EpubPath> {
    let authored_path = target
        .as_str()
        .split_once('?')
        .map_or(target.as_str(), |(path, _)| path);
    if authored_path.is_empty() {
        return Some(source.clone());
    }
    let decoded = decoded_local_path(authored_path)?;
    resolve_relative_epub_path(base, decoded.as_ref())
}

fn resolve_relative_epub_path(base: &Path, relative: &str) -> Option<EpubPath> {
    let mut parts = base
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_string),
            Component::CurDir => Some(String::new()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    parts.retain(|part| !part.is_empty());
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    EpubPath::new(parts.join("/")).ok()
}

fn source_dir(source: &EpubPath) -> PathBuf {
    source
        .as_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn has_scheme(value: &str) -> bool {
    value.split_once(':').is_some_and(|(scheme, _)| {
        scheme
            .as_bytes()
            .first()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
            && scheme
                .chars()
                .skip(1)
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    })
}

fn starts_with_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn valid_remote_url(value: &str) -> bool {
    if value.starts_with("//") {
        url::Url::parse(&format!("https:{value}")).is_ok()
    } else {
        url::Url::parse(value).is_ok()
    }
}

fn valid_href_syntax(value: &str) -> bool {
    if value.trim() != value
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return false;
    }

    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let Some(_) = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok())
        else {
            return false;
        };
        index += 3;
    }
    true
}

fn contains_encoded_separator(value: &str) -> bool {
    value.as_bytes().windows(3).any(|escape| {
        escape[0] == b'%'
            && u8::from_str_radix(std::str::from_utf8(&escape[1..]).unwrap_or_default(), 16)
                .is_ok_and(|decoded| matches!(decoded, b'/' | b'\\'))
    })
}

fn decoded_local_path(value: &str) -> Option<std::borrow::Cow<'_, str>> {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()
}

fn validate_epub_path(value: &str) -> Result<(), EpubPathError> {
    if value.is_empty() {
        return Err(EpubPathError::Empty);
    }
    if value.starts_with('/') {
        return Err(EpubPathError::Absolute);
    }
    if value.contains('\\') {
        return Err(EpubPathError::Backslash);
    }
    if value.contains(['?', '#']) {
        return Err(EpubPathError::QueryOrFragment);
    }
    if value.chars().any(char::is_control) {
        return Err(EpubPathError::ControlCharacter);
    }

    let mut segments = value.split('/');
    let first = segments.next().expect("non-empty string has one segment");
    if first.is_empty() || segments.clone().any(str::is_empty) {
        return Err(EpubPathError::EmptySegment);
    }
    if first == "." || first == ".." || segments.clone().any(|part| matches!(part, "." | "..")) {
        return Err(EpubPathError::DotSegment);
    }
    if first.contains(':') {
        return Err(EpubPathError::SchemeLikeFirstSegment);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::provider::{ResourceProviderEntry, ResourceProviderIndexLimits};

    fn parse_package(xml: &str) -> Package {
        Package::parse(xml).unwrap()
    }

    fn provider_index(
        entries: impl IntoIterator<Item = impl Into<String>>,
    ) -> ResourceProviderIndex {
        ResourceProviderIndex::try_from_entries(
            entries.into_iter().map(|path| {
                let path = path.into();
                ResourceProviderEntry::new(EpubPath::new(path).unwrap(), None)
            }),
            &ResourceProviderIndexLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn resource_index_coalesces_declarations_and_preserves_spine_occurrences() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata />
            <manifest>
                <item id="a" href="chapter.xhtml" media-type="application/xhtml+xml" properties="scripted" />
                <item id="b" href="./chapter.xhtml" media-type="image/svg+xml" />
                <item id="missing" media-type="text/css" />
            </manifest>
            <spine>
                <itemref idref="a" linear="no" />
                <itemref idref="a" />
                <itemref idref="unknown" />
            </spine>
        </package>"#,
        );
        let provider =
            provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml", "EPUB/orphan.bin"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        assert_eq!(index.declarations().len(), 3);
        let first = index.find_unique_resource_by_id("a").unwrap();
        let second = index.find_unique_resource_by_id("b").unwrap();
        assert_eq!(first.key(), second.key());
        assert_eq!(first.declarations().len(), 2);
        assert_eq!(first.declarations().next().unwrap().resource(), Some(first));
        assert!(first.has_xhtml_declaration());
        assert!(first.has_svg_declaration());
        assert!(first.is_scripted());
        assert!(matches!(
            index.find_unique_by_id("missing").unwrap().target_row(),
            DeclarationTargetRow::MissingHref
        ));

        let order = index.reading_order().collect::<Vec<_>>();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0].linear(), Linear::No);
        assert_eq!(order[1].linear(), Linear::Yes);
        assert_eq!(
            order[0].declaration().unwrap().ordinal(),
            ManifestOrdinal(0)
        );
        assert_eq!(order[0].resource(), Some(first));
        assert!(matches!(
            order[2].target_row(),
            ReadingOrderTargetRow::MissingManifestId
        ));
        assert!(index.resources().any(|record| {
            record
                .local_path()
                .is_some_and(|path| path.as_str() == "EPUB/orphan.bin")
                && !record.is_manifest_resource()
        }));
    }

    #[test]
    fn resource_ordinals_are_topology_positions() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml" /></manifest><spine />
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/a.xhtml"]);
        let first = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();
        let clone = first.clone();
        let rebuilt = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();
        let ordinal = first.find_unique_resource_by_id("a").unwrap().ordinal();

        assert_eq!(
            clone.resource(ordinal).unwrap().address(),
            first.resource(ordinal).unwrap().address()
        );
        assert_eq!(
            rebuilt.resource(ordinal).unwrap().address(),
            first.resource(ordinal).unwrap().address()
        );
    }

    #[test]
    fn resource_index_models_local_remote_and_invalid_declarations() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest>
                <item id="local" href="missing.xhtml" media-type="application/xhtml+xml" />
                <item id="remote" href="https://example.com/book.xhtml" media-type="application/xhtml+xml" />
                <item id="invalid" href="../../outside.xhtml" media-type="application/xhtml+xml" />
            </manifest><spine />
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        assert_eq!(
            index
                .find_unique_resource_by_id("local")
                .unwrap()
                .presence(),
            ProviderPresence::Missing
        );
        assert_eq!(
            index
                .find_unique_resource_by_id("remote")
                .unwrap()
                .presence(),
            ProviderPresence::NotApplicable
        );
        assert!(matches!(
            index.find_unique_by_id("invalid").unwrap().target_row(),
            DeclarationTargetRow::InvalidHref(_)
        ));
    }

    #[test]
    fn resource_index_preserves_invalid_manifest_ids() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest>
                <item id="1chapter" href="chapter.xhtml" media-type="application/xhtml+xml" />
                <item id="chapter:name" href="colon.xhtml" media-type="application/xhtml+xml" />
                <item id="章节·一" href="unicode.xhtml" media-type="application/xhtml+xml" />
                <item id="  spaced  " href="spaced.xhtml" media-type="application/xhtml+xml" />
                <item id="chapter name" href="internal-space.xhtml" media-type="application/xhtml+xml" />
            </manifest><spine><itemref idref="1chapter" /></spine>
        </package>"#,
        );
        assert_eq!(package.manifest().items()[4].id(), Some("chapter name"));
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        let declarations = index.declarations().collect::<Vec<_>>();
        assert!(matches!(
            declarations[0].id(),
            ManifestIdValue::Invalid(value) if value == "1chapter"
        ));
        assert!(matches!(
            declarations[1].id(),
            ManifestIdValue::Invalid(value) if value == "chapter:name"
        ));
        assert!(matches!(
            declarations[2].id(),
            ManifestIdValue::Valid(value) if value.as_str() == "章节·一"
        ));
        assert!(matches!(
            declarations[3].id(),
            ManifestIdValue::Valid(value) if value.as_str() == "spaced"
        ));
        assert!(matches!(
            declarations[4].id(),
            ManifestIdValue::Invalid(value) if value == "chapter name"
        ));
        assert!(matches!(
            index.reading_order().next().unwrap().target_row(),
            ReadingOrderTargetRow::MissingManifestId
        ));
    }

    #[test]
    fn duplicate_ids_only_coalesce_for_physical_resource_lookup() {
        let same = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest>
            <item id="dup" href="same.xhtml" media-type="application/xhtml+xml"/>
            <item id="dup" href="./same.xhtml" media-type="application/xhtml+xml"/>
            </manifest><spine/></package>"#,
        );
        let different = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest>
            <item id="dup" href="one.xhtml" media-type="application/xhtml+xml"/>
            <item id="dup" href="two.xhtml" media-type="application/xhtml+xml"/>
            </manifest><spine/></package>"#,
        );
        let provider = provider_index([
            "EPUB/package.opf",
            "EPUB/same.xhtml",
            "EPUB/one.xhtml",
            "EPUB/two.xhtml",
        ]);
        let same = ResourceIndex::new(&same, "EPUB/package.opf", &provider).unwrap();
        let different = ResourceIndex::new(&different, "EPUB/package.opf", &provider).unwrap();

        assert!(matches!(
            same.find_unique_by_id("dup"),
            Err(ResourceLookupError::AmbiguousManifestId { .. })
        ));
        assert_eq!(
            same.find_unique_resource_by_id("dup")
                .unwrap()
                .local_path()
                .unwrap()
                .as_str(),
            "EPUB/same.xhtml"
        );
        assert!(matches!(
            different.find_unique_by_id("dup"),
            Err(ResourceLookupError::AmbiguousManifestId { .. })
        ));
        assert!(matches!(
            different.find_unique_resource_by_id("dup"),
            Err(ResourceLookupError::AmbiguousManifestId { .. })
        ));
    }

    #[test]
    fn multiple_cover_image_declarations_are_explicitly_ambiguous() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest>
            <item id="one" href="one.jpg" media-type="image/jpeg" properties="cover-image"/>
            <item id="two" href="two.jpg" media-type="image/jpeg" properties="cover-image"/>
            </manifest><spine/></package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/one.jpg", "EPUB/two.jpg"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        assert!(index.cover_image().is_none());
        assert!(matches!(
            index.select(&ResourceSelector::CoverImage),
            Err(ResourceLookupError::Ambiguous { ref candidates, .. }) if candidates.len() == 2
        ));
        assert!(matches!(
            index.facts().selections.cover,
            facts::SelectionFacts::AmbiguousCandidateDeclarations { ref candidates, .. } if candidates.len() == 2
        ));
    }

    #[test]
    fn reading_order_presentation_retains_provenance_precedence_and_ambiguity() {
        let package = parse_package(
            r##"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata>
                <meta property="rendition:layout">roll</meta>
                <meta property="rendition:flow">paginated</meta>
                <meta property="rendition:flow">paginated</meta>
                <meta property="rendition:orientation" />
                <meta property="rendition:orientation">portrait</meta>
                <meta property="rendition:spread">none</meta>
                <meta property="rendition:layout" refines="#chapter">pre-paginated</meta>
            </metadata>
            <manifest>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" />
            </manifest>
            <spine page-progression-direction="default">
                <itemref idref="chapter" properties="RENDITION:LAYOUT-PRE-PAGINATED custom:value rendition:layout-reflowable rendition:flow-sideways" />
                <itemref idref="chapter" />
                <itemref idref="missing" properties="RENDITION:PAGE-SPREAD-RIGHT" />
            </spine>
        </package>"##,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();
        let entries = index.reading_order().collect::<Vec<_>>();

        assert_eq!(
            entries[0]
                .properties()
                .iter()
                .map(SpinePropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec![
                "RENDITION:LAYOUT-PRE-PAGINATED",
                "custom:value",
                "rendition:layout-reflowable",
                "rendition:flow-sideways"
            ]
        );
        let overridden = entries[0].presentation().layout().candidates();
        assert_eq!(overridden.len(), 2);
        assert_eq!(
            overridden[0].source(),
            RenditionValueSource::ItemRefProperty
        );
        assert_eq!(
            overridden[0].authored_value().map(EpubString::as_str),
            Some("RENDITION:LAYOUT-PRE-PAGINATED")
        );
        assert_eq!(overridden[0].value(), Some(&RenditionLayout::PrePaginated));
        assert_eq!(entries[0].presentation().layout().value(), None);
        let malformed_override = entries[0].presentation().flow().candidate().unwrap();
        assert_eq!(
            malformed_override.source(),
            RenditionValueSource::ItemRefProperty
        );
        assert_eq!(malformed_override.value(), None);
        assert_eq!(
            malformed_override.authored_value().map(EpubString::as_str),
            Some("rendition:flow-sideways")
        );

        let inherited = entries[1].presentation();
        assert_eq!(inherited.layout().value(), Some(&RenditionLayout::Roll));
        let layout_candidate = inherited.layout().candidate().unwrap();
        assert_eq!(
            layout_candidate.source(),
            RenditionValueSource::PackageMetadata
        );
        assert_eq!(
            layout_candidate.authored_value().map(EpubString::as_str),
            Some("roll")
        );
        assert_eq!(inherited.flow().candidates().len(), 2);
        assert_eq!(inherited.flow().value(), None);
        assert_eq!(inherited.orientation().candidates().len(), 2);
        assert_eq!(
            inherited.orientation().candidates()[0].authored_value(),
            None
        );
        assert_eq!(inherited.orientation().candidates()[0].value(), None);
        assert_eq!(
            inherited.orientation().candidates()[1].value(),
            Some(&RenditionOrientation::Portrait)
        );
        assert_eq!(inherited.spread().value(), Some(&RenditionSpread::None));
        assert!(inherited.page_spread().is_unspecified());
        assert_eq!(
            inherited.page_progression_direction(),
            Some(PageProgressionDirection::Default)
        );

        assert!(matches!(
            entries[2].target_row(),
            ReadingOrderTargetRow::MissingManifestId
        ));
        assert_eq!(
            entries[2].presentation().layout().value(),
            Some(&RenditionLayout::Roll)
        );
        assert_eq!(
            entries[2].presentation().page_spread().value(),
            Some(&PageSpread::Right)
        );
    }

    #[test]
    fn reading_order_presentation_is_owned_by_cloned_and_rebuilt_snapshots() {
        let mut package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata><meta property="rendition:layout">reflowable</meta></metadata>
            <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
            <spine><itemref idref="chapter" /></spine>
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let original = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();
        let detached_clone = original.clone();

        package.metadata_mut().add_meta(Meta::new(
            crate::package::metadata::MetaPropertyToken::known(KnownMetaProperty::RenditionLayout),
            EpubString::new("pre-paginated").unwrap(),
        ));
        let rebuilt = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        assert_eq!(
            original
                .reading_order()
                .next()
                .unwrap()
                .presentation()
                .layout()
                .value(),
            Some(&RenditionLayout::Reflowable)
        );
        assert_eq!(
            detached_clone
                .reading_order()
                .next()
                .unwrap()
                .presentation(),
            original.reading_order().next().unwrap().presentation()
        );
        assert_eq!(
            rebuilt
                .reading_order()
                .next()
                .unwrap()
                .presentation()
                .layout()
                .candidates()
                .len(),
            2
        );
    }

    #[test]
    fn manifest_id_validation_normalizes_only_xml_whitespace() {
        assert_eq!(
            normalize_manifest_id("  Chapter-One  ").unwrap(),
            "Chapter-One"
        );
        assert_eq!(normalize_manifest_id("\t章节·一\r").unwrap(), "章节·一");
        assert!(normalize_manifest_id("a\u{301}").is_ok());
        assert!(normalize_manifest_id(" \t\n ").is_err());
        assert!(normalize_manifest_id("1chapter").is_err());
        assert!(normalize_manifest_id("chapter:name").is_err());
        assert!(normalize_manifest_id("chapter name").is_err());
        assert!(normalize_manifest_id("chapter\tname").is_err());
        assert!(normalize_manifest_id("\u{A0}chapter").is_err());
    }

    #[test]
    fn relationship_ids_preserve_source_and_use_only_xml_whitespace() {
        let package = parse_package(
            "<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"2.0\"><metadata>\
             <meta name=\"cover\" content=\" cover \"/></metadata><manifest>\
             <item id=\" chapter \" href=\"chapter.xhtml\" media-type=\"application/xhtml+xml\" fallback=\" fallback \" media-overlay=\" overlay \"/>\
             <item id=\"fallback\" href=\"fallback.xhtml\" media-type=\"application/xhtml+xml\"/>\
             <item id=\"overlay\" href=\"overlay.smil\" media-type=\"application/smil+xml\"/>\
             <item id=\"cover\" href=\"cover.jpg\" media-type=\"image/jpeg\"/>\
             <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\
             <item id=\"\u{a0}bad\" href=\"bad.xhtml\" media-type=\"application/xhtml+xml\" fallback=\"\u{a0}fallback\" media-overlay=\"\u{a0}overlay\"/>\
             </manifest><spine toc=\" ncx \"><itemref idref=\" chapter \"/><itemref idref=\"\u{a0}chapter\"/></spine></package>",
        );
        let item = &package.manifest().items()[0];
        assert_eq!(item.id(), Some(" chapter "));
        assert_eq!(item.fallback(), Some(" fallback "));
        assert_eq!(item.media_overlay(), Some(" overlay "));
        assert_eq!(package.spine().toc(), Some(" ncx "));
        assert_eq!(package.metadata().opf2_cover_id(), Some(" cover "));
        assert_eq!(package.manifest().items()[5].id(), Some("\u{a0}bad"));

        let index = ResourceIndex::new(
            &package,
            "EPUB/package.opf",
            &provider_index(["EPUB/package.opf"]),
        )
        .unwrap();
        assert!(matches!(
            index.reading_order().next().unwrap().target(),
            ReadingOrderTarget::Declaration { .. }
        ));
        assert_eq!(
            index.reading_order().nth(1).unwrap().target(),
            ReadingOrderTarget::MissingManifestId
        );
        let facts = index.facts();
        assert!(matches!(
            facts.selections.cover,
            facts::SelectionFacts::Selected { .. }
        ));
        assert!(matches!(
            facts.selections.ncx,
            facts::SelectionFacts::Selected { .. }
        ));
    }

    #[test]
    fn duplicate_cover_properties_are_ambiguous_in_package_and_index() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest>
            <item id="one" href="one.jpg" media-type="image/jpeg" properties="cover-image"/>
            <item id="two" href="two.jpg" media-type="image/jpeg" properties="cover-image"/>
            </manifest><spine/></package>"#,
        );
        assert!(matches!(
            package.resource_selections().cover,
            PackageSelection::Ambiguous { ref candidates, .. } if candidates == &[0, 1]
        ));
        let index = ResourceIndex::new(
            &package,
            "EPUB/package.opf",
            &provider_index(["EPUB/package.opf"]),
        )
        .unwrap();
        assert!(matches!(
            index.facts().selections.cover,
            facts::SelectionFacts::AmbiguousCandidateDeclarations { ref candidates, .. }
                if candidates.len() == 2
        ));
    }

    #[test]
    fn ordinal_lookups_return_linked_borrowed_views() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="chapter"/></spine></package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider).unwrap();

        let declaration = index.declaration(ManifestOrdinal(0)).unwrap();
        let resource = declaration.resource().unwrap();
        let occurrence = index.occurrence(ReadingOrderOrdinal(0)).unwrap();
        assert_eq!(index.resource(resource.ordinal()).unwrap(), resource);
        assert_eq!(resource.declarations().next(), Some(declaration));
        assert_eq!(occurrence.declaration(), Some(declaration));
        assert_eq!(occurrence.resource(), Some(resource));
        assert!(index.resource(ResourceOrdinal(u32::MAX)).is_err());
        assert!(index.declaration(ManifestOrdinal(u32::MAX)).is_err());
        assert!(index.occurrence(ReadingOrderOrdinal(u32::MAX)).is_err());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn topology_rejects_indices_beyond_public_ordinals() {
        assert!(
            ensure_ordinal_count(u32::MAX as usize + 1, ResourceTopologyKind::Resources).is_ok()
        );
        assert_eq!(
            ensure_ordinal_count(u32::MAX as usize + 2, ResourceTopologyKind::Resources)
                .unwrap_err()
                .kind(),
            ResourceTopologyKind::Resources
        );
    }

    #[test]
    fn canonical_href_corpus_preserves_authored_text_and_never_repairs() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Kind {
            Empty,
            Invalid,
            Local,
            SameDocument,
            Remote,
            Data,
            External,
        }

        let cases = [
            ("", Kind::Empty),
            (" chapter.xhtml", Kind::Invalid),
            ("chapter.xhtml ", Kind::Invalid),
            ("chapter\\part.xhtml", Kind::Invalid),
            ("chapter%", Kind::Invalid),
            ("chapter%2", Kind::Invalid),
            ("chapter%GG", Kind::Invalid),
            ("chapter%2Fpart.xhtml", Kind::Invalid),
            ("chapter%5cpart.xhtml", Kind::Invalid),
            ("/chapter.xhtml", Kind::Invalid),
            ("chapter.xhtml", Kind::Local),
            ("chapter%20one.xhtml", Kind::Local),
            ("chapter.xhtml?view=full", Kind::Local),
            ("?view=full", Kind::Local),
            ("#part", Kind::SameDocument),
            ("#", Kind::SameDocument),
            ("HTTP://example.com/book.xhtml?q=1#part", Kind::Remote),
            ("//example.com/book.xhtml", Kind::Remote),
            ("DATA:text/plain,hello#part", Kind::Data),
            ("mailto:reader@example.com?subject=EPUB", Kind::External),
        ];

        for (authored, expected) in cases {
            let parsed = parse_href(AuthoredHref::new(authored));
            let actual = match &parsed {
                ParsedHref::Empty { .. } => Kind::Empty,
                ParsedHref::Invalid { .. } => Kind::Invalid,
                ParsedHref::Local { .. } => Kind::Local,
                ParsedHref::SameDocument { .. } => Kind::SameDocument,
                ParsedHref::Remote { .. } => Kind::Remote,
                ParsedHref::Data { .. } => Kind::Data,
                ParsedHref::ExternalScheme { .. } => Kind::External,
            };
            assert_eq!(actual, expected, "authored href: {authored:?}");
            assert_eq!(parsed.original().as_str(), authored);
        }

        assert!(AuthoredHref::new(" chapter.xhtml").to_epub_href().is_none());
        assert!(
            AuthoredHref::new("chapter\\part.xhtml")
                .to_epub_href()
                .is_none()
        );
        assert!(matches!(
            parse_href(AuthoredHref::new("#frag")),
            ParsedHref::SameDocument { ref fragment, .. } if fragment == "frag"
        ));
        assert!(matches!(
            parse_href(AuthoredHref::new("data:image/svg+xml;base64,AAAA#icon")),
            ParsedHref::Data {
                fragment: Some(ref fragment),
                ..
            } if fragment == "icon"
        ));
    }

    #[test]
    fn canonical_href_corpus_resolves_paths_queries_and_fragments() {
        let source = EpubPath::new("EPUB/text/current.xhtml").unwrap();
        let cases = [
            ("chapter.xhtml", "EPUB/text/chapter.xhtml", None),
            ("chapter%20one.xhtml", "EPUB/text/chapter one.xhtml", None),
            ("../chapter.xhtml?view=full", "EPUB/chapter.xhtml", None),
            ("?view=full", "EPUB/text/current.xhtml", None),
            ("#part", "EPUB/text/current.xhtml", Some("part")),
            ("#part%20one", "EPUB/text/current.xhtml", Some("part one")),
            ("#", "EPUB/text/current.xhtml", None),
        ];

        for (authored, expected_path, expected_fragment) in cases {
            let resolved = target_for_href(&AuthoredHref::new(authored), &source);
            match (resolved, expected_fragment) {
                (ResolvedHref::Resource(ResourceAddress::Local(path)), None) => {
                    assert_eq!(path.as_str(), expected_path, "authored href: {authored:?}");
                }
                (
                    ResolvedHref::Fragment {
                        resource: ResourceAddress::Local(path),
                        fragment,
                        ..
                    },
                    Some(expected_fragment),
                ) => {
                    assert_eq!(path.as_str(), expected_path, "authored href: {authored:?}");
                    assert_eq!(fragment, expected_fragment);
                }
                (actual, _) => panic!("unexpected resolution for {authored:?}: {actual:?}"),
            }
        }

        for authored in [
            "../../../outside.xhtml",
            "%2e%2e/%2e%2e/%2e%2e/outside.xhtml",
        ] {
            let resolved = target_for_href(&AuthoredHref::new(authored), &source);
            assert!(
                matches!(
                    resolved,
                    ResolvedHref::Invalid(ref value) if value == authored
                ),
                "authored href {authored:?} resolved as {resolved:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn epub_path_rejects_non_unicode_native_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(b"EPUB/chapter-\xff.xhtml".to_vec()));

        assert_eq!(EpubPath::new(path), Err(EpubPathError::NonUtf8));
    }

    #[test]
    fn epub_path_rejects_noncanonical_identity_forms() {
        let cases = [
            ("", EpubPathError::Empty),
            ("/EPUB/chapter.xhtml", EpubPathError::Absolute),
            ("EPUB\\chapter.xhtml", EpubPathError::Backslash),
            ("EPUB/chapter.xhtml#part", EpubPathError::QueryOrFragment),
            (
                "EPUB/chapter.xhtml?view=full",
                EpubPathError::QueryOrFragment,
            ),
            ("EPUB//chapter.xhtml", EpubPathError::EmptySegment),
            ("EPUB/chapter.xhtml/", EpubPathError::EmptySegment),
            ("EPUB/./chapter.xhtml", EpubPathError::DotSegment),
            ("EPUB/../chapter.xhtml", EpubPathError::DotSegment),
            ("EPUB/chapter\0.xhtml", EpubPathError::ControlCharacter),
            ("custom:chapter", EpubPathError::SchemeLikeFirstSegment),
            (
                "data:text/plain,chapter",
                EpubPathError::SchemeLikeFirstSegment,
            ),
        ];

        for (value, expected) in cases {
            assert_eq!(EpubPath::new(value), Err(expected), "path: {value:?}");
        }

        assert_eq!(
            EpubPath::new("EPUB/chapitre été 100%.xhtml")
                .unwrap()
                .as_str(),
            "EPUB/chapitre été 100%.xhtml"
        );
        assert_eq!(
            EpubPath::new("EPUB/custom:chapter.xhtml").unwrap().as_str(),
            "EPUB/custom:chapter.xhtml"
        );
    }
}
