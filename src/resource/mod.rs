//! Find, identify, and resolve resources used by an EPUB publication.
//!
//! Use [`ResourceIndex`] to browse package declarations, local publication files, and reading
//! order. [`EpubPath`] identifies a local resource, while the href APIs resolve relative links
//! found in EPUB documents.

pub(crate) mod base;
mod href;
pub mod provider;

/// Canonical authored media type representation.
pub use crate::media_type::MediaType;
pub use href::{InvalidHref, ResolvedHref, resolve_href, resolve_publication_href};
pub(crate) use href::{ParsedHref, parse_href, resolve_local_href_from_source};

use crate::package::{
    Package, ReadingOrderPresentation,
    manifest::{KnownManifestProperty, ManifestPropertyToken},
    normalize_manifest_id,
    spine::{Linear, SpinePropertyToken},
};
use provider::{ProviderIndex, ProviderReadError};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
/// Failure to read bytes or text for a publication resource.
pub enum ResourceReadError {
    /// The path has no current committed bytes in the publication.
    #[error("resource is missing: {path}")]
    Missing {
        /// Canonical local path that was requested.
        path: EpubPath,
    },
    /// The provider failed while reading.
    #[error(transparent)]
    Provider(ProviderReadError),
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
            ProviderReadError::Missing { path } => Self::Missing { path },
            error => Self::Provider(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// A file's path inside an EPUB, such as `EPUB/text/chapter.xhtml`.
///
/// Paths are relative, `/`-separated, and compared exactly. Nothing is cleaned up for you: a
/// path with `..`, a query, a fragment, or a doubled slash is rejected rather than repaired.
pub struct EpubPath(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
/// Reason a path cannot serve as canonical EPUB resource identity.
pub enum EpubPathError {
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
    /// Validates exact canonical path text without repairing it.
    pub fn new(value: impl AsRef<str>) -> Result<Self, EpubPathError> {
        let value = value.as_ref();
        validate_epub_path(value)?;
        Ok(Self(value.to_string()))
    }

    /// Returns the exact canonical UTF-8 path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the text after the last `.` of the final segment, when present.
    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name();
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map(|(_, extension)| extension)
    }

    /// Reports whether this is the OCF `mimetype` entry or lies below `META-INF/`.
    pub fn is_ocf_control(&self) -> bool {
        self.0 == "mimetype" || self.0.starts_with("META-INF/")
    }

    pub(crate) fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.as_str(), |(_, name)| name)
    }

    pub(crate) fn parent_dir(&self) -> &str {
        self.0.rsplit_once('/').map_or("", |(parent, _)| parent)
    }
}

impl FromStr for EpubPath {
    type Err = EpubPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for EpubPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// An href exactly as the book wrote it, including empty or malformed ones.
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
/// An href whose syntax is valid.
///
/// Valid syntax is not a promise that anything is there. [`resolve_href`] turns one into the
/// address it points at.
pub struct EpubHref(String);

impl EpubHref {
    /// Validates exact href text without trimming or repairing it.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, EpubHrefError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(EpubHrefError::Empty);
        }
        if !href::valid_href_syntax(value) {
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
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "kind", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Where a resource lives: a path in the container, or a URL the book points out to.
pub enum ResourceAddress {
    /// A canonical path in the publication provider.
    Local(
        /// Canonical provider-relative path.
        EpubPath,
    ),
    /// An HTTP(S) or scheme-relative URL.
    Remote(
        /// Remote URL text excluding any fragment.
        String,
    ),
    /// A `data:` URL retained as authored.
    Data(
        /// Exact authored data URL.
        String,
    ),
    /// A syntactically valid URL using another scheme.
    External(
        /// Authored URL using another scheme, excluding any fragment.
        String,
    ),
}

impl ResourceAddress {
    /// Returns the provider path for a local address.
    pub fn local_path(&self) -> Option<&EpubPath> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_) | Self::Data(_) | Self::External(_) => None,
        }
    }

    /// Returns the URL for an HTTP(S) or scheme-relative remote address.
    pub fn remote_url(&self) -> Option<&str> {
        match self {
            Self::Remote(url) => Some(url),
            Self::Local(_) | Self::Data(_) | Self::External(_) => None,
        }
    }

    /// Returns the address text suitable for display or source inspection.
    pub fn display_value(&self) -> &str {
        match self {
            Self::Local(path) => path.as_str(),
            Self::Remote(value) | Self::Data(value) | Self::External(value) => value,
        }
    }
}

/// A resource-index collection whose length cannot be represented by 32-bit ordinals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceIndexCollection {
    /// Distinct physical resources.
    Resources,
    /// Manifest declarations.
    ManifestDeclarations,
    /// Reading-order occurrences.
    ReadingOrderOccurrences,
}

/// A resource index has a collection that cannot be represented by its public ordinal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resource index collection {collection:?} exceeds 32-bit ordinals")]
pub struct ResourceIndexError {
    collection: ResourceIndexCollection,
}

impl ResourceIndexError {
    /// Returns the collection that exceeded the ordinal range.
    pub fn collection(self) -> ResourceIndexCollection {
        self.collection
    }
}

macro_rules! ordinal {
    ($name:ident, $docs:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(
            feature = "serde",
            derive(serde::Serialize, serde::Deserialize),
            serde(transparent)
        )]
        #[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
        #[doc = $docs]
        pub struct $name(u32);

        impl $name {
            /// Rebuilds a position that travelled outside the crate, such as one read back from
            /// serialized facts.
            ///
            /// A position means nothing on its own: it addresses the index it came from, and
            /// using it against another selects an unrelated entry or none at all.
            pub const fn new(position: u32) -> Self {
                Self(position)
            }

            pub(crate) fn from_index(index: usize) -> Self {
                Self(u32::try_from(index).expect("resource index count was checked"))
            }

            /// The zero-based position.
            pub fn index(self) -> usize {
                self.0 as usize
            }

            /// The position as it serializes.
            pub const fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

ordinal!(
    ResourceOrdinal,
    "Where a resource sits in a [`ResourceIndex`]."
);
ordinal!(
    ManifestOrdinal,
    "Where a declaration sits in the package manifest."
);
ordinal!(ReadingOrderOrdinal, "Where an entry sits in the spine.");

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
/// An IDREF exactly as the book wrote it, including ones that match nothing.
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
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "state", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// What a manifest item wrote for its `id`, including nothing at all.
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

/// Resolution of one authored manifest IDREF against the declarations of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(tag = "state", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum IdrefTarget {
    /// The IDREF named exactly one declaration.
    Declaration(
        /// Selected declaration position.
        ManifestOrdinal,
    ),
    /// The IDREF is not a well-formed ID.
    Invalid,
    /// The IDREF is well-formed but names nothing.
    Missing,
    /// The IDREF names more than one declaration, because the manifest reuses an ID.
    ///
    /// The candidates are not serialized: one ID shared by many declarations would make the
    /// encoded index grow with the square of the manifest. Match the authored IDREF against the
    /// declaration IDs to recover them.
    Ambiguous(
        /// Matching declaration positions in manifest order.
        #[cfg_attr(feature = "serde", serde(skip))]
        #[cfg_attr(feature = "specta", specta(skip))]
        Vec<ManifestOrdinal>,
    ),
}

impl IdrefTarget {
    /// Returns the uniquely selected declaration.
    pub fn declaration(&self) -> Option<ManifestOrdinal> {
        match self {
            Self::Declaration(declaration) => Some(*declaration),
            Self::Invalid | Self::Missing | Self::Ambiguous(_) => None,
        }
    }

    /// Returns the declarations an ambiguous IDREF matched, in manifest order.
    pub fn candidates(&self) -> &[ManifestOrdinal] {
        match self {
            Self::Ambiguous(candidates) => candidates,
            Self::Declaration(_) | Self::Invalid | Self::Missing => &[],
        }
    }
}

/// Resolution state of one manifest declaration in the current snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase",
        deny_unknown_fields
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum DeclarationTarget {
    /// The href resolved to a physical resource.
    Resource {
        /// Resolved resource ordinal.
        resource: ResourceOrdinal,
    },
    /// The declaration had no `href` attribute.
    MissingHref,
    /// The authored href could not resolve to an accepted address.
    InvalidHref,
}

impl DeclarationTarget {
    /// Returns the resolved resource.
    pub fn resource(self) -> Option<ResourceOrdinal> {
        match self {
            Self::Resource { resource } => Some(resource),
            Self::MissingHref | Self::InvalidHref => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Whether a resolved resource address has corresponding provider bytes.
pub enum ProviderPresence {
    /// The local path appeared in the complete provider index.
    Present {
        /// Provider-reported byte length, encoded as a decimal string for JSON safety.
        #[cfg_attr(
            feature = "serde",
            serde(
                serialize_with = "serialize_size_bytes",
                deserialize_with = "deserialize_size_bytes"
            )
        )]
        #[cfg_attr(feature = "specta", specta(type = Option<String>))]
        size_bytes: Option<u64>,
    },
    /// The local path did not appear in the complete provider index.
    Missing,
    /// Provider membership does not apply to a non-local address.
    NotApplicable,
}

impl ProviderPresence {
    /// Reports whether provider bytes are present.
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present { .. })
    }

    /// Returns the provider-reported byte length of a present resource, when known.
    pub fn size_bytes(self) -> Option<u64> {
        match self {
            Self::Present { size_bytes } => size_bytes,
            Self::Missing | Self::NotApplicable => None,
        }
    }
}

#[cfg(feature = "serde")]
#[cfg(feature = "serde")]
fn deserialize_size_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let encoded = <Option<String> as serde::Deserialize>::deserialize(deserializer)?;
    encoded
        .map(|value| value.parse::<u64>().map_err(serde::de::Error::custom))
        .transpose()
}

#[cfg(feature = "serde")]
fn serialize_size_bytes<S: serde::Serializer>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    value.map(|size| size.to_string()).serialize(serializer)
}

/// Authored relationship used to select a structural resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum SelectionSource {
    /// The package path supplied when opening the rendition.
    PackagePath,
    /// The manifest `nav` property.
    EpubNavProperty,
    /// The manifest `cover-image` property.
    CoverImageProperty,
    /// EPUB 2 `meta name="cover"` metadata.
    Opf2CoverMetadata,
    /// The spine `toc` attribute.
    SpineToc,
}

/// Explicit structural-resource selection outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase",
        deny_unknown_fields
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ResourceSelection {
    /// No applicable authored selection evidence exists.
    #[default]
    Absent,
    /// One declaration or direct resource was selected.
    Selected {
        /// Relationship responsible for the selection.
        source: SelectionSource,
        /// Selected declaration, absent for the directly selected package resource.
        declaration: Option<ManifestOrdinal>,
        /// Resolved resource, absent when a selected declaration has no usable href.
        resource: Option<ResourceOrdinal>,
    },
    /// An authored ID relationship matched no declaration.
    UnresolvedAuthoredId {
        /// Relationship containing the unresolved ID.
        source: SelectionSource,
        /// Exact modeled authored ID text.
        authored_id: String,
    },
    /// More than one declaration satisfies the authored selection relationship.
    Ambiguous {
        /// Relationship responsible for the ambiguity.
        source: SelectionSource,
        /// Candidate declaration ordinals in manifest order.
        candidates: Vec<ManifestOrdinal>,
    },
}

impl ResourceSelection {
    /// Returns the selected declaration, absent for the directly selected package resource.
    pub fn declaration(&self) -> Option<ManifestOrdinal> {
        match self {
            Self::Selected { declaration, .. } => *declaration,
            _ => None,
        }
    }

    /// Returns the resolved resource, absent when nothing was selected or the selected
    /// declaration has no usable href.
    pub fn resource(&self) -> Option<ResourceOrdinal> {
        match self {
            Self::Selected { resource, .. } => *resource,
            _ => None,
        }
    }
}

/// Explicit outcomes for publication structural-resource selections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ResourceSelections {
    /// Selected package document.
    #[cfg_attr(feature = "serde", serde(default))]
    pub package: ResourceSelection,
    /// Selected cover image.
    #[cfg_attr(feature = "serde", serde(default))]
    pub cover: ResourceSelection,
    /// Selected EPUB navigation document declaration.
    #[cfg_attr(feature = "serde", serde(default))]
    pub epub_nav: ResourceSelection,
    /// Selected EPUB 2 NCX declaration.
    #[cfg_attr(feature = "serde", serde(default))]
    pub ncx: ResourceSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
struct ManifestDeclaration {
    id: ManifestIdValue,
    href: Option<AuthoredHref>,
    target: DeclarationTarget,
    media_type: Option<MediaType>,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "specta", specta(skip))]
    properties: Vec<ManifestPropertyToken>,
    fallback: Option<AuthoredIdRef>,
    fallback_target: Option<IdrefTarget>,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "specta", specta(skip))]
    media_overlay: Option<AuthoredIdRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
struct ResourceRecord {
    address: ResourceAddress,
    presence: ProviderPresence,
    declarations: Vec<ManifestOrdinal>,
    // Derived from the declarations resolving here. A decoded index recomputes these rather than
    // trusting them, so a supplied claim cannot disagree with the manifest it came with.
    has_xhtml_declaration: bool,
    has_stylesheet_declaration: bool,
    has_svg_declaration: bool,
    scripted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase", deny_unknown_fields)
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
struct ReadingOrderOccurrence {
    idref: Option<AuthoredIdRef>,
    target: Option<IdrefTarget>,
    resource: Option<ResourceOrdinal>,
    linear: Linear,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "specta", specta(skip))]
    properties: Vec<SpinePropertyToken>,
    presentation: ReadingOrderPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Everything the publication declares and everything the container holds, resolved against
/// each other.
///
/// Two declarations of the same file are one resource.
pub struct ResourceIndex {
    package_path: EpubPath,
    cover_path: Option<EpubPath>,
    resources: Vec<ResourceRecord>,
    declarations: Vec<ManifestDeclaration>,
    reading_order: Vec<ReadingOrderOccurrence>,
    selections: ResourceSelections,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "specta", specta(skip))]
    by_id: HashMap<String, Vec<ManifestOrdinal>>,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "specta", specta(skip))]
    by_address: HashMap<ResourceAddress, ResourceOrdinal>,
}

/// One resource: where it lives, whether the container holds it, and what declares it.
#[derive(Clone, Copy)]
pub struct ResourceRef<'a> {
    index: &'a ResourceIndex,
    ordinal: ResourceOrdinal,
}

impl fmt::Debug for ResourceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceRef")
            .field("ordinal", &self.ordinal)
            .field("address", self.address())
            .finish()
    }
}

impl PartialEq for ResourceRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.ordinal == other.ordinal
    }
}

impl Eq for ResourceRef<'_> {}

impl<'a> ResourceRef<'a> {
    fn record(self) -> &'a ResourceRecord {
        &self.index.resources[self.ordinal.index()]
    }

    /// Returns this resource's position in the snapshot.
    pub fn ordinal(self) -> ResourceOrdinal {
        self.ordinal
    }

    /// Returns the canonical resolved address.
    pub fn address(self) -> &'a ResourceAddress {
        &self.record().address
    }

    /// Returns the provider path when this resource is local.
    pub fn local_path(self) -> Option<&'a EpubPath> {
        self.address().local_path()
    }

    /// Returns the URL when this is an HTTP(S) or scheme-relative resource.
    pub fn remote_url(self) -> Option<&'a str> {
        self.address().remote_url()
    }

    /// Iterates manifest declarations resolving to this resource in manifest order.
    ///
    /// Provider-only resources have none. Duplicate declarations at one address share this
    /// resource.
    pub fn declarations(self) -> impl ExactSizeIterator<Item = ManifestDeclarationRef<'a>> + 'a {
        self.record()
            .declarations
            .iter()
            .map(move |ordinal| ManifestDeclarationRef {
                index: self.index,
                ordinal: *ordinal,
            })
    }

    /// Returns provider membership and size for this address.
    pub fn presence(self) -> ProviderPresence {
        self.record().presence
    }

    pub(crate) fn declares(self, media_type: impl Fn(&MediaType) -> bool) -> bool {
        self.declarations()
            .any(|declaration| declaration.media_type().is_some_and(&media_type))
    }

    /// Reports whether any declaration resolving here identifies XHTML.
    ///
    /// Duplicate declarations may disagree; this reports that at least one supplied the
    /// classification.
    pub fn has_xhtml_declaration(self) -> bool {
        self.record().has_xhtml_declaration
    }

    /// Reports whether any declaration resolving here identifies CSS.
    pub fn has_stylesheet_declaration(self) -> bool {
        self.record().has_stylesheet_declaration
    }

    /// Reports whether any declaration resolving here identifies SVG.
    pub fn has_svg_declaration(self) -> bool {
        self.record().has_svg_declaration
    }

    /// Reports whether any declaration resolving here carries the manifest `scripted` property.
    pub fn is_scripted(self) -> bool {
        self.record().scripted
    }
}

/// One `item` in the package manifest, and the resource it resolves to.
#[derive(Clone, Copy)]
pub struct ManifestDeclarationRef<'a> {
    index: &'a ResourceIndex,
    ordinal: ManifestOrdinal,
}

impl fmt::Debug for ManifestDeclarationRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManifestDeclarationRef")
            .field("ordinal", &self.ordinal)
            .field("id", self.id())
            .finish()
    }
}

impl PartialEq for ManifestDeclarationRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.ordinal == other.ordinal
    }
}

impl Eq for ManifestDeclarationRef<'_> {}

impl<'a> ManifestDeclarationRef<'a> {
    fn record(self) -> &'a ManifestDeclaration {
        &self.index.declarations[self.ordinal.index()]
    }

    /// Returns this declaration's position in the snapshot.
    pub fn ordinal(self) -> ManifestOrdinal {
        self.ordinal
    }

    /// Returns the authored ID state.
    pub fn id(self) -> &'a ManifestIdValue {
        &self.record().id
    }

    /// Returns the exact authored href when present.
    pub fn href(self) -> Option<&'a AuthoredHref> {
        self.record().href.as_ref()
    }

    /// Returns this declaration's resolution state.
    pub fn target(self) -> DeclarationTarget {
        self.record().target
    }

    /// Returns the resolved physical resource, when available.
    pub fn resource(self) -> Option<ResourceRef<'a>> {
        self.target().resource().map(|ordinal| ResourceRef {
            index: self.index,
            ordinal,
        })
    }

    /// Returns the declared media type when present.
    ///
    /// A returned [`MediaType`] may contain malformed MIME syntax; use
    /// [`MediaType::is_valid`] before relying on parsed MIME facts.
    pub fn media_type(self) -> Option<&'a MediaType> {
        self.record().media_type.as_ref()
    }

    /// Returns manifest property tokens in authored order.
    pub fn properties(self) -> &'a [ManifestPropertyToken] {
        &self.record().properties
    }

    /// Returns the exact authored fallback IDREF when present.
    pub fn fallback(self) -> Option<&'a AuthoredIdRef> {
        self.record().fallback.as_ref()
    }

    /// Returns the resolution of the authored fallback IDREF when present.
    pub fn fallback_target(self) -> Option<&'a IdrefTarget> {
        self.record().fallback_target.as_ref()
    }

    /// Returns the exact authored media-overlay IDREF when present.
    pub fn media_overlay(self) -> Option<&'a AuthoredIdRef> {
        self.record().media_overlay.as_ref()
    }
}

/// One entry in the spine.
///
/// A book that reads the same chapter twice has two entries here, not one.
#[derive(Clone, Copy)]
pub struct ReadingOrderOccurrenceRef<'a> {
    index: &'a ResourceIndex,
    ordinal: ReadingOrderOrdinal,
}

impl fmt::Debug for ReadingOrderOccurrenceRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadingOrderOccurrenceRef")
            .field("ordinal", &self.ordinal)
            .field("idref", &self.idref())
            .finish()
    }
}

impl PartialEq for ReadingOrderOccurrenceRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.index, other.index) && self.ordinal == other.ordinal
    }
}

impl Eq for ReadingOrderOccurrenceRef<'_> {}

impl<'a> ReadingOrderOccurrenceRef<'a> {
    fn record(self) -> &'a ReadingOrderOccurrence {
        &self.index.reading_order[self.ordinal.index()]
    }

    /// Returns this occurrence's position in authored spine order.
    pub fn ordinal(self) -> ReadingOrderOrdinal {
        self.ordinal
    }

    /// Returns the exact authored IDREF when present.
    pub fn idref(self) -> Option<&'a AuthoredIdRef> {
        self.record().idref.as_ref()
    }

    /// Returns the IDREF resolution, or `None` when the itemref had no `idref`.
    pub fn target(self) -> Option<&'a IdrefTarget> {
        self.record().target.as_ref()
    }

    /// Returns the uniquely selected declaration, when available.
    pub fn declaration(self) -> Option<ManifestDeclarationRef<'a>> {
        self.target()
            .and_then(IdrefTarget::declaration)
            .map(|ordinal| ManifestDeclarationRef {
                index: self.index,
                ordinal,
            })
    }

    /// Returns the resolved physical resource, when available.
    pub fn resource(self) -> Option<ResourceRef<'a>> {
        self.record().resource.map(|ordinal| ResourceRef {
            index: self.index,
            ordinal,
        })
    }

    /// Returns effective linearity; missing or malformed source defaults to [`Linear::Yes`].
    pub fn linear(self) -> Linear {
        self.record().linear
    }

    /// Returns itemref property tokens in authored order.
    pub fn properties(self) -> &'a [SpinePropertyToken] {
        &self.record().properties
    }

    /// Returns this occurrence's presentation snapshot.
    pub fn presentation(self) -> &'a ReadingOrderPresentation {
        &self.record().presentation
    }
}

impl ResourceIndex {
    pub(crate) fn new(
        package: &Package,
        package_path: &EpubPath,
        provider_index: &ProviderIndex,
    ) -> Result<Self, ResourceIndexError> {
        let items = package.manifest().items();
        let itemrefs = package.spine().itemrefs();
        ensure_ordinal_count(items.len(), ResourceIndexCollection::ManifestDeclarations)?;
        ensure_ordinal_count(
            itemrefs.len(),
            ResourceIndexCollection::ReadingOrderOccurrences,
        )?;

        let mut index = Self {
            package_path: package_path.clone(),
            cover_path: None,
            resources: Vec::new(),
            declarations: Vec::with_capacity(items.len()),
            reading_order: Vec::with_capacity(itemrefs.len()),
            selections: ResourceSelections {
                package: ResourceSelection::Absent,
                cover: ResourceSelection::Absent,
                epub_nav: ResourceSelection::Absent,
                ncx: ResourceSelection::Absent,
            },
            by_id: HashMap::new(),
            by_address: HashMap::new(),
        };
        let package_ordinal =
            index.insert_resource(ResourceAddress::Local(package_path.clone()), provider_index)?;
        index.selections.package = ResourceSelection::Selected {
            source: SelectionSource::PackagePath,
            declaration: None,
            resource: Some(package_ordinal),
        };

        for (position, item) in items.iter().enumerate() {
            let ordinal = ManifestOrdinal::from_index(position);
            let id = match item.id() {
                Some(value) => match normalize_manifest_id(value) {
                    Ok(value) => ManifestIdValue::Valid(value.to_string()),
                    Err(_) => ManifestIdValue::Invalid(value.to_string()),
                },
                None => ManifestIdValue::Missing,
            };
            if let ManifestIdValue::Valid(value) = &id {
                index.by_id.entry(value.clone()).or_default().push(ordinal);
            }
            let href = item.authored_href().cloned();
            let target = match &href {
                None => DeclarationTarget::MissingHref,
                Some(href) => match resolve_href(href, package_path) {
                    Ok(resolved) => {
                        let resource = index.insert_resource(resolved.address, provider_index)?;
                        let record = &mut index.resources[resource.index()];
                        record.declarations.push(ordinal);
                        record.has_xhtml_declaration |=
                            item.media_type().is_some_and(MediaType::is_xhtml);
                        record.has_stylesheet_declaration |=
                            item.media_type().is_some_and(MediaType::is_css);
                        record.has_svg_declaration |=
                            item.media_type().is_some_and(MediaType::is_svg);
                        record.scripted |= item.has_property(KnownManifestProperty::Scripted);
                        DeclarationTarget::Resource { resource }
                    }
                    Err(_) => DeclarationTarget::InvalidHref,
                },
            };
            index.declarations.push(ManifestDeclaration {
                id,
                href,
                target,
                media_type: item.media_type().cloned(),
                properties: item.properties().to_vec(),
                fallback: item.fallback().map(AuthoredIdRef::new),
                fallback_target: None,
                media_overlay: item.media_overlay().map(AuthoredIdRef::new),
            });
        }
        for position in 0..index.declarations.len() {
            let target = index.declarations[position]
                .fallback
                .as_ref()
                .map(|fallback| index.resolve_idref(fallback.as_str()));
            index.declarations[position].fallback_target = target;
        }

        for entry in provider_index.publication_entries() {
            index.insert_resource(ResourceAddress::Local(entry.path.clone()), provider_index)?;
        }

        index.reading_order = itemrefs
            .iter()
            .map(|itemref| {
                let idref = itemref.idref().map(AuthoredIdRef::new);
                let target = idref
                    .as_ref()
                    .map(|idref| index.resolve_idref(idref.as_str()));
                ReadingOrderOccurrence {
                    resource: target.as_ref().and_then(IdrefTarget::declaration).and_then(
                        |declaration| index.declarations[declaration.index()].target.resource(),
                    ),
                    target,
                    idref,
                    linear: itemref.linear(),
                    properties: itemref.properties().to_vec(),
                    presentation: ReadingOrderPresentation::of(package, itemref),
                }
            })
            .collect();

        let (cover, epub_nav, ncx) = package.resource_selections();
        let resolve = |selection: ResourceSelection| match selection {
            ResourceSelection::Selected {
                source,
                declaration: Some(declaration),
                ..
            } => ResourceSelection::Selected {
                source,
                declaration: Some(declaration),
                resource: index.declarations[declaration.index()].target.resource(),
            },
            selection => selection,
        };
        index.selections.cover = resolve(cover);
        index.selections.epub_nav = resolve(epub_nav);
        index.selections.ncx = resolve(ncx);
        index.cover_path = index
            .cover_image()
            .filter(|resource| resource.presence().is_present())
            .and_then(|resource| resource.local_path().cloned());
        Ok(index)
    }

    fn insert_resource(
        &mut self,
        address: ResourceAddress,
        provider_index: &ProviderIndex,
    ) -> Result<ResourceOrdinal, ResourceIndexError> {
        if let Some(ordinal) = self.by_address.get(&address) {
            return Ok(*ordinal);
        }
        ensure_ordinal_count(self.resources.len() + 1, ResourceIndexCollection::Resources)?;
        let ordinal = ResourceOrdinal::from_index(self.resources.len());
        let presence = match address.local_path() {
            Some(path) => provider_index
                .get(path)
                .map_or(ProviderPresence::Missing, |entry| {
                    ProviderPresence::Present {
                        size_bytes: entry.size_bytes,
                    }
                }),
            None => ProviderPresence::NotApplicable,
        };
        self.resources.push(ResourceRecord {
            address: address.clone(),
            presence,
            declarations: Vec::new(),
            has_xhtml_declaration: false,
            has_stylesheet_declaration: false,
            has_svg_declaration: false,
            scripted: false,
        });
        self.by_address.insert(address, ordinal);
        Ok(ordinal)
    }

    pub(crate) fn resolve_idref(&self, value: &str) -> IdrefTarget {
        let Ok(value) = normalize_manifest_id(value) else {
            return IdrefTarget::Invalid;
        };
        match self.by_id.get(value).map(Vec::as_slice).unwrap_or_default() {
            [] => IdrefTarget::Missing,
            [ordinal] => IdrefTarget::Declaration(*ordinal),
            candidates => IdrefTarget::Ambiguous(candidates.to_vec()),
        }
    }

    /// Every `item` in the package manifest, in authored order.
    pub fn declarations(&self) -> impl ExactSizeIterator<Item = ManifestDeclarationRef<'_>> {
        (0..self.declarations.len()).map(|position| ManifestDeclarationRef {
            index: self,
            ordinal: ManifestOrdinal::from_index(position),
        })
    }

    /// Every distinct resource in the publication, one per address.
    ///
    /// `mimetype` and `META-INF` are container files, not resources. Declared targets that are
    /// remote, or that the container doesn't hold, are.
    pub fn resources(&self) -> impl ExactSizeIterator<Item = ResourceRef<'_>> {
        (0..self.resources.len()).map(|position| ResourceRef {
            index: self,
            ordinal: ResourceOrdinal::from_index(position),
        })
    }

    /// Every entry in the spine, in reading order.
    ///
    /// An entry whose `idref` names nothing, or names more than one declaration, still appears
    /// here with no resource.
    pub fn reading_order(&self) -> impl ExactSizeIterator<Item = ReadingOrderOccurrenceRef<'_>> {
        (0..self.reading_order.len()).map(|position| ReadingOrderOccurrenceRef {
            index: self,
            ordinal: ReadingOrderOrdinal::from_index(position),
        })
    }

    /// Only the resources the manifest declares, skipping files the container merely holds.
    pub fn manifest_resources(&self) -> impl Iterator<Item = ResourceRef<'_>> {
        self.resources()
            .filter(|resource| resource.declarations().len() > 0)
    }

    /// The resource at this position, or `None` if the position is past the end.
    ///
    /// Positions belong to the index that produced them. One from a different index points at a
    /// different resource, or at nothing.
    pub fn resource(&self, ordinal: ResourceOrdinal) -> Option<ResourceRef<'_>> {
        (ordinal.index() < self.resources.len()).then_some(ResourceRef {
            index: self,
            ordinal,
        })
    }

    /// The declaration at this position, or `None` if the position is past the end.
    ///
    /// Positions belong to the index that produced them.
    pub fn declaration(&self, ordinal: ManifestOrdinal) -> Option<ManifestDeclarationRef<'_>> {
        (ordinal.index() < self.declarations.len()).then_some(ManifestDeclarationRef {
            index: self,
            ordinal,
        })
    }

    /// The spine entry at this position, or `None` if the position is past the end.
    ///
    /// Positions belong to the index that produced them.
    pub fn occurrence(
        &self,
        ordinal: ReadingOrderOrdinal,
    ) -> Option<ReadingOrderOccurrenceRef<'_>> {
        (ordinal.index() < self.reading_order.len()).then_some(ReadingOrderOccurrenceRef {
            index: self,
            ordinal,
        })
    }

    /// Returns the resource at an exact resolved address.
    pub fn resource_at(&self, address: &ResourceAddress) -> Option<ResourceRef<'_>> {
        self.by_address.get(address).map(|ordinal| ResourceRef {
            index: self,
            ordinal: *ordinal,
        })
    }

    /// Returns the local resource at an exact canonical path.
    pub fn resource_by_path(&self, path: &EpubPath) -> Option<ResourceRef<'_>> {
        self.resource_at(&ResourceAddress::Local(path.clone()))
    }

    /// Iterates declarations whose valid manifest ID exactly equals `value`.
    ///
    /// Invalid authored IDs are preserved on declarations but are not indexed by this query.
    pub fn declarations_with_id<'a>(
        &'a self,
        value: &str,
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
            .map(|ordinal| ManifestDeclarationRef {
                index: self,
                ordinal: *ordinal,
            }))
    }

    /// Selects the one declaration with this manifest ID.
    ///
    /// Returns `None` for invalid ID syntax, no match, and duplicate IDs alike.
    /// [`Self::declarations_with_id`] tells those apart: it reports invalid syntax as an error,
    /// no match as an empty iterator, and ambiguity as more than one candidate. The declaration's
    /// href need not resolve to a resource.
    pub fn declaration_by_id(&self, value: &str) -> Option<ManifestDeclarationRef<'_>> {
        let id = normalize_manifest_id(value).ok()?;
        match self.resolve_idref(id) {
            IdrefTarget::Declaration(ordinal) => Some(ManifestDeclarationRef {
                index: self,
                ordinal,
            }),
            IdrefTarget::Ambiguous(_) | IdrefTarget::Invalid | IdrefTarget::Missing => None,
        }
    }

    /// Returns the loaded package document resource.
    pub fn package(&self) -> ResourceRef<'_> {
        ResourceRef {
            index: self,
            ordinal: self
                .selections
                .package
                .resource()
                .expect("package resource is always selected"),
        }
    }

    /// The cover image's path, ready to read.
    ///
    /// `None` when the book declares no cover, or declares one that is remote, unresolved, or
    /// absent from the container. [`Self::selections`] says which of those it was.
    pub fn cover_path(&self) -> Option<&EpubPath> {
        self.cover_path.as_ref()
    }

    /// The path of the package document this rendition was opened from.
    pub fn package_path(&self) -> &EpubPath {
        &self.package_path
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

    /// Returns structural selection outcomes, including unresolved and ambiguous selections.
    pub fn selections(&self) -> &ResourceSelections {
        &self.selections
    }

    pub(crate) fn epub_nav_declaration(&self) -> Option<ManifestOrdinal> {
        self.selections.epub_nav.declaration()
    }

    pub(crate) fn ncx_declaration(&self) -> Option<ManifestOrdinal> {
        self.selections.ncx.declaration()
    }

    /// Resolves an href written in the package document to the address it points at.
    ///
    /// The address need not exist; nothing is read to check.
    pub fn resolve_manifest_href(
        &self,
        href: impl AsRef<str>,
    ) -> Result<ResolvedHref, InvalidHref> {
        resolve_href(&AuthoredHref::new(href.as_ref()), self.package_path())
    }

    fn selected_resource(&self, selection: &ResourceSelection) -> Option<ResourceRef<'_>> {
        selection.resource().map(|ordinal| ResourceRef {
            index: self,
            ordinal,
        })
    }
}

fn ensure_ordinal_count(
    count: usize,
    collection: ResourceIndexCollection,
) -> Result<(), ResourceIndexError> {
    if count
        .checked_sub(1)
        .is_none_or(|last| u32::try_from(last).is_ok())
    {
        Ok(())
    } else {
        Err(ResourceIndexError { collection })
    }
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

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EpubPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EpubHref {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ResourceIndex {
    /// Rebuilds the lookup tables, which are derived from the inventory rather than encoded.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Encoded {
            package_path: EpubPath,
            // Accepted and ignored: the cover path is recomputed from the selection below.
            #[serde(default)]
            #[allow(dead_code)]
            cover_path: Option<EpubPath>,
            resources: Vec<ResourceRecord>,
            declarations: Vec<ManifestDeclaration>,
            reading_order: Vec<ReadingOrderOccurrence>,
            // Selections are authored evidence: an omitted one is absent, not invalid.
            #[serde(default)]
            selections: ResourceSelections,
        }

        let mut encoded = Encoded::deserialize(deserializer)?;
        for record in &mut encoded.resources {
            let declares = |select: fn(&ManifestDeclaration) -> bool| {
                record.declarations.iter().any(|ordinal| {
                    encoded
                        .declarations
                        .get(ordinal.index())
                        .is_some_and(select)
                })
            };
            record.has_xhtml_declaration = declares(|declaration| {
                declaration
                    .media_type
                    .as_ref()
                    .is_some_and(MediaType::is_xhtml)
            });
            record.has_stylesheet_declaration = declares(|declaration| {
                declaration
                    .media_type
                    .as_ref()
                    .is_some_and(MediaType::is_css)
            });
            record.has_svg_declaration = declares(|declaration| {
                declaration
                    .media_type
                    .as_ref()
                    .is_some_and(MediaType::is_svg)
            });
            record.scripted = declares(|declaration| {
                declaration
                    .properties
                    .iter()
                    .any(|property| property.known_value() == Some(KnownManifestProperty::Scripted))
            });
        }
        // Ordinals address this index. A decoded one that points outside it would panic the
        // borrowed views, so the index a payload describes has to exist before it is built.
        let declaration_count = encoded.declarations.len();
        let resource_count = encoded.resources.len();
        let declaration_exists = |ordinal: &ManifestOrdinal| ordinal.index() < declaration_count;
        let resource_exists = |ordinal: &ResourceOrdinal| ordinal.index() < resource_count;
        let idref_declaration_exists = |target: &IdrefTarget| match target {
            IdrefTarget::Declaration(declaration) => declaration_exists(declaration),
            IdrefTarget::Invalid | IdrefTarget::Missing | IdrefTarget::Ambiguous(_) => true,
        };
        let in_range = encoded
            .resources
            .iter()
            .all(|record| record.declarations.iter().all(declaration_exists))
            && encoded.declarations.iter().all(|declaration| {
                (match &declaration.target {
                    DeclarationTarget::Resource { resource } => resource_exists(resource),
                    DeclarationTarget::MissingHref | DeclarationTarget::InvalidHref => true,
                }) && declaration
                    .fallback_target
                    .as_ref()
                    .is_none_or(idref_declaration_exists)
            })
            && encoded.reading_order.iter().all(|occurrence| {
                occurrence
                    .target
                    .as_ref()
                    .is_none_or(idref_declaration_exists)
                    && occurrence.resource.as_ref().is_none_or(resource_exists)
            })
            && [
                &encoded.selections.package,
                &encoded.selections.cover,
                &encoded.selections.epub_nav,
                &encoded.selections.ncx,
            ]
            .iter()
            .all(|selection| {
                selection
                    .declaration()
                    .as_ref()
                    .is_none_or(declaration_exists)
                    && selection.resource().as_ref().is_none_or(resource_exists)
            });
        if !in_range {
            return Err(serde::de::Error::custom(
                "resource index ordinal is outside the inventory it addresses",
            ));
        }

        let mut by_id = HashMap::<String, Vec<ManifestOrdinal>>::new();
        for (position, declaration) in encoded.declarations.iter().enumerate() {
            if let ManifestIdValue::Valid(value) = &declaration.id {
                by_id
                    .entry(value.clone())
                    .or_default()
                    .push(ManifestOrdinal::from_index(position));
            }
        }
        let by_address = encoded
            .resources
            .iter()
            .enumerate()
            .map(|(position, record)| {
                (
                    record.address.clone(),
                    ResourceOrdinal::from_index(position),
                )
            })
            .collect();

        let cover_path = encoded
            .selections
            .cover
            .resource()
            .and_then(|ordinal| encoded.resources.get(ordinal.index()))
            .filter(|record| record.presence.is_present())
            .and_then(|record| record.address.local_path().cloned());

        Ok(Self {
            package_path: encoded.package_path,
            cover_path,
            resources: encoded.resources,
            declarations: encoded.declarations,
            reading_order: encoded.reading_order,
            selections: encoded.selections,
            by_id,
            by_address,
        })
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for IdrefTarget {
    /// Accepts an ambiguous outcome without candidates, which is how the index encodes it.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(
            tag = "state",
            content = "value",
            rename_all = "kebab-case",
            deny_unknown_fields
        )]
        enum Encoded {
            Declaration(ManifestOrdinal),
            Invalid,
            Missing,
            Ambiguous,
        }

        Ok(match Encoded::deserialize(deserializer)? {
            Encoded::Declaration(declaration) => Self::Declaration(declaration),
            Encoded::Invalid => Self::Invalid,
            Encoded::Missing => Self::Missing,
            Encoded::Ambiguous => Self::Ambiguous(Vec::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{
        RenditionCandidate, RenditionLayout, RenditionOrientation, RenditionSetting,
        RenditionSpread, RenditionValueSource,
        metadata::{KnownMetaProperty, Meta},
        spine::{PageProgressionDirection, PageSpread},
    };
    use crate::publication::EpubOpenLimits;
    use crate::string::EpubString;

    fn parse_package(xml: &str) -> Package {
        Package::parse(xml).unwrap()
    }

    fn provider_index(entries: impl IntoIterator<Item = &'static str>) -> ProviderIndex {
        ProviderIndex::build(
            entries
                .into_iter()
                .map(|path| (EpubPath::new(path).unwrap(), None)),
            &EpubOpenLimits::default(),
        )
        .unwrap()
    }

    fn index(package: &Package, provider: &ProviderIndex) -> ResourceIndex {
        ResourceIndex::new(
            package,
            &EpubPath::new("EPUB/package.opf").unwrap(),
            provider,
        )
        .unwrap()
    }

    fn resource_by_id<'a>(index: &'a ResourceIndex, id: &str) -> ResourceRef<'a> {
        index.declaration_by_id(id).unwrap().resource().unwrap()
    }

    #[test]
    fn declaration_predicates_report_any_declaring_media_type_or_property() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata />
            <manifest>
                <item id="a" href="chapter.xhtml" media-type="application/xhtml+xml" properties="scripted" />
                <item id="b" href="./chapter.xhtml" media-type="image/svg+xml" />
                <item id="style" href="style.css" media-type="text/css" />
                <item id="image" href="cover.png" media-type="image/png" />
            </manifest>
            <spine />
        </package>"#,
        );
        let provider = provider_index([
            "EPUB/package.opf",
            "EPUB/chapter.xhtml",
            "EPUB/style.css",
            "EPUB/cover.png",
        ]);
        let index = index(&package, &provider);

        let chapter = resource_by_id(&index, "a");
        assert!(chapter.has_xhtml_declaration());
        assert!(chapter.has_svg_declaration());
        assert!(!chapter.has_stylesheet_declaration());
        assert!(chapter.is_scripted());

        let style = resource_by_id(&index, "style");
        assert!(style.has_stylesheet_declaration());
        assert!(!style.has_xhtml_declaration());
        assert!(!style.is_scripted());

        let image = resource_by_id(&index, "image");
        assert!(!image.has_xhtml_declaration());
        assert!(!image.has_svg_declaration());
        assert!(!image.has_stylesheet_declaration());
        assert!(!image.is_scripted());
    }

    #[test]
    fn ordinals_recover_from_positions_carried_outside_the_crate() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata />
            <manifest>
                <item id="a" href="chapter.xhtml" media-type="application/xhtml+xml" />
            </manifest>
            <spine><itemref idref="a" /></spine>
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = index(&package, &provider);

        let declaration = index.declaration_by_id("a").unwrap();
        let recovered = ManifestOrdinal::new(declaration.ordinal().index() as u32);
        assert_eq!(index.declaration(recovered), Some(declaration));

        let resource = declaration.resource().unwrap();
        let recovered = ResourceOrdinal::new(resource.ordinal().index() as u32);
        assert_eq!(index.resource(recovered), Some(resource));

        let occurrence = index.reading_order().next().unwrap();
        let recovered = ReadingOrderOrdinal::new(occurrence.ordinal().index() as u32);
        assert_eq!(index.occurrence(recovered), Some(occurrence));
    }

    #[test]
    fn standalone_href_resolution_matches_the_index_resolver() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata />
            <manifest>
                <item id="a" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
            </manifest>
            <spine />
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/text/chapter.xhtml"]);
        let index = index(&package, &provider);
        let package_path = EpubPath::new("EPUB/package.opf").unwrap();

        let free = resolve_href(&AuthoredHref::new("text/chapter.xhtml"), &package_path).unwrap();
        assert_eq!(index.resolve_manifest_href("text/chapter.xhtml"), Ok(free));

        let fragment =
            resolve_href(&AuthoredHref::new("text/chapter.xhtml#p1"), &package_path).unwrap();
        assert_eq!(
            fragment.address,
            ResourceAddress::Local(EpubPath::new("EPUB/text/chapter.xhtml").unwrap())
        );
        assert_eq!(fragment.fragment.as_deref(), Some("p1"));

        let source = EpubPath::new("EPUB/text/chapter.xhtml").unwrap();
        let sibling = resolve_href(&AuthoredHref::new("../images/cover.png"), &source).unwrap();
        assert_eq!(
            sibling.address,
            ResourceAddress::Local(EpubPath::new("EPUB/images/cover.png").unwrap())
        );

        assert!(resolve_href(&AuthoredHref::new(""), &package_path).is_err());
        assert!(resolve_href(&AuthoredHref::new("../../escape.xhtml"), &package_path).is_err());
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
        let index = index(&package, &provider);

        assert_eq!(index.declarations().len(), 3);
        let first = resource_by_id(&index, "a");
        let second = resource_by_id(&index, "b");
        assert_eq!(first.ordinal(), second.ordinal());
        assert_eq!(first.declarations().len(), 2);
        assert_eq!(first.declarations().next().unwrap().resource(), Some(first));
        assert!(first.declares(MediaType::is_xhtml));
        assert!(first.declares(MediaType::is_svg));
        assert_eq!(
            index.declaration_by_id("missing").unwrap().target(),
            DeclarationTarget::MissingHref
        );

        let order = index.reading_order().collect::<Vec<_>>();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0].linear(), Linear::No);
        assert_eq!(order[1].linear(), Linear::Yes);
        assert_eq!(order[0].declaration().unwrap().ordinal().index(), 0);
        assert_eq!(order[0].resource(), Some(first));
        assert_eq!(order[2].target(), Some(&IdrefTarget::Missing));
        assert!(index.resources().any(|record| {
            record
                .local_path()
                .is_some_and(|path| path.as_str() == "EPUB/orphan.bin")
                && record.declarations().len() == 0
        }));
    }

    #[test]
    fn resource_ordinals_are_index_positions() {
        let package = parse_package(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata /><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml" /></manifest><spine />
        </package>"#,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/a.xhtml"]);
        let first = index(&package, &provider);
        let clone = first.clone();
        let rebuilt = index(&package, &provider);
        let ordinal = resource_by_id(&first, "a").ordinal();

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
        let index = index(&package, &provider);

        assert_eq!(
            resource_by_id(&index, "local").presence(),
            ProviderPresence::Missing
        );
        assert_eq!(
            resource_by_id(&index, "remote").presence(),
            ProviderPresence::NotApplicable
        );
        assert_eq!(
            index.declaration_by_id("invalid").unwrap().target(),
            DeclarationTarget::InvalidHref
        );
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
        let index = index(&package, &provider);

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
        assert_eq!(
            index.reading_order().next().unwrap().target(),
            Some(&IdrefTarget::Invalid)
        );
    }

    #[test]
    fn duplicate_ids_are_ambiguous_declaration_lookups() {
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
        let same = index(&same, &provider);
        let different = index(&different, &provider);

        assert!(same.declaration_by_id("dup").is_none());
        assert_eq!(
            same.declarations_with_id("dup")
                .unwrap()
                .map(|declaration| declaration.resource().unwrap().ordinal())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            1
        );
        assert!(different.declaration_by_id("dup").is_none());
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
        let index = index(&package, &provider);

        assert!(index.cover_image().is_none());
        assert!(matches!(
            index.selections().cover,
            ResourceSelection::Ambiguous { ref candidates, .. } if candidates.len() == 2
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
        let index = index(&package, &provider);
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
        let unrecognized_family = entries[0].presentation().flow().candidates();
        assert_eq!(unrecognized_family.len(), 2);
        assert!(
            unrecognized_family
                .iter()
                .all(|candidate| candidate.source() == RenditionValueSource::PackageMetadata)
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
        assert!(matches!(
            inherited.page_spread(),
            RenditionSetting::Unspecified
        ));
        assert_eq!(
            inherited.page_progression_direction(),
            Some(PageProgressionDirection::Default)
        );

        assert_eq!(entries[2].target(), Some(&IdrefTarget::Missing));
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
    fn unrecognized_itemref_tokens_do_not_override_publication_rendition_metadata() {
        let package = parse_package(
            r##"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata>
                <meta property="rendition:layout">pre-paginated</meta>
                <meta property="rendition:spread">both</meta>
            </metadata>
            <manifest>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml" />
            </manifest>
            <spine>
                <itemref idref="chapter" properties="rendition:layout-prepaginated" />
                <itemref idref="chapter" properties="rendition:layout-reflowable" />
                <itemref idref="chapter" properties="rendition:spread-none rendition:spread-portrait" />
                <itemref idref="chapter" properties="rendition:page-spread-centre" />
            </spine>
        </package>"##,
        );
        let provider = provider_index(["EPUB/package.opf", "EPUB/chapter.xhtml"]);
        let index = index(&package, &provider);
        let entries = index.reading_order().collect::<Vec<_>>();

        let typo = entries[0].presentation().layout();
        assert_eq!(typo.value(), Some(&RenditionLayout::PrePaginated));
        assert_eq!(
            typo.candidate().map(RenditionCandidate::source),
            Some(RenditionValueSource::PackageMetadata)
        );
        assert_eq!(
            entries[0]
                .properties()
                .iter()
                .map(SpinePropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["rendition:layout-prepaginated"]
        );

        let recognized = entries[1].presentation().layout();
        assert_eq!(recognized.value(), Some(&RenditionLayout::Reflowable));
        assert_eq!(
            recognized.candidate().map(RenditionCandidate::source),
            Some(RenditionValueSource::ItemRefProperty)
        );

        assert!(matches!(
            entries[2].presentation().spread(),
            RenditionSetting::Ambiguous(candidates) if candidates.len() == 2
        ));

        let page_spread = entries[3].presentation().page_spread();
        assert_eq!(page_spread.value(), None);
        assert_eq!(
            page_spread
                .candidate()
                .and_then(RenditionCandidate::authored_value)
                .map(EpubString::as_str),
            Some("rendition:page-spread-centre")
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
        let original = index(&package, &provider);
        let detached_clone = original.clone();

        package.metadata_mut().add_meta(Meta::new(
            crate::package::metadata::MetaPropertyToken::from(KnownMetaProperty::RenditionLayout),
            EpubString::new("pre-paginated").unwrap(),
        ));
        let rebuilt = index(&package, &provider);

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

        let index = index(&package, &provider_index(["EPUB/package.opf"]));
        assert!(matches!(
            index.reading_order().next().unwrap().target(),
            Some(IdrefTarget::Declaration(_))
        ));
        assert_eq!(
            index.reading_order().nth(1).unwrap().target(),
            Some(&IdrefTarget::Invalid)
        );
        assert!(matches!(
            index.selections().cover,
            ResourceSelection::Selected { .. }
        ));
        assert!(matches!(
            index.selections().ncx,
            ResourceSelection::Selected { .. }
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
            package.resource_selections().0,
            ResourceSelection::Ambiguous { ref candidates, .. }
                if candidates.iter().map(|ordinal| ordinal.index()).eq([0, 1])
        ));
        let index = index(&package, &provider_index(["EPUB/package.opf"]));
        assert!(matches!(
            index.selections().cover,
            ResourceSelection::Ambiguous { ref candidates, .. } if candidates.len() == 2
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
        let index = index(&package, &provider);

        let declaration = index.declaration(ManifestOrdinal::from_index(0)).unwrap();
        let resource = declaration.resource().unwrap();
        let occurrence = index
            .occurrence(ReadingOrderOrdinal::from_index(0))
            .unwrap();
        assert_eq!(index.resource(resource.ordinal()).unwrap(), resource);
        assert_eq!(resource.declarations().next(), Some(declaration));
        assert_eq!(occurrence.declaration(), Some(declaration));
        assert_eq!(occurrence.resource(), Some(resource));
        assert!(index.resource(ResourceOrdinal(u32::MAX)).is_none());
        assert!(index.declaration(ManifestOrdinal(u32::MAX)).is_none());
        assert!(index.occurrence(ReadingOrderOrdinal(u32::MAX)).is_none());
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn resource_index_rejects_indices_beyond_public_ordinals() {
        assert!(
            ensure_ordinal_count(u32::MAX as usize + 1, ResourceIndexCollection::Resources).is_ok()
        );
        assert_eq!(
            ensure_ordinal_count(u32::MAX as usize + 2, ResourceIndexCollection::Resources)
                .unwrap_err()
                .collection(),
            ResourceIndexCollection::Resources
        );
    }

    #[test]
    fn canonical_href_corpus_preserves_authored_text_and_only_encodes_interior_spaces() {
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
            ("chapter one [1].xhtml", Kind::Local),
            ("chapter\tone.xhtml", Kind::Invalid),
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
            if authored == "chapter one [1].xhtml" {
                let ParsedHref::Local { target, .. } = &parsed else {
                    panic!("expected a local href");
                };
                assert_eq!(target.as_str(), "chapter%20one%20[1].xhtml");
            }
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
            let resolved = href::resolve_href(&AuthoredHref::new(authored), &source).unwrap();
            assert_eq!(
                resolved.address,
                ResourceAddress::Local(EpubPath::new(expected_path).unwrap()),
                "authored href: {authored:?}"
            );
            assert_eq!(resolved.fragment.as_deref(), expected_fragment);
        }

        for authored in [
            "../../../outside.xhtml",
            "%2e%2e/%2e%2e/%2e%2e/outside.xhtml",
        ] {
            let resolved = href::resolve_href(&AuthoredHref::new(authored), &source);
            assert!(
                matches!(resolved, Err(ref error) if error.authored().as_str() == authored),
                "authored href {authored:?} resolved as {resolved:?}"
            );
        }
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

    #[test]
    fn strict_publication_href_rejects_what_link_resolution_repairs() {
        let package_path = EpubPath::new("EPUB/package.opf").unwrap();
        let accepted = [
            ("text/chapter.xhtml", "EPUB/text/chapter.xhtml"),
            ("text/chapter%20one.xhtml", "EPUB/text/chapter one.xhtml"),
            ("text/chapter.xhtml?view=full", "EPUB/text/chapter.xhtml"),
            ("../EPUB/chapter.xhtml", "EPUB/chapter.xhtml"),
        ];
        for (authored, expected) in accepted {
            assert_eq!(
                resolve_publication_href(&AuthoredHref::new(authored), &package_path),
                Some(EpubPath::new(expected).unwrap()),
                "authored href: {authored:?}"
            );
        }

        let rejected = [
            "chapter.xhtml#part",
            "chapter.xhtml/",
            "text//chapter.xhtml",
            "text/%2E%2E",
            "text/.",
            "https://example.com/chapter.xhtml",
            "data:text/plain,hello",
            "#part",
            "",
        ];
        for authored in rejected {
            let href = AuthoredHref::new(authored);
            assert_eq!(
                resolve_publication_href(&href, &package_path),
                None,
                "authored href: {authored:?}"
            );
        }

        for authored in ["chapter.xhtml/", "text//chapter.xhtml"] {
            assert!(
                resolve_href(&AuthoredHref::new(authored), &package_path).is_ok(),
                "link resolution should stay lenient for {authored:?}"
            );
        }
    }

    #[test]
    fn ordinals_round_trip_the_position_they_serialize() {
        let ordinals = [
            ResourceOrdinal::new(3).as_u32(),
            ManifestOrdinal::new(3).as_u32(),
            ReadingOrderOrdinal::new(3).as_u32(),
        ];
        assert_eq!(ordinals, [3, 3, 3]);
        assert_eq!(ManifestOrdinal::new(3).index(), 3);
    }
}
