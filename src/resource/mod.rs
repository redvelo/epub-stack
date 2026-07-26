//! Find, identify, and resolve resources used by an EPUB publication.
//!
//! Use [`ResourceIndex`] to browse package declarations, local publication files, and reading
//! order. [`EpubPath`] identifies a local resource, while the href APIs resolve relative links
//! found in EPUB documents.

/// Storage adapters and bounded inventories used when opening a publication.
pub mod provider;

/// Canonical authored media type representation.
pub use crate::media_type::MediaType;
/// A live resource handle obtained from a publication.
pub use crate::publication::Resource;

use crate::package::{
    Package, RenditionFlow, RenditionLayout, RenditionOrientation, RenditionSpread,
    manifest::{KnownManifestProperty, ManifestItem, ManifestPropertyToken},
    metadata::{KnownMetaProperty, Meta},
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
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, thiserror::Error)]
/// Failure to select exactly one resource or manifest declaration.
pub enum ResourceLookupError {
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
        ManifestKey,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// A valid manifest XML ID used for lookup.
///
/// Construction trims surrounding whitespace, then requires an XML-name-like identifier:
/// an alphabetic character or `_`, followed by alphanumeric characters, `_`, `-`, or `.`.
/// Colons are not accepted.
pub struct ResourceId(String);

impl ResourceId {
    /// Trims and validates an identifier, returning `None` when it is not accepted.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let value = value.as_ref().trim();
        is_xml_id(value).then(|| Self(value.to_string()))
    }

    /// Returns the validated, surrounding-whitespace-trimmed identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_xml_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '-' || ch == '.' || ch.is_alphanumeric())
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ResourceId> for String {
    fn from(value: ResourceId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
struct ResourceIndexId(u64);

impl ResourceIndexId {
    fn fresh() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

macro_rules! index_key {
    ($name:ident, $docs:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[doc = $docs]
        pub struct $name {
            owner: ResourceIndexId,
            slot: u32,
        }
    };
}

index_key!(
    ResourceKey,
    "Opaque identity of one resolved resource within a particular [`ResourceIndex`].\n\nCloning an index preserves this identity. Rebuilding an index creates a new owner, so keys must not be persisted across publication edits or mixed between unrelated indexes."
);
index_key!(
    ManifestKey,
    "Opaque identity of one manifest declaration within a particular [`ResourceIndex`].\n\nDeclaration identity is distinct from resolved resource identity: duplicate href declarations have different `ManifestKey` values but may share one [`ResourceKey`]. Cloning preserves keys; rebuilding invalidates them."
);
index_key!(
    ReadingOrderKey,
    "Opaque identity of one spine occurrence within a particular [`ResourceIndex`].\n\nRepeated `idref` values remain distinct occurrences. Cloning preserves keys; rebuilding an index after an edit creates a new identity."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// Failure to use an opaque key with an index-owned query.
pub enum IndexKeyError {
    /// The key was created by another or rebuilt index.
    #[error("key belongs to a different resource index")]
    ForeignIndex,
    /// The key has this index owner but its slot does not identify an entry.
    #[error("key does not identify an entry in this resource index")]
    UnknownKey,
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
    /// A present ID accepted as [`ResourceId`].
    Valid(
        /// Validated identifier.
        ResourceId,
    ),
    /// No `id` attribute was present.
    Missing,
    /// A present ID was preserved but did not satisfy [`ResourceId`] syntax.
    Invalid(
        /// Exact malformed authored ID.
        String,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolution state of one manifest declaration's `href`.
pub enum DeclarationTarget {
    /// The href resolved to canonical resource identity.
    Resource(
        /// Resolved resource identity.
        ResourceKey,
    ),
    /// The declaration had no `href` attribute.
    MissingHref,
    /// The exact href was present but could not resolve to an accepted address.
    InvalidHref(
        /// Exact authored href that failed resolution.
        AuthoredHref,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One authored manifest item, kept distinct from its resolved resource.
///
/// Multiple declarations may resolve to the same [`ResourceKey`] while retaining their own
/// IDs, href spelling, media types, properties, and relationships.
pub struct ManifestDeclaration {
    key: ManifestKey,
    id: ManifestIdValue,
    href: Option<AuthoredHref>,
    target: DeclarationTarget,
    media_type: Option<MediaType>,
    properties: Vec<ManifestPropertyToken>,
    fallback: Option<AuthoredIdRef>,
    media_overlay: Option<AuthoredIdRef>,
}

impl ManifestDeclaration {
    /// Returns this declaration's index-local identity.
    pub fn key(&self) -> ManifestKey {
        self.key
    }
    /// Returns the authored ID state.
    pub fn id(&self) -> &ManifestIdValue {
        &self.id
    }
    /// Returns the exact authored href when the attribute was present.
    pub fn href(&self) -> Option<&AuthoredHref> {
        self.href.as_ref()
    }
    /// Returns the declaration's resource-resolution state.
    pub fn target(&self) -> &DeclarationTarget {
        &self.target
    }
    /// Returns the raw-preserving media type declaration when present.
    ///
    /// A returned [`MediaType`] may contain malformed MIME syntax; use
    /// [`MediaType::is_valid`] before relying on parsed MIME facts.
    pub fn media_type(&self) -> Option<&MediaType> {
        self.media_type.as_ref()
    }
    /// Returns manifest property tokens in authored order.
    pub fn properties(&self) -> &[ManifestPropertyToken] {
        &self.properties
    }
    /// Returns the exact authored fallback IDREF when present.
    pub fn fallback(&self) -> Option<&AuthoredIdRef> {
        self.fallback.as_ref()
    }
    /// Returns the exact authored media-overlay IDREF when present.
    pub fn media_overlay(&self) -> Option<&AuthoredIdRef> {
        self.media_overlay.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub struct ResourceRecord {
    key: ResourceKey,
    address: ResourceAddress,
    declarations: Vec<ManifestKey>,
    presence: ProviderPresence,
    metadata: ResourceMetadata,
    has_xhtml: bool,
    has_stylesheet: bool,
    has_svg: bool,
    scripted: bool,
}

impl ResourceRecord {
    /// Returns this resource's index-local identity.
    pub fn key(&self) -> ResourceKey {
        self.key
    }
    /// Returns the canonical resolved address.
    pub fn address(&self) -> &ResourceAddress {
        &self.address
    }
    /// Returns the provider path when this is local.
    pub fn local_path(&self) -> Option<&EpubPath> {
        self.address.local_path()
    }
    /// Returns the URL when this is an HTTP(S) or scheme-relative remote resource.
    pub fn remote_url(&self) -> Option<&str> {
        self.address.remote_url()
    }
    /// Returns every manifest declaration resolving to this resource, in manifest order.
    pub fn declarations(&self) -> &[ManifestKey] {
        &self.declarations
    }
    /// Returns provider membership for this address.
    pub fn presence(&self) -> ProviderPresence {
        self.presence
    }
    /// Returns path and provider metadata available without reading content.
    pub fn metadata(&self) -> &ResourceMetadata {
        &self.metadata
    }
    /// Reports whether any declaration has valid XHTML MIME essence.
    pub fn has_xhtml_declaration(&self) -> bool {
        self.has_xhtml
    }
    /// Reports whether any declaration has valid CSS MIME essence.
    pub fn has_stylesheet_declaration(&self) -> bool {
        self.has_stylesheet
    }
    /// Reports whether any declaration has valid SVG MIME essence.
    pub fn has_svg_declaration(&self) -> bool {
        self.has_svg
    }
    /// Reports whether any declaration has the manifest `scripted` property.
    pub fn is_scripted(&self) -> bool {
        self.scripted
    }
    /// Reports whether at least one manifest declaration resolves to this resource.
    pub fn is_manifest_resource(&self) -> bool {
        !self.declarations.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolution state of one reading-order occurrence.
pub enum ReadingOrderTarget {
    /// The IDREF uniquely selected a declaration.
    Declaration {
        /// Selected manifest declaration.
        declaration: ManifestKey,
        /// Its resolved resource, absent when that declaration has no usable href.
        resource: Option<ResourceKey>,
    },
    /// The spine itemref had no `idref` attribute.
    MissingIdref,
    /// The authored IDREF matched no valid manifest ID.
    MissingManifestId,
    /// The authored IDREF matched multiple manifest declarations.
    AmbiguousManifestId {
        /// Matching declarations in manifest order.
        candidates: Vec<ManifestKey>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// The authored source of a rendition presentation candidate.
pub enum RenditionValueSource {
    /// The candidate came from an unrefined package metadata `meta` element.
    PackageMetadata,
    /// The candidate came from a spine `itemref` property token.
    ItemRefProperty,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
pub struct ReadingOrderEntry {
    key: ReadingOrderKey,
    index: usize,
    idref: Option<AuthoredIdRef>,
    target: ReadingOrderTarget,
    linear: Linear,
    properties: Vec<SpinePropertyToken>,
    presentation: ReadingOrderPresentation,
}

impl ReadingOrderEntry {
    /// Returns this occurrence's index-local identity.
    pub fn key(&self) -> ReadingOrderKey {
        self.key
    }
    /// Returns the zero-based authored spine position.
    pub fn index(&self) -> usize {
        self.index
    }
    /// Returns the exact authored IDREF when present.
    pub fn idref(&self) -> Option<&AuthoredIdRef> {
        self.idref.as_ref()
    }
    /// Returns the occurrence's declaration/resource resolution state.
    pub fn target(&self) -> &ReadingOrderTarget {
        &self.target
    }
    /// Returns effective linearity; missing or malformed source defaults to [`Linear::Yes`].
    pub fn linear(&self) -> Linear {
        self.linear
    }
    /// Returns cloned itemref property tokens in authored order.
    pub fn properties(&self) -> &[SpinePropertyToken] {
        &self.properties
    }
    /// Returns this occurrence's owned presentation snapshot.
    pub fn presentation(&self) -> &ReadingOrderPresentation {
        &self.presentation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An identity included in an ambiguous resource lookup result.
pub enum ResourceLookupCandidate {
    /// A resolved resource identity.
    Resource(
        /// Candidate resource identity.
        ResourceKey,
    ),
    /// A manifest declaration identity.
    Manifest(
        /// Candidate manifest declaration identity.
        ManifestKey,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// The resource inventory and lookup table for one publication state.
///
/// Manifest declarations, resolved resources, and reading-order occurrences have separate
/// identities. Cloning this index, including into publication analysis, preserves its private
/// owner and all keys. Rebuilding after an edit creates a new owner; old keys then return
/// [`IndexKeyError::ForeignIndex`]. Keys are therefore snapshot-local handles, not durable
/// locators. Construction uses complete provider enumeration and performs no broad content
/// parsing. Use [`crate::Epub::analyze`] when content-derived facts are needed.
pub struct ResourceIndex {
    id: ResourceIndexId,
    declarations: Vec<ManifestDeclaration>,
    resources: Vec<ResourceRecord>,
    reading_order: Vec<ReadingOrderEntry>,
    by_id: HashMap<String, Vec<ManifestKey>>,
    by_address: HashMap<ResourceAddress, ResourceKey>,
    package: ResourceKey,
    epub_nav: Option<ResourceKey>,
    ncx: Option<ResourceKey>,
    cover_image: Option<ResourceKey>,
}

impl ResourceIndex {
    pub(crate) fn new(
        package: &Package,
        package_path: impl AsRef<Path>,
        provider_index: &ResourceProviderIndex,
    ) -> Self {
        let id = ResourceIndexId::fresh();
        let package_path = EpubPath::new(package_path).expect("package path is canonical");
        let package_address = ResourceAddress::Local(package_path.clone());
        let package_key = ResourceKey { owner: id, slot: 0 };
        let package_entry = provider_index.get(&package_path);
        let mut resources = vec![ResourceRecord {
            key: package_key,
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
        let mut by_id = HashMap::<String, Vec<ManifestKey>>::new();

        for item in package.manifest().items() {
            let key = ManifestKey {
                owner: id,
                slot: declarations.len() as u32,
            };
            let id_value = match item.id() {
                Some(value) => ResourceId::new(value.as_str())
                    .map(ManifestIdValue::Valid)
                    .unwrap_or_else(|| ManifestIdValue::Invalid(value.as_str().to_string())),
                None => ManifestIdValue::Missing,
            };
            if let ManifestIdValue::Valid(value) = &id_value {
                by_id
                    .entry(value.as_str().to_string())
                    .or_default()
                    .push(key);
            }
            let href = item.authored_href().cloned();
            let target = match href.as_ref() {
                None => DeclarationTarget::MissingHref,
                Some(href) => {
                    match address_for_parsed_href(&parse_href(href.clone()), &package_path) {
                        Some(address) if !matches!(address, ResourceAddress::Invalid(_)) => {
                            let resource_key = if let Some(key) = by_address.get(&address).copied()
                            {
                                key
                            } else {
                                let key = ResourceKey {
                                    owner: id,
                                    slot: resources.len() as u32,
                                };
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
                                resources.push(ResourceRecord {
                                    key,
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
                            DeclarationTarget::Resource(resource_key)
                        }
                        _ => DeclarationTarget::InvalidHref(href.clone()),
                    }
                }
            };
            if let DeclarationTarget::Resource(resource_key) = target {
                let record = &mut resources[resource_key.slot as usize];
                record.declarations.push(key);
                record.has_xhtml |= item.media_type().is_some_and(MediaType::is_xhtml);
                record.has_stylesheet |= item.media_type().is_some_and(MediaType::is_css);
                record.has_svg |= item.media_type().is_some_and(MediaType::is_svg);
                record.scripted |= item.has_property(KnownManifestProperty::Scripted);
            }
            declarations.push(ManifestDeclaration {
                key,
                id: id_value,
                href,
                target,
                media_type: item.media_type().cloned(),
                properties: item.properties().to_vec(),
                fallback: item
                    .fallback()
                    .map(|value| AuthoredIdRef::new(value.as_str())),
                media_overlay: item
                    .media_overlay()
                    .map(|value| AuthoredIdRef::new(value.as_str())),
            });
        }

        for entry in provider_index.publication_entries() {
            let address = ResourceAddress::Local(entry.path().clone());
            if by_address.contains_key(&address) {
                continue;
            }
            let key = ResourceKey {
                owner: id,
                slot: resources.len() as u32,
            };
            resources.push(ResourceRecord {
                key,
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

        let reading_order = package
            .spine()
            .itemrefs()
            .iter()
            .enumerate()
            .map(|(index, itemref)| {
                let key = ReadingOrderKey {
                    owner: id,
                    slot: index as u32,
                };
                let idref = itemref
                    .idref()
                    .map(|value| AuthoredIdRef::new(value.as_str()));
                let target = match idref.as_ref().and_then(|value| by_id.get(value.as_str())) {
                    None if idref.is_none() => ReadingOrderTarget::MissingIdref,
                    None => ReadingOrderTarget::MissingManifestId,
                    Some(keys) if keys.len() > 1 => ReadingOrderTarget::AmbiguousManifestId {
                        candidates: keys.clone(),
                    },
                    Some(keys) => {
                        let declaration = keys[0];
                        let resource = match declarations[declaration.slot as usize].target {
                            DeclarationTarget::Resource(key) => Some(key),
                            DeclarationTarget::MissingHref | DeclarationTarget::InvalidHref(_) => {
                                None
                            }
                        };
                        ReadingOrderTarget::Declaration {
                            declaration,
                            resource,
                        }
                    }
                };
                ReadingOrderEntry {
                    key,
                    index,
                    idref,
                    target,
                    linear: itemref.linear(),
                    properties: itemref.properties().to_vec(),
                    presentation: reading_order_presentation(package, itemref),
                }
            })
            .collect();

        let key_for_item = |selected: Option<&ManifestItem>| {
            selected.and_then(|selected| {
                package
                    .manifest()
                    .items()
                    .iter()
                    .position(|item| std::ptr::eq(item, selected))
                    .and_then(|slot| match declarations[slot].target {
                        DeclarationTarget::Resource(key) => Some(key),
                        _ => None,
                    })
            })
        };
        let epub_nav = key_for_item(package.nav_item());
        let ncx = key_for_item(package.ncx_item());
        let cover_image = key_for_item(package.cover_image_item());
        Self {
            id,
            declarations,
            resources,
            reading_order,
            by_id,
            by_address,
            package: package_key,
            epub_nav,
            ncx,
            cover_image,
        }
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
    pub fn declarations(&self) -> &[ManifestDeclaration] {
        &self.declarations
    }
    /// Returns all distinct resource records in deterministic index order.
    pub fn resources(&self) -> &[ResourceRecord] {
        &self.resources
    }
    /// Iterates every authored spine occurrence in order.
    pub fn reading_order(&self) -> impl Iterator<Item = &ReadingOrderEntry> {
        self.reading_order.iter()
    }
    /// Iterates resources having at least one manifest declaration.
    pub fn manifest_resources(&self) -> impl Iterator<Item = &ResourceRecord> {
        self.resources
            .iter()
            .filter(|record| record.is_manifest_resource())
    }
    /// Resolves a resource key owned by this index snapshot.
    pub fn resource(&self, key: ResourceKey) -> Result<&ResourceRecord, IndexKeyError> {
        self.check_owner(key.owner)?;
        self.resources
            .get(key.slot as usize)
            .ok_or(IndexKeyError::UnknownKey)
    }
    /// Resolves a manifest key owned by this index snapshot.
    pub fn declaration(&self, key: ManifestKey) -> Result<&ManifestDeclaration, IndexKeyError> {
        self.check_owner(key.owner)?;
        self.declarations
            .get(key.slot as usize)
            .ok_or(IndexKeyError::UnknownKey)
    }
    /// Resolves a reading-order key owned by this index snapshot.
    pub fn reading_order_entry(
        &self,
        key: ReadingOrderKey,
    ) -> Result<&ReadingOrderEntry, IndexKeyError> {
        self.check_owner(key.owner)?;
        self.reading_order
            .get(key.slot as usize)
            .ok_or(IndexKeyError::UnknownKey)
    }
    /// Iterates declarations whose valid manifest ID exactly equals `value`.
    ///
    /// Invalid authored IDs are preserved on declarations but are not indexed by this query.
    pub fn declarations_with_id<'a>(
        &'a self,
        value: &'a str,
    ) -> impl Iterator<Item = &'a ManifestDeclaration> + 'a {
        self.by_id
            .get(value)
            .into_iter()
            .flatten()
            .filter_map(|key| self.declarations.get(key.slot as usize))
    }
    /// Iterates the resource at an exact resolved address.
    ///
    /// Address identity is unique, so this iterator yields zero or one record; duplicate
    /// declarations remain available through [`ResourceRecord::declarations`].
    pub fn resources_at<'a>(
        &'a self,
        address: &ResourceAddress,
    ) -> impl Iterator<Item = &'a ResourceRecord> + 'a {
        self.by_address
            .get(address)
            .and_then(|key| self.resources.get(key.slot as usize))
            .into_iter()
    }
    /// Selects exactly one declaration by valid manifest ID.
    ///
    /// The lookup reports duplicate IDs as ambiguity and does not require the declaration's
    /// href to resolve to a resource.
    pub fn find_unique_by_id(
        &self,
        value: &str,
    ) -> Result<&ManifestDeclaration, ResourceLookupError> {
        let selector = ResourceSelector::Id(
            ResourceId::new(value).unwrap_or_else(|| ResourceId(value.to_string())),
        );
        let keys = self.by_id.get(value).map(Vec::as_slice).unwrap_or_default();
        match keys {
            [] => Err(ResourceLookupError::NotFound(selector)),
            [key] => Ok(&self.declarations[key.slot as usize]),
            keys => Err(ResourceLookupError::Ambiguous {
                selector,
                candidates: keys
                    .iter()
                    .copied()
                    .map(ResourceLookupCandidate::Manifest)
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
    ) -> Result<&ResourceRecord, ResourceLookupError> {
        let declaration = self.find_unique_by_id(value)?;
        match declaration.target {
            DeclarationTarget::Resource(key) => Ok(&self.resources[key.slot as usize]),
            DeclarationTarget::MissingHref | DeclarationTarget::InvalidHref(_) => {
                Err(ResourceLookupError::UnresolvedDeclaration(declaration.key))
            }
        }
    }
    /// Returns the loaded package document resource.
    pub fn package(&self) -> &ResourceRecord {
        &self.resources[self.package.slot as usize]
    }
    /// Returns the package-selected EPUB navigation resource, when one resolved.
    pub fn epub_nav(&self) -> Option<&ResourceRecord> {
        self.epub_nav
            .and_then(|key| self.resources.get(key.slot as usize))
    }
    /// Returns the package-selected NCX resource, when one resolved.
    pub fn ncx(&self) -> Option<&ResourceRecord> {
        self.ncx
            .and_then(|key| self.resources.get(key.slot as usize))
    }
    /// Returns the package-selected cover image resource, when one resolved.
    pub fn cover_image(&self) -> Option<&ResourceRecord> {
        self.cover_image
            .and_then(|key| self.resources.get(key.slot as usize))
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
    ) -> Result<&ResourceRecord, ResourceLookupError> {
        let key = match selector {
            ResourceSelector::Id(id) => return self.find_unique_resource_by_id(id.as_str()),
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
            ResourceSelector::EpubNav => self.epub_nav.as_ref(),
            ResourceSelector::CoverImage => self.cover_image.as_ref(),
        };
        key.and_then(|key| self.resources.get(key.slot as usize))
            .ok_or_else(|| ResourceLookupError::NotFound(selector.clone()))
    }
    fn select_resolved(
        &self,
        selector: &ResourceSelector,
        resolved: ResolvedHref,
    ) -> Result<&ResourceRecord, ResourceLookupError> {
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
            .and_then(|key| self.resources.get(key.slot as usize))
            .ok_or_else(|| ResourceLookupError::NotFound(selector.clone()))
    }
    fn check_owner(&self, owner: ResourceIndexId) -> Result<(), IndexKeyError> {
        if owner == self.id {
            Ok(())
        } else {
            Err(IndexKeyError::ForeignIndex)
        }
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
        candidates: Vec<ResourceId>,
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
    /// Select by unique valid manifest ID.
    Id(
        /// Valid manifest ID to select uniquely.
        ResourceId,
    ),
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

impl From<ResourceId> for ResourceSelector {
    fn from(value: ResourceId) -> Self {
        Self::Id(value)
    }
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
    /// Creates an ID selector after trimming and validating the identifier.
    pub fn id(value: impl AsRef<str>) -> Option<Self> {
        ResourceId::new(value).map(Self::Id)
    }

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
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider);

        assert_eq!(index.declarations().len(), 3);
        let first = index.find_unique_resource_by_id("a").unwrap();
        let second = index.find_unique_resource_by_id("b").unwrap();
        assert_eq!(first.key(), second.key());
        assert_eq!(first.declarations().len(), 2);
        assert!(first.has_xhtml_declaration());
        assert!(first.has_svg_declaration());
        assert!(first.is_scripted());
        assert!(matches!(
            index.find_unique_by_id("missing").unwrap().target(),
            DeclarationTarget::MissingHref
        ));

        let order = index.reading_order().collect::<Vec<_>>();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0].linear(), Linear::No);
        assert_eq!(order[1].linear(), Linear::Yes);
        assert!(matches!(
            order[2].target(),
            ReadingOrderTarget::MissingManifestId
        ));
        assert!(index.resources().iter().any(|record| {
            record
                .local_path()
                .is_some_and(|path| path.as_str() == "EPUB/orphan.bin")
                && !record.is_manifest_resource()
        }));
    }

    #[test]
    fn resource_index_clones_retain_keys_and_rebuilds_replace_them() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml" /></manifest><spine />
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/a.xhtml"]);
        let first = ResourceIndex::new(&package, "EPUB/package.opf", &provider);
        let clone = first.clone();
        let rebuilt = ResourceIndex::new(&package, "EPUB/package.opf", &provider);
        let key = first.find_unique_resource_by_id("a").unwrap().key();

        assert_eq!(
            clone.resource(key).unwrap().address(),
            first.resource(key).unwrap().address()
        );
        assert_eq!(rebuilt.resource(key), Err(IndexKeyError::ForeignIndex));
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
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider);

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
            index.find_unique_by_id("invalid").unwrap().target(),
            DeclarationTarget::InvalidHref(_)
        ));
    }

    #[test]
    fn resource_index_preserves_invalid_manifest_ids() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest>
                <item id="1chapter" href="chapter.xhtml" media-type="application/xhtml+xml" />
            </manifest><spine><itemref idref="1chapter" /></spine>
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider);

        assert!(matches!(
            index.declarations()[0].id(),
            ManifestIdValue::Invalid(value) if value == "1chapter"
        ));
        assert!(matches!(
            index.reading_order().next().unwrap().target(),
            ReadingOrderTarget::MissingManifestId
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
        let index = ResourceIndex::new(&package, "EPUB/package.opf", &provider);
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
            entries[2].target(),
            ReadingOrderTarget::MissingManifestId
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
        let original = ResourceIndex::new(&package, "EPUB/package.opf", &provider);
        let detached_clone = original.clone();

        package.metadata_mut().add_meta(Meta::new(
            crate::package::metadata::MetaPropertyToken::known(KnownMetaProperty::RenditionLayout),
            EpubString::new("pre-paginated").unwrap(),
        ));
        let rebuilt = ResourceIndex::new(&package, "EPUB/package.opf", &provider);

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
    fn resource_id_normalizes_only_surrounding_whitespace() {
        let id = ResourceId::new("  Chapter-One  ").unwrap();

        assert_eq!(id.as_str(), "Chapter-One");
        assert_eq!(ResourceId::new(" \t\n "), None);
        assert_eq!(ResourceId::new("1chapter"), None);
        assert_eq!(ResourceId::new("chapter:name"), None);
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
