//! Open an EPUB publication to browse its structure, read resources, analyze content, and
//! export edits.
//!
//! The public facade is [`crate::Epub`]. Opening indexes available paths, parses the package, and
//! loads at most one selected navigation document. Other resource content is read on demand.
//! Committed edits affect publication reads and exports but do not modify the underlying provider.
pub(crate) mod annotation;
mod cfi;
pub(crate) mod persistence;

#[cfg(test)]
use crate::analysis::orchestration::{
    ExtractionOutcome, IngestPlan, StreamBudget, collect_smil_references, ingest_resource_reader,
    probe_root_format,
};
#[cfg(test)]
use crate::{
    accessibility::{
        AccessibilityFact, AccessibilityMetadataKind, AccessibilityObservationRef,
        PageBreakSourceTerm, WcagLevel,
    },
    analysis::{
        AnalysisIssue, AnalysisOutcome, ResourceClassification, ResourceFacts, SemanticFormat,
        coverage::{CoverageState, RelationshipSource},
        fingerprint::Blake3Hash,
        inspection::InspectionKind,
        reference::{AuthoredReference, HrefRole, HrefTarget, ManifestTarget, ReferenceContext},
    },
    content::ContentFacts,
    content::extraction::svg::{reset_scan_count, scan_count},
    media_overlay::SmilNodeFact,
    media_type::{MediaContainer, MediaType, MediaTypeClassification},
    resource::AuthoredHref,
};
use crate::{
    analysis::{AnalysisLimits, PublicationAnalysis},
    container::{ContainerError, ExportError, export_provider},
    edit::{EditError, StructuralResourceKind},
    error::Result,
    navigation::{
        Heading, Navigation, NavigationDepthError, NavigationDocument, NavigationList,
        NavigationSource, NavigationXhtmlError, parse, parse::NavigationParseError,
    },
    package::{Package, PackageError, PackageXmlDecodeError, manifest::ManifestItem},
    resource::provider::{
        MemoryResourceProvider, ProviderReadError, ResourceProvider, ResourceProviderIndex,
        ResourceProviderIndexError, ResourceProviderIndexLimits,
    },
    resource::{
        EpubPath, EpubPathError, ProviderPresence, ReadingOrderEntry, ResourceAddress,
        ResourceIndex, ResourceKey, ResourceLookupError, ResourceReadError, ResourceRecord,
        ResourceSelector, resolve_local_href_from_source,
    },
    semantics::EpubStructuralSemantic,
    string::EpubString,
    xml::decode_xml,
};
use persistence::ResourceChanges;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Limits to apply when opening an [`Epub`] from an untrusted or potentially large provider.
///
/// These limits bound provider-index path data and bytes read for the package and each selected
/// navigation candidate. They do not cap the number or total byte size of ordinary resources;
/// use [`AnalysisLimits`] for a later analysis pass. Values are copied into the publication and
/// continue to govern index rebuilds performed by embedded annotation loading and export.
pub struct EpubOpenLimits {
    provider_index: ResourceProviderIndexLimits,
    max_package_bytes: u64,
    max_selected_navigation_bytes: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("EPUB opening limits must be nonzero")]
/// Returned when either structural-document byte limit is zero.
///
/// Provider-index limits are validated separately by
/// [`ResourceProviderIndexLimits::new`](crate::resource::provider::ResourceProviderIndexLimits::new).
pub struct EpubOpenLimitsError;

impl EpubOpenLimits {
    /// Creates limits for package and selected-navigation reads.
    ///
    /// Provider indexing initially uses [`ResourceProviderIndexLimits::default`]. Use
    /// [`Self::with_provider_index_limits`] to replace those limits.
    ///
    /// # Errors
    ///
    /// Returns [`EpubOpenLimitsError`] if either argument is zero.
    pub fn new(
        max_package_bytes: u64,
        max_selected_navigation_bytes: u64,
    ) -> std::result::Result<Self, EpubOpenLimitsError> {
        if max_package_bytes == 0 || max_selected_navigation_bytes == 0 {
            return Err(EpubOpenLimitsError);
        }
        Ok(Self {
            provider_index: ResourceProviderIndexLimits::default(),
            max_package_bytes,
            max_selected_navigation_bytes,
        })
    }

    /// Replaces the limits used to enumerate and index provider paths.
    ///
    /// This consumes and returns the value for builder-style configuration. The supplied limits
    /// are also reused when the resource index is rebuilt for annotations and export.
    pub fn with_provider_index_limits(mut self, limits: ResourceProviderIndexLimits) -> Self {
        self.provider_index = limits;
        self
    }

    /// Returns the limits applied while enumerating provider paths.
    pub fn provider_index_limits(&self) -> &ResourceProviderIndexLimits {
        &self.provider_index
    }

    /// Returns the maximum number of decoded-source bytes read for the package document.
    ///
    /// Opening reads at most one byte beyond this value to detect an over-limit document.
    pub fn max_package_bytes(&self) -> u64 {
        self.max_package_bytes
    }
    /// Returns the per-candidate byte limit for the selected EPUB NAV or NCX document.
    ///
    /// Opening reads at most one byte beyond this value. A malformed or unreadable candidate is
    /// otherwise omitted from the navigation model; exceeding this limit is a hard open failure.
    pub fn max_selected_navigation_bytes(&self) -> u64 {
        self.max_selected_navigation_bytes
    }
}

impl Default for EpubOpenLimits {
    /// Uses 65,536 provider entries, 8 MiB of provider path text, and 16 MiB for each structural
    /// document byte limit.
    fn default() -> Self {
        Self {
            provider_index: ResourceProviderIndexLimits::default(),
            max_package_bytes: 16 * 1024 * 1024,
            max_selected_navigation_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// The reason a provider could not be opened as the requested EPUB rendition.
///
/// This error owns paths and lower-level failures but not the provider. [`EpubOpenError`] wraps
/// it and returns ownership of the provider to the caller. The enum is non-exhaustive so callers
/// must include a fallback arm.
pub enum EpubOpenFailure {
    /// The requested package path was not a canonical local EPUB path.
    #[error("Invalid package path {path}: {source}")]
    InvalidPackagePath {
        /// The caller-supplied path.
        path: PathBuf,
        /// The path-normalization failure.
        source: EpubPathError,
    },
    /// The provider could not produce a complete index within the configured limits.
    #[error("Could not index the resource provider: {source}")]
    ProviderIndex {
        /// The indexing or limit failure.
        source: ResourceProviderIndexError,
    },
    /// The selected package resource could not be read.
    #[error("Could not read package document {path}: {source}")]
    PackageRead {
        /// The canonical package path.
        path: EpubPath,
        /// The provider failure.
        source: ProviderReadError,
    },
    /// The package resource exceeded [`EpubOpenLimits::max_package_bytes`].
    #[error("Package document {path} exceeds the opening byte limit of {limit}")]
    PackageByteLimit {
        /// The canonical package path.
        path: EpubPath,
        /// The configured maximum byte count.
        limit: u64,
    },
    /// The package bytes could not be decoded according to their XML encoding.
    #[error("Could not decode package XML at {path}: {source}")]
    PackageXmlDecode {
        /// The canonical package path.
        path: EpubPath,
        /// The XML byte-decoding failure.
        source: PackageXmlDecodeError,
    },
    /// The decoded package XML could not produce a package model.
    #[error("Could not parse package document {path}: {source}")]
    PackageParse {
        /// The canonical package path.
        path: EpubPath,
        /// The package parsing failure.
        source: PackageError,
    },
    /// A selected EPUB NAV or NCX candidate exceeded the navigation byte limit.
    #[error("Selected navigation document {path} exceeds the opening byte limit of {limit}")]
    SelectedNavigationByteLimit {
        /// The canonical path of the selected candidate.
        path: EpubPath,
        /// The configured per-candidate maximum byte count.
        limit: u64,
    },
    /// OCF metadata did not select a usable package rendition.
    ///
    /// This variant is used by container-level rendition selection rather than
    /// [`Epub::from_provider`], which receives a package path directly.
    #[error("Could not select the OCF rendition: {source}")]
    ContainerSelection {
        /// The container selection failure.
        source: ContainerError,
    },
}

/// An opening failure that lets an application recover or retry with the original provider.
///
/// Consume this error with [`Self::into_provider`] or [`Self::into_parts`] to retry or recover
/// backend state. Opening does not intentionally mutate the provider. `Debug` does not print it.
pub struct EpubOpenError<R> {
    failure: EpubOpenFailure,
    provider: R,
}

impl<R> std::fmt::Debug for EpubOpenError<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EpubOpenError")
            .field("failure", &self.failure)
            .field("provider", &"<returned provider>")
            .finish()
    }
}

impl<R> EpubOpenError<R> {
    pub(crate) fn new(failure: EpubOpenFailure, provider: R) -> Self {
        Self { failure, provider }
    }
    /// Borrows the focused opening failure without consuming the returned provider.
    pub fn failure(&self) -> &EpubOpenFailure {
        &self.failure
    }
    /// Consumes the error and returns the original provider, discarding the failure value.
    pub fn into_provider(self) -> R {
        self.provider
    }
    /// Consumes the error and returns both the failure and original provider.
    pub fn into_parts(self) -> (EpubOpenFailure, R) {
        (self.failure, self.provider)
    }
}

impl<R> std::fmt::Display for EpubOpenError<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl<R> std::error::Error for EpubOpenError<R> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

#[derive(Debug)]
/// A loaded EPUB rendition for reading, analysis, editing, and export.
///
/// Resource bytes are read from the owned provider on demand. Committing an edit replaces the
/// current package, navigation, and resource index, so cloned keys and detached analyses can
/// become stale.
///
/// Parsing and normalized models preserve supported authored values and unknown vocabulary data
/// where documented by their modules, but [`Self::export`] rebuilds ZIP structure and is not a
/// byte-for-byte archive round trip.
pub struct Epub<R: ResourceProvider> {
    /// the zip archive
    pub(crate) container: R,
    /// root file full path
    pub(crate) package_path: EpubPath,

    pub(crate) package: Package,
    pub(crate) navigation: Navigation,

    pub(crate) resources: ResourceIndex,
    pub(crate) open_limits: EpubOpenLimits,
    pub(crate) provider_index: ResourceProviderIndex,
    pub(crate) resource_changes: ResourceChanges,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Failure to construct a new minimal in-memory EPUB.
///
/// Construction is transactional: no partial [`Epub`] is returned. The enum is non-exhaustive so
/// callers must include a fallback arm.
pub enum EpubCreateError {
    /// The required package metadata could not be constructed or generated.
    #[error("Could not construct the package document: {source}")]
    Package {
        #[from]
        /// The package-model construction or serialization failure.
        source: PackageError,
    },
    /// The initial EPUB navigation document could not be constructed or generated.
    #[error("Could not construct the navigation document: {source}")]
    Navigation {
        #[from]
        /// The generated navigation XHTML parsing failure.
        source: NavigationParseError,
    },
    /// The empty table-of-contents model exceeded navigation structural limits.
    #[error("Could not construct the navigation model: {source}")]
    NavigationDepth {
        #[from]
        /// The navigation model failure.
        source: NavigationDepthError,
    },
    /// The navigation model could not be serialized as XHTML.
    #[error("Could not generate the navigation document: {source}")]
    NavigationXhtml {
        #[from]
        /// The normalized XHTML generation failure.
        source: NavigationXhtmlError,
    },
    /// The generated in-memory resources could not be indexed.
    #[error("Could not index generated EPUB resources: {source}")]
    ProviderIndex {
        #[from]
        /// The generated in-memory provider indexing failure.
        source: ResourceProviderIndexError,
    },
    /// The generated package would not reopen under the publication's default limits.
    #[error("Generated package is {size} bytes, exceeding the opening limit of {limit}")]
    PackageByteLimit {
        /// The generated package size in bytes.
        size: u64,
        /// The default package opening limit.
        limit: u64,
    },
}

impl Epub<MemoryResourceProvider> {
    /// Creates a new minimal EPUB 3 publication for an application to populate and export.
    ///
    /// The publication contains a conventional container, package document, and empty
    /// EPUB navigation document. Its reading order is intentionally empty so callers can
    /// add content through the ordinary edit API before export.
    ///
    /// The returned publication owns generated container, package, and navigation bytes. Generated
    /// XML is normalized; no caller source formatting is involved.
    ///
    /// # Errors
    ///
    /// Returns [`EpubCreateError`] if required metadata is not representable, package or
    /// navigation generation fails, generated resources cannot be indexed, or the package exceeds
    /// the default opening byte limit.
    pub fn create(
        identifier: impl AsRef<str>,
        title: impl AsRef<str>,
        language: impl AsRef<str>,
        modified: time::OffsetDateTime,
    ) -> std::result::Result<Self, EpubCreateError> {
        const PACKAGE_PATH: &str = "EPUB/package.opf";
        const NAV_PATH: &str = "EPUB/nav.xhtml";
        const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
    <rootfiles>
        <rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/>
    </rootfiles>
</container>"#;

        let package = Package::new_minimal(identifier, title, language, modified)?;
        let package_xml = package.to_normalized_xml()?;
        let open_limits = EpubOpenLimits::default();
        if package_xml.len() as u64 > open_limits.max_package_bytes() {
            return Err(EpubCreateError::PackageByteLimit {
                size: package_xml.len() as u64,
                limit: open_limits.max_package_bytes(),
            });
        }
        let nav_path = EpubPath::new(NAV_PATH).expect("static NAV path is valid");
        let toc = NavigationList::builder()
            .semantic(EpubStructuralSemantic::Toc)
            .heading(Heading::new(
                crate::semantics::HeadingLevel::new(1).expect("1 is a valid heading level"),
                EpubString::new("Contents").expect("static heading is non-empty"),
            ))
            .build()?;
        let navigation = NavigationDocument::builder()
            .path(nav_path.clone())
            .lists(vec![toc])
            .build()?;
        let publication_title = package.metadata().title()[0]
            .content()
            .expect("minimal package title has content");
        let nav_xml = Navigation::new(navigation).to_normalized_xhtml(publication_title)?;
        let navigation = parse::epub_nav(nav_path.clone(), &nav_xml)?;
        let provider = MemoryResourceProvider::from_entries([
            ("mimetype", b"application/epub+zip".to_vec()),
            ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
            (PACKAGE_PATH, package_xml.into_bytes()),
            (NAV_PATH, nav_xml.into_bytes()),
        ])
        .expect("generated EPUB paths are valid");
        let package_path = EpubPath::new(PACKAGE_PATH).expect("static package path is valid");
        let provider_index = provider.index(open_limits.provider_index_limits())?;
        let resources = ResourceIndex::new(&package, &package_path, &provider_index);

        Ok(Self {
            container: provider,
            package_path,
            package,
            navigation: Navigation::new(navigation),
            resources,
            open_limits,
            provider_index,
            resource_changes: ResourceChanges::new(),
        })
    }
}

/// A resource selected from the current publication state, ready to inspect or read.
///
/// The handle cannot outlive or be held across a mutable edit. It does not cache bytes, so every
/// read can perform provider I/O.
pub struct Resource<'a, R: ResourceProvider> {
    epub: &'a Epub<R>,
    key: ResourceKey,
}

impl<R: ResourceProvider> Resource<'_, R> {
    /// Returns metadata and declaration information for this resource.
    pub fn record(&self) -> &ResourceRecord {
        self.epub
            .resources
            .resource(self.key)
            .expect("resource key belongs to the live index")
    }

    /// Returns the record's canonical local path or remote URL address.
    pub fn address(&self) -> &ResourceAddress {
        self.record().address()
    }

    /// Opens a local resource for streaming or parser-driven reading.
    ///
    /// The reader is valid only during the callback. The callback's return value is not
    /// interpreted, so application errors in `T` are not folded into [`ResourceReadError`].
    ///
    /// # Errors
    ///
    /// Returns [`ResourceReadError::NonLocal`] for a remote resource,
    /// [`ResourceReadError::Missing`] for an absent or removed resource, or an I/O/backend variant
    /// when the provider cannot open it. Callback errors, if any, are contained in `T`.
    pub fn read_with<T>(
        &self,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> std::result::Result<T, ResourceReadError> {
        let Some(path) = self.record().local_path() else {
            return Err(ResourceReadError::NonLocal {
                address: self.address().clone(),
            });
        };
        if self.record().presence() != ProviderPresence::Present {
            return Err(ResourceReadError::Missing { path: path.clone() });
        }
        if let Some(change) = self.epub.resource_changes.entry(path.as_path()) {
            return match change {
                Some(bytes) => {
                    let mut cursor = Cursor::new(bytes);
                    Ok(read(&mut cursor))
                }
                None => Err(ResourceReadError::Missing { path: path.clone() }),
            };
        }
        self.epub
            .container
            .read_with(path, read)
            .map_err(Into::into)
    }

    /// Reads the complete local resource when the application needs owned bytes.
    ///
    /// This operation has no byte limit and uses memory proportional to the resource size. Prefer
    /// [`Self::read_with`] when a parser can stream input.
    ///
    /// # Errors
    ///
    /// Returns the same location and provider failures as [`Self::read_with`], plus
    /// [`ResourceReadError::Io`] if reading the opened stream fails.
    pub fn bytes(&self) -> std::result::Result<Vec<u8>, ResourceReadError> {
        let path =
            self.record()
                .local_path()
                .cloned()
                .ok_or_else(|| ResourceReadError::NonLocal {
                    address: self.address().clone(),
                })?;
        self.read_with(|reader| {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        })?
        .map_err(|source| ResourceReadError::Io { path, source })
    }

    /// Reads a complete local text resource as strict UTF-8.
    ///
    /// The method buffers the full resource. It does not perform XML encoding detection or
    /// newline normalization.
    ///
    /// # Errors
    ///
    /// Returns errors from [`Self::bytes`] or [`ResourceReadError::InvalidUtf8`].
    pub fn utf8_text(&self) -> std::result::Result<String, ResourceReadError> {
        let path =
            self.record()
                .local_path()
                .cloned()
                .ok_or_else(|| ResourceReadError::NonLocal {
                    address: self.address().clone(),
                })?;
        String::from_utf8(self.bytes()?)
            .map_err(|source| ResourceReadError::InvalidUtf8 { path, source })
    }
}

impl<R: ResourceProvider> Epub<R> {
    /// Opens one package rendition from application-provided storage with default limits.
    ///
    /// This consumes `provider`. Opening indexes all provider paths, reads and parses the package,
    /// and attempts the package-selected EPUB NAV followed by NCX fallback. Missing, unreadable,
    /// undecodable, or malformed navigation candidates are omitted rather than failing opening;
    /// an over-limit selected candidate is a hard failure. Ordinary publication resources are not
    /// read.
    ///
    /// # Errors
    ///
    /// Returns [`EpubOpenError`] for an invalid package path, provider indexing or package
    /// read/decode/parse failure, or a structural byte-limit failure. The error returns ownership
    /// of `provider`.
    pub fn from_provider(
        provider: R,
        package_path: impl AsRef<Path>,
    ) -> std::result::Result<Self, EpubOpenError<R>> {
        Self::from_provider_with_limits(provider, package_path, EpubOpenLimits::default())
    }

    /// Opens one package rendition with limits chosen for the application's storage or trust model.
    ///
    /// Opening behavior matches [`Self::from_provider`]. The limits are retained for later index
    /// rebuilds. A successful provider must satisfy [`ResourceProvider`]'s stable-view contract
    /// for the publication's lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`EpubOpenError`] under the conditions documented by [`Self::from_provider`], with
    /// the supplied limits determining index and structural-byte failures.
    pub fn from_provider_with_limits(
        provider: R,
        package_path: impl AsRef<Path>,
        open_limits: EpubOpenLimits,
    ) -> std::result::Result<Self, EpubOpenError<R>> {
        let package_path_buf = package_path.as_ref().to_path_buf();
        let prepared = (|| -> std::result::Result<_, EpubOpenFailure> {
            let package_epub_path = EpubPath::new(&package_path_buf).map_err(|source| {
                EpubOpenFailure::InvalidPackagePath {
                    path: package_path_buf.clone(),
                    source,
                }
            })?;
            let provider_index = provider
                .index(open_limits.provider_index_limits())
                .map_err(|source| EpubOpenFailure::ProviderIndex { source })?;
            let package_bytes = provider_bytes_bounded(
                &provider,
                &package_epub_path,
                open_limits.max_package_bytes,
            )
            .map_err(|error| match error {
                BoundedReadError::Provider(source) => EpubOpenFailure::PackageRead {
                    path: package_epub_path.clone(),
                    source,
                },
                BoundedReadError::Limit => EpubOpenFailure::PackageByteLimit {
                    path: package_epub_path.clone(),
                    limit: open_limits.max_package_bytes,
                },
            })?;
            let package_xml =
                decode_xml(&package_bytes).map_err(|source| EpubOpenFailure::PackageXmlDecode {
                    path: package_epub_path.clone(),
                    source,
                })?;
            let package = Package::parse(package_xml.as_ref()).map_err(|source| {
                EpubOpenFailure::PackageParse {
                    path: package_epub_path.clone(),
                    source,
                }
            })?;
            let mut navigation = Navigation::empty();
            let nav_document = match package.nav_item() {
                Some(nav_item) => try_load_navigation(
                    &provider,
                    nav_item,
                    &package_epub_path,
                    NavigationSource::EpubNav,
                    &open_limits,
                )?,
                None => None,
            };
            if let Some(document) = nav_document {
                navigation = Navigation::new(document);
            }
            if navigation.is_empty() {
                let ncx_document = match package.ncx_item() {
                    Some(ncx_item) => try_load_navigation(
                        &provider,
                        ncx_item,
                        &package_epub_path,
                        NavigationSource::Ncx,
                        &open_limits,
                    )?,
                    None => None,
                };
                if let Some(document) = ncx_document {
                    navigation = Navigation::new(document);
                }
            }

            let resources = ResourceIndex::new(&package, &package_epub_path, &provider_index);
            Ok((
                package_epub_path,
                package,
                navigation,
                resources,
                provider_index,
            ))
        })();

        let (package_path, package, navigation, resources, provider_index) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err(EpubOpenError::new(error, provider)),
        };

        Ok(Self {
            container: provider,
            package_path,
            package,
            navigation,
            resources,
            open_limits,
            provider_index,
            resource_changes: ResourceChanges::new(),
        })
    }

    /// Returns the current resource inventory for browsing and lookup.
    ///
    /// Keys cloned from this index can become stale after a committed edit rebuilds it.
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    /// Analyzes publication resources with default limits for inspection and derived facts.
    ///
    /// Analysis may read every indexed resource and returns a detached snapshot. Per-resource
    /// failures are recorded in that snapshot rather than returned as a method error. The snapshot
    /// remains usable after edits but then describes stale publication state.
    pub fn analyze(&self) -> PublicationAnalysis {
        crate::analysis::orchestration::analyze(self, AnalysisLimits::default())
    }

    /// Analyzes publication resources under limits chosen by the application.
    ///
    /// The returned detached snapshot owns `limits`, its resource index, and all derived facts.
    /// Analysis can read every eligible resource, while bounded extraction records limit outcomes
    /// in the snapshot. It does not mutate or cache data in the publication.
    pub fn analyze_with_limits(&self, limits: AnalysisLimits) -> PublicationAnalysis {
        crate::analysis::orchestration::analyze(self, limits)
    }

    /// Iterates reading-order entries in authored spine order.
    ///
    /// Entries can represent unresolved or ambiguous declarations. Iteration does not read
    /// resource bytes, and borrowed entries cannot be retained across a mutable edit.
    pub fn reading_order(&self) -> impl Iterator<Item = &ReadingOrderEntry> {
        self.resources.reading_order()
    }

    fn resource_record(
        &self,
        selector: impl Into<ResourceSelector>,
    ) -> std::result::Result<&ResourceRecord, ResourceLookupError> {
        let selector = selector.into();
        self.resources.select(&selector)
    }

    /// Selects one resource for metadata inspection or later reading.
    ///
    /// Selection does not read resource bytes. The returned handle borrows this publication and
    /// therefore cannot be held while an edit is committed.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceLookupError`] when the selector finds no resource, is ambiguous, or
    /// identifies an unresolved manifest declaration. Provider and content errors are deferred to
    /// the handle's read methods.
    pub fn resource(
        &self,
        selector: impl Into<ResourceSelector>,
    ) -> std::result::Result<Resource<'_, R>, ResourceLookupError> {
        let record = self.resource_record(selector)?;
        Ok(Resource {
            epub: self,
            key: record.key(),
        })
    }

    pub(crate) fn read_resource_for_analysis<T>(
        &self,
        address: &ResourceAddress,
        read: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<T> {
        resource_reader_from_parts(&self.container, &self.resource_changes, address, read)
    }

    /// Consumes the publication and returns its unchanged base provider.
    ///
    /// **Committed overlay additions, replacements, and removals are discarded.** Export first if
    /// those changes must be retained. Parsed models, indexes, and analysis state are also dropped.
    pub fn into_base_provider(self) -> R {
        self.container
    }

    /// Returns the canonical provider path of the package document.
    pub fn package_path(&self) -> &EpubPath {
        &self.package_path
    }
    /// Returns the parsed package metadata, manifest, and spine for the current state.
    ///
    /// The model is semantic rather than a complete copy of original OPF lexical formatting.
    pub fn package(&self) -> &Package {
        &self.package
    }
    /// Returns the selected normalized navigation model for the current committed state.
    ///
    /// This may be empty when no readable, decodable, and parseable EPUB NAV or NCX was available
    /// at opening. Access does not read the provider.
    pub fn navigation(&self) -> &Navigation {
        &self.navigation
    }
    /// Writes the current publication state to an EPUB ZIP without ending the editing session.
    ///
    /// Export includes every committed change and emits the canonical stored `mimetype` entry
    /// first. It is separate from edit commit. Unchanged provider resources are streamed.
    /// Resource payload bytes are preserved unless replaced, but ZIP entry order, compression,
    /// timestamps, extra fields, and other archive representation details are normalized.
    ///
    /// The returned writer remains owned by the caller. Export does not mutate the publication or
    /// clear changes and imposes no aggregate output-byte limit.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] if the effective index exceeds the publication's opening limits, a
    /// provider resource cannot be read, output streaming fails, or ZIP finalization fails. A
    /// failure can leave the writer partially written.
    pub fn export<A: std::io::Write + std::io::Seek>(
        &self,
        writer: A,
    ) -> std::result::Result<A, ExportError> {
        let index = self
            .resource_changes
            .apply_to_index(
                &self.provider_index,
                self.open_limits.provider_index_limits(),
            )
            .map_err(|source| ExportError::ProviderIndex { source })?;
        export_provider(&self.container, &index, &self.resource_changes, writer)
    }

    /// Creates or truncates a file and writes the normalized current publication state.
    ///
    /// This has the same preservation, cost, and limit behavior as [`Self::export`]. The output
    /// file is created before export, so any error can leave a missing, empty, or partial archive;
    /// replacement is not atomic.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::OutputPath`] if the file cannot be created, or any error documented
    /// by [`Self::export`] while writing it.
    pub fn export_to_path(&self, path: impl AsRef<Path>) -> std::result::Result<(), ExportError> {
        let path = path.as_ref();
        let file = std::fs::File::create(path).map_err(|source| ExportError::OutputPath {
            source,
            path: path.to_path_buf(),
        })?;
        self.export(file).map(|_| ())
    }

    pub(crate) fn install_edit_preview(
        &mut self,
        package: Package,
        navigation: Navigation,
        resource_changes: ResourceChanges,
        resources: ResourceIndex,
    ) {
        self.package = package;
        self.navigation = navigation;
        self.resource_changes = resource_changes;
        self.resources = resources;
    }

    pub(crate) fn reject_structural_resource_edits(
        &self,
        changes: &ResourceChanges,
        semantic_structural_rewrites: &BTreeMap<EpubPath, Option<Vec<u8>>>,
    ) -> std::result::Result<(), EditError> {
        for path in changes.entries().keys() {
            if let Some(kind) = self.structural_resource_kind(path) {
                if let Some(expected_change) = semantic_structural_rewrites.get(path)
                    && changes.entry(path.as_path())
                        == Some(expected_change.as_ref().map(Vec::as_slice))
                {
                    continue;
                }
                return Err(EditError::StructuralResourceEdit {
                    path: path.clone(),
                    kind,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn structural_resource_kind(
        &self,
        path: &EpubPath,
    ) -> Option<StructuralResourceKind> {
        if path == &self.package_path {
            return Some(StructuralResourceKind::Package);
        }
        if self
            .resources
            .epub_nav()
            .and_then(ResourceRecord::local_path)
            == Some(path)
        {
            return Some(StructuralResourceKind::Navigation);
        }
        (self.resources.ncx().and_then(ResourceRecord::local_path) == Some(path))
            .then_some(StructuralResourceKind::Ncx)
    }
}

fn try_load_navigation<R: ResourceProvider>(
    provider: &R,
    item: &ManifestItem,
    package_path: &EpubPath,
    source: NavigationSource,
    options: &EpubOpenLimits,
) -> std::result::Result<Option<NavigationDocument>, EpubOpenFailure> {
    let Some(epub_path) = structural_manifest_href_path(item, package_path) else {
        return Ok(None);
    };
    let bytes =
        match provider_bytes_bounded(provider, &epub_path, options.max_selected_navigation_bytes) {
            Ok(bytes) => bytes,
            Err(BoundedReadError::Limit) => {
                return Err(EpubOpenFailure::SelectedNavigationByteLimit {
                    path: epub_path,
                    limit: options.max_selected_navigation_bytes,
                });
            }
            Err(BoundedReadError::Provider(_)) => return Ok(None),
        };
    let xml = match decode_xml(&bytes) {
        Ok(xml) => xml,
        Err(_) => return Ok(None),
    };
    let parsed = match source {
        NavigationSource::EpubNav => parse::epub_nav(epub_path, &xml),
        NavigationSource::Ncx => parse::ncx(epub_path, &xml),
    };
    Ok(parsed.ok())
}

fn provider_bytes_bounded<R: ResourceProvider>(
    provider: &R,
    path: &EpubPath,
    limit: u64,
) -> std::result::Result<Vec<u8>, BoundedReadError> {
    let bytes = provider
        .read_with(path, |reader| {
            let mut bytes = Vec::new();
            reader
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|source| ProviderReadError::IoPath {
                    source,
                    path: path.clone(),
                })?;
            Ok::<_, ProviderReadError>(bytes)
        })
        .map_err(BoundedReadError::Provider)?
        .map_err(BoundedReadError::Provider)?;
    if bytes.len() as u64 > limit {
        return Err(BoundedReadError::Limit);
    }
    Ok(bytes)
}

enum BoundedReadError {
    Provider(ProviderReadError),
    Limit,
}

pub(crate) fn structural_manifest_href_path(
    item: &ManifestItem,
    package_path: &EpubPath,
) -> Option<EpubPath> {
    let authored = item.authored_href()?;
    let (path, fragment) = resolve_local_href_from_source(authored, package_path)?;
    fragment.is_none().then_some(path)
}

pub(crate) fn invalid_navigation_href_error(item: &ManifestItem) -> EditError {
    EditError::InvalidNavigationHref {
        id: item
            .id()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<missing>".to_string()),
        href: item
            .authored_href()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<missing>".to_string()),
    }
}

pub(crate) fn resource_bytes_from_parts<R: ResourceProvider>(
    container: &R,
    changes: &ResourceChanges,
    address: &ResourceAddress,
) -> std::result::Result<Vec<u8>, EditError> {
    let path = address
        .local_path()
        .ok_or_else(|| EditError::NonLocalResource {
            selector: address.display_value().to_string(),
        })?;
    if let Some(change) = changes.entry(path.as_path()) {
        return change
            .map(Vec::from)
            .ok_or_else(|| ProviderReadError::MissingResource { path: path.clone() }.into());
    }
    container.read(path).map_err(Into::into)
}

fn resource_reader_from_parts<R: ResourceProvider, T>(
    container: &R,
    changes: &ResourceChanges,
    address: &ResourceAddress,
    read: impl FnOnce(&mut dyn Read) -> Result<T>,
) -> Result<T> {
    let path = address
        .local_path()
        .ok_or_else(|| crate::error::EpubError::NonLocalResource {
            address: address.display_value().to_string(),
        })?;
    if let Some(change) = changes.entry(path.as_path()) {
        return match change {
            Some(bytes) => {
                let mut cursor = Cursor::new(bytes);
                read(&mut cursor)
            }
            None => Err(ProviderReadError::MissingResource { path: path.clone() }.into()),
        };
    }
    container.read_with(path, read)?
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::analysis::{
        dependency::{Root, RootError},
        impact::{ImpactError, StructuralChange},
        reference::ReferenceSource,
    };
    use crate::cfi::Cfi;
    use crate::container::EpubZip;
    use crate::media_overlay::SmilFacts;
    use crate::resource::IndexKeyError;
    use crate::resource::provider::{
        MemoryResourceProvider, ProviderReadError, ResourceProvider, ResourceProviderEntry,
        ResourceProviderIndex,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::{Cursor, Read, Write};
    use std::str::FromStr;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    #[derive(Debug)]
    struct CountingProvider {
        marker: &'static str,
        entries: HashMap<String, Vec<u8>>,
        reads: RefCell<HashMap<String, usize>>,
        opens: RefCell<HashMap<String, usize>>,
    }

    impl CountingProvider {
        fn new(
            marker: &'static str,
            entries: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
        ) -> Self {
            Self {
                marker,
                entries: entries
                    .into_iter()
                    .map(|(path, bytes)| (path.to_string(), bytes))
                    .collect(),
                reads: RefCell::new(HashMap::new()),
                opens: RefCell::new(HashMap::new()),
            }
        }

        fn bytes_read(&self, path: &str) -> usize {
            self.reads.borrow().get(path).copied().unwrap_or(0)
        }

        fn open_count(&self, path: &str) -> usize {
            self.opens.borrow().get(path).copied().unwrap_or(0)
        }
    }

    struct CountingReader<'a> {
        inner: Cursor<&'a [u8]>,
        bytes_read: usize,
    }

    impl Read for CountingReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl ResourceProvider for CountingProvider {
        fn read(&self, _path: &EpubPath) -> crate::resource::provider::ReadResult<Vec<u8>> {
            panic!("opening should use a scoped reader")
        }

        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> crate::resource::provider::ReadResult<T> {
            let bytes = self
                .entries
                .get(path.as_str())
                .ok_or_else(|| ProviderReadError::MissingResource { path: path.clone() })?;
            *self
                .opens
                .borrow_mut()
                .entry(path.as_str().to_string())
                .or_default() += 1;
            let mut reader = CountingReader {
                inner: Cursor::new(bytes.as_slice()),
                bytes_read: 0,
            };
            let result = read(&mut reader);
            *self
                .reads
                .borrow_mut()
                .entry(path.as_str().to_string())
                .or_default() += reader.bytes_read;
            Ok(result)
        }

        fn index(
            &self,
            limits: &ResourceProviderIndexLimits,
        ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
            ResourceProviderIndex::try_from_entries(
                self.entries.iter().map(|(path, bytes)| {
                    ResourceProviderEntry::new(
                        EpubPath::new(path).expect("test paths are valid"),
                        Some(bytes.len() as u64),
                    )
                }),
                limits,
            )
        }
    }

    #[test]
    fn epub_open_limits_reject_zeroes_and_expose_conservative_defaults() {
        let invalid = [EpubOpenLimits::new(0, 1), EpubOpenLimits::new(1, 0)];
        assert!(invalid.into_iter().all(|result| result.is_err()));

        let limits = EpubOpenLimits::default();
        assert_eq!(limits.max_package_bytes(), 16 * 1024 * 1024);
        assert_eq!(limits.max_selected_navigation_bytes(), 16 * 1024 * 1024);
    }

    #[test]
    fn focused_package_path_read_and_decode_failures_return_provider() {
        let provider = memory_provider();
        let error = Epub::from_provider(provider, "../package.opf").unwrap_err();
        let (failure, provider) = error.into_parts();
        assert!(matches!(
            failure,
            EpubOpenFailure::InvalidPackagePath { .. }
        ));
        assert!(provider.contains(&EpubPath::new("EPUB/package.opf").unwrap()));

        let provider =
            MemoryResourceProvider::from_entries([("EPUB/other.txt", b"not a package".to_vec())])
                .unwrap();
        let error = Epub::from_provider(provider, "EPUB/package.opf").unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::PackageRead {
                source: ProviderReadError::MissingResource { .. },
                ..
            }
        ));

        let provider = MemoryResourceProvider::from_entries([(
            "EPUB/package.opf",
            b"<package>\xff</package>".to_vec(),
        )])
        .unwrap();
        let error = Epub::from_provider(provider, "EPUB/package.opf").unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::PackageXmlDecode {
                source: PackageXmlDecodeError::InvalidBytes { .. },
                ..
            }
        ));
    }

    #[test]
    fn fatal_index_failure_returns_provider() {
        let provider = memory_provider();

        let error = Epub::from_provider_with_limits(
            provider,
            "EPUB/package.opf",
            EpubOpenLimits::default()
                .with_provider_index_limits(ResourceProviderIndexLimits::new(1, 1024).unwrap()),
        )
        .unwrap_err();

        assert!(matches!(
            error.failure(),
            EpubOpenFailure::ProviderIndex {
                source:
                    crate::resource::provider::ResourceProviderIndexError::EntryCountLimitExceeded {
                        limit: 1
                    }
            }
        ));
        let provider = error.into_provider();
        assert!(provider.contains(&EpubPath::new("EPUB/package.opf").unwrap()));
    }

    #[test]
    fn package_byte_budget_accepts_boundary_and_stops_at_plus_one() {
        let package = opening_package(false, false, 0);
        let limit = package.len() as u64;
        let provider = opening_provider("package-exact", package.clone(), []);
        let epub = open_counting(provider, open_limits_with_byte_limits(limit, 1)).unwrap();
        assert_eq!(
            epub.container.bytes_read("EPUB/package.opf"),
            limit as usize
        );

        let provider = opening_provider("package-plus-one", package, []);
        let error =
            open_counting(provider, open_limits_with_byte_limits(limit - 1, 1)).unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::PackageByteLimit { limit: found, .. }
                if *found == limit - 1
        ));
        let provider = error.into_provider();
        assert_eq!(provider.marker, "package-plus-one");
        assert_eq!(provider.bytes_read("EPUB/package.opf"), limit as usize);
    }

    #[test]
    fn nav_byte_budget_accepts_boundary_and_fatal_plus_one_skips_ncx() {
        let package = opening_package(true, true, 0);
        let nav = opening_nav();
        let ncx = opening_ncx();
        let limit = nav.len() as u64;
        let provider = opening_provider(
            "nav-exact",
            package.clone(),
            [
                ("EPUB/nav.xhtml", nav.clone()),
                ("EPUB/toc.ncx", ncx.clone()),
            ],
        );
        let epub = open_counting(
            provider,
            open_limits_with_byte_limits(package.len() as u64, limit),
        )
        .unwrap();
        assert!(epub.navigation().epub_nav().is_some());
        assert_eq!(epub.container.bytes_read("EPUB/nav.xhtml"), limit as usize);
        assert_eq!(epub.container.bytes_read("EPUB/toc.ncx"), 0);

        let provider = opening_provider(
            "nav-plus-one",
            package.clone(),
            [("EPUB/nav.xhtml", nav), ("EPUB/toc.ncx", ncx)],
        );
        let error = open_counting(
            provider,
            open_limits_with_byte_limits(package.len() as u64, limit - 1),
        )
        .unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::SelectedNavigationByteLimit { limit: found, .. }
                if *found == limit - 1
        ));
        let provider = error.into_provider();
        assert_eq!(provider.marker, "nav-plus-one");
        assert_eq!(provider.bytes_read("EPUB/nav.xhtml"), limit as usize);
        assert_eq!(provider.bytes_read("EPUB/toc.ncx"), 0);
    }

    #[test]
    fn ncx_fallback_byte_budget_accepts_boundary_and_stops_at_plus_one() {
        let package = opening_package(true, true, 0);
        let ncx = opening_ncx();
        let limit = ncx.len() as u64;
        let extras = || {
            [
                ("EPUB/nav.xhtml", b"x".to_vec()),
                ("EPUB/toc.ncx", ncx.clone()),
            ]
        };
        let provider = opening_provider("ncx-exact", package.clone(), extras());
        let epub = open_counting(
            provider,
            open_limits_with_byte_limits(package.len() as u64, limit),
        )
        .unwrap();
        assert!(epub.navigation().ncx().is_some());
        assert_eq!(epub.container.bytes_read("EPUB/toc.ncx"), limit as usize);

        let provider = opening_provider("ncx-plus-one", package.clone(), extras());
        let error = open_counting(
            provider,
            open_limits_with_byte_limits(package.len() as u64, limit - 1),
        )
        .unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::SelectedNavigationByteLimit { limit: found, .. }
                if *found == limit - 1
        ));
        let provider = error.into_provider();
        assert_eq!(provider.marker, "ncx-plus-one");
        assert_eq!(provider.bytes_read("EPUB/toc.ncx"), limit as usize);
    }

    #[test]
    fn opening_does_not_read_declared_smil_resources() {
        let package = opening_package(false, false, 2);
        let one = opening_smil("one");
        let two = opening_smil("two");
        let provider = opening_provider(
            "no-eager-smil",
            package.clone(),
            [("EPUB/one.smil", one), ("EPUB/two.smil", two)],
        );
        let epub = open_counting(
            provider,
            open_limits_with_byte_limits(package.len() as u64, 1),
        )
        .unwrap();
        assert_eq!(epub.container.bytes_read("EPUB/one.smil"), 0);
        assert_eq!(epub.container.bytes_read("EPUB/two.smil"), 0);
    }

    fn memory_provider_epub() -> Epub<MemoryResourceProvider> {
        Epub::from_provider(memory_provider(), "EPUB/package.opf").unwrap()
    }

    #[test]
    fn resource_handle_separates_lookup_provider_and_callback_failures() {
        let mut epub = memory_provider_epub();
        let selector = ResourceSelector::path("EPUB/text/chapter.xhtml").unwrap();
        let resource = epub.resource(selector.clone()).unwrap();

        assert_eq!(
            resource.utf8_text().unwrap(),
            "<html><body>Chapter</body></html>"
        );
        let callback_result = resource
            .read_with(|_| Err::<(), _>("parser rejected resource"))
            .unwrap();
        assert_eq!(callback_result, Err("parser rejected resource"));

        epub.edit()
            .replace_resource(selector.clone(), b"chapter \xff".to_vec())
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(matches!(
            epub.resource(selector.clone()).unwrap().utf8_text(),
            Err(ResourceReadError::InvalidUtf8 { .. })
        ));

        epub.edit()
            .remove_resource(selector.clone())
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(matches!(
            epub.resource(selector).unwrap().bytes(),
            Err(ResourceReadError::Missing { .. })
        ));
        assert!(matches!(
            epub.resource(ResourceSelector::path("EPUB/absent.xhtml").unwrap()),
            Err(ResourceLookupError::NotFound(_))
        ));
    }

    fn open_limits_with_byte_limits(package: u64, navigation: u64) -> EpubOpenLimits {
        EpubOpenLimits::new(package, navigation).unwrap()
    }

    fn opening_package(has_nav: bool, has_ncx: bool, smil_resources: usize) -> Vec<u8> {
        let mut manifest = String::new();
        if has_nav {
            manifest.push_str(
                r#"<item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml"/>"#,
            );
        }
        if has_ncx {
            manifest.push_str(
                r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            );
        }
        for index in 0..smil_resources {
            let name = if index == 0 { "one" } else { "two" };
            manifest.push_str(&format!(
                r#"<item id="{name}" href="{name}.smil" media-type="application/smil+xml"/>"#
            ));
        }
        let toc = if has_ncx { r#" toc="ncx""# } else { "" };
        format!(
            r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid"><metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata><manifest>{manifest}</manifest><spine{toc}/></package>"#
        )
        .into_bytes()
    }

    fn opening_nav() -> Vec<u8> {
        br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol/></nav></body></html>"#.to_vec()
    }

    fn opening_ncx() -> Vec<u8> {
        br#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap/></ncx>"#
            .to_vec()
    }

    fn opening_smil(id: &str) -> Vec<u8> {
        format!(
            r#"<smil xmlns="http://www.w3.org/ns/SMIL"><body><seq><par><text src="{id}.xhtml"/></par></seq></body></smil>"#
        )
        .into_bytes()
    }

    #[test]
    fn media_analysis_opens_the_provider_resource_once() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="audio" href="audio.mp3" media-type="audio/mpeg"/><item id="captions" href="captions.vtt" media-type="text/vtt"/></manifest>
  <spine/>
</package>"#
            .to_vec();
        let provider = opening_provider(
            "media-open-count",
            package,
            [
                (
                    "EPUB/audio.mp3",
                    include_bytes!("../tests/fixtures/media-cbr.mp3").to_vec(),
                ),
                (
                    "EPUB/captions.vtt",
                    b"WEBVTT\n\n00:00.000 --> 00:01.000\nCaption\n".to_vec(),
                ),
            ],
        );
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        assert_eq!(epub.container.open_count("EPUB/audio.mp3"), 0);

        let analysis = epub.analyze();

        assert!(
            analysis
                .accessibility_observations()
                .any(|observation| matches!(
                    observation,
                    AccessibilityObservationRef::MediaTrack { .. }
                ))
        );
        assert!(
            analysis
                .accessibility_observations()
                .any(|observation| matches!(
                    observation,
                    AccessibilityObservationRef::WebVtt { .. }
                ))
        );

        assert_eq!(epub.container.open_count("EPUB/audio.mp3"), 1);
    }

    fn opening_provider(
        marker: &'static str,
        package: Vec<u8>,
        extras: impl IntoIterator<Item = (&'static str, Vec<u8>)>,
    ) -> CountingProvider {
        CountingProvider::new(
            marker,
            std::iter::once(("EPUB/package.opf", package)).chain(extras),
        )
    }

    #[allow(clippy::result_large_err)]
    fn open_counting(
        provider: CountingProvider,
        limits: EpubOpenLimits,
    ) -> std::result::Result<Epub<CountingProvider>, EpubOpenError<CountingProvider>> {
        Epub::from_provider_with_limits(provider, "EPUB/package.opf", limits)
    }

    fn memory_provider() -> MemoryResourceProvider {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            (
                "EPUB/text/chapter.xhtml",
                b"<html><body>Chapter</body></html>".to_vec(),
            ),
        ])
        .unwrap()
    }

    #[test]
    fn publication_analysis_owns_css_references_and_inspection() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="style" href="styles/book.css" media-type="text/css"/>
    <item id="image" href="images/paper.png" media-type="application/xhtml+xml"/>
    <item id="not-css" href="styles/not-css.css" media-type="application/octet-stream"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&16u32.to_be_bytes());
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png.extend_from_slice(b"\0\0\0\0IEND\0\0\0\0");
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/chapter.xhtml", b"<html><body/></html>".to_vec()),
            (
                "EPUB/styles/book.css",
                b"body { background: url(../images/paper.png) }".to_vec(),
            ),
            ("EPUB/styles/not-css.css", b"@import 'wrong.css';".to_vec()),
            ("EPUB/provider.css", b"@import \"missing.css\";".to_vec()),
            ("EPUB/images/paper.png", png),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        reset_scan_count();
        let analysis = epub.analyze();
        let style = analysis
            .resources()
            .find_unique_resource_by_id("style")
            .unwrap();
        let image = analysis
            .resources()
            .find_unique_resource_by_id("image")
            .unwrap();
        let not_css = analysis
            .resources()
            .find_unique_resource_by_id("not-css")
            .unwrap();
        let provider_css_path = EpubPath::new("EPUB/provider.css").unwrap();
        let provider_css = analysis
            .resources()
            .resources()
            .iter()
            .find(|resource| resource.local_path() == Some(&provider_css_path))
            .unwrap();
        assert!(
            analysis
                .content_for(style.key())
                .unwrap()
                .is_not_applicable()
        );
        assert!(analysis
            .references_from_resource(style.key())
            .unwrap()
            .any(|reference| {
                reference.role() == HrefRole::CssUrl
                    && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == image.key())
            }));
        let inspection = analysis
            .inspection_for(image.key())
            .unwrap()
            .value()
            .unwrap();
        assert!(matches!(
            analysis
                .facts_for(image.key())
                .unwrap()
                .classification()
                .value(),
            Some(ResourceClassification::Identified(SemanticFormat::Xhtml))
        ));
        assert!(
            !analysis
                .content_for(image.key())
                .unwrap()
                .is_not_applicable()
        );
        assert!(
            analysis
                .content_for(provider_css.key())
                .unwrap()
                .is_not_applicable()
        );
        assert!(
            analysis
                .content_for(not_css.key())
                .unwrap()
                .is_not_applicable()
        );
        assert!(
            analysis
                .inspection_for(provider_css.key())
                .unwrap()
                .value()
                .unwrap()
                .detected_media_type()
                .is_none()
        );
        assert!(matches!(
            analysis
                .inspection_for(provider_css.key())
                .unwrap()
                .value()
                .unwrap()
                .kind(),
            InspectionKind::Text(_)
        ));
        let InspectionKind::RasterImage(image) = inspection.kind() else {
            panic!("expected PNG inspection")
        };
        assert_eq!((image.width(), image.height()), (Some(16), Some(8)));
        assert!(analysis.coverage().inspection().is_complete());
    }

    #[test]
    fn dependency_closure_and_edit_impact_use_canonical_references() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata/>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml" fallback="fallback"/>
    <item id="fallback" href="text/fallback.xhtml" media-type="application/xhtml+xml"/>
    <item id="other" href="text/other.xhtml" media-type="application/xhtml+xml"/>
    <item id="a" href="styles/a.css" media-type="text/css"/>
    <item id="b" href="styles/b.css" media-type="text/css"/>
    <item id="image" href="images/paper.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#;
        let nav = br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let chapter = br#"<html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="../styles/a.css"/></head><body><img src="../images/paper.png"/><a href="other.xhtml">Other</a><form action="submit.xhtml"/></body></html>"#;
        let other = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><a href="chapter.xhtml">Chapter</a></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/nav.xhtml", nav.to_vec()),
            ("EPUB/text/chapter.xhtml", chapter.to_vec()),
            ("EPUB/text/fallback.xhtml", b"<html><body/></html>".to_vec()),
            ("EPUB/text/other.xhtml", other.to_vec()),
            (
                "EPUB/styles/a.css",
                b"@import 'b.css'; body { background: url('../images/missing.png') }".to_vec(),
            ),
            ("EPUB/styles/b.css", b"@import 'a.css';".to_vec()),
            ("EPUB/images/paper.png", b"not a png".to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();
        let fallback = analysis
            .resources()
            .find_unique_resource_by_id("fallback")
            .unwrap();
        let other = analysis
            .resources()
            .find_unique_resource_by_id("other")
            .unwrap();
        let declaration = analysis.resources().find_unique_by_id("chapter").unwrap();

        let resource_closure = analysis
            .dependency_closure(Root::Resource(chapter.key()))
            .unwrap();
        assert!(!resource_closure.resources().contains(&fallback.key()));
        assert!(!resource_closure.resources().contains(&other.key()));
        assert_eq!(resource_closure.unresolved().len(), 1);
        assert!(!resource_closure.is_complete());
        assert_eq!(
            resource_closure,
            analysis
                .dependency_closure(Root::Resource(chapter.key()))
                .unwrap()
        );
        assert!(resource_closure.resources().iter().any(|key| {
            analysis
                .resources()
                .resource(*key)
                .is_ok_and(|resource| resource.metadata().file_extension() == Some("css"))
        }));
        assert!(
            resource_closure.resources().len() < 7,
            "cycles must terminate"
        );

        let declaration_closure = analysis
            .dependency_closure(Root::Declaration(declaration.key()))
            .unwrap();
        assert!(declaration_closure.resources().contains(&fallback.key()));

        let removal = analysis.impact_of_removal(chapter.key()).unwrap();
        assert_eq!(removal.incoming().len(), 2);
        assert!(removal.outgoing_rebased().is_empty());
        assert!(removal.structural_changes().iter().any(|impact| matches!(
            impact,
            StructuralChange::ManifestDeclaration(key) if *key == declaration.key()
        )));
        assert!(
            removal
                .structural_changes()
                .iter()
                .any(|impact| matches!(impact, StructuralChange::ReadingOrderOccurrence(_)))
        );
        assert!(
            removal
                .structural_changes()
                .iter()
                .any(|impact| matches!(impact, StructuralChange::NavigationReference(_)))
        );

        let movement = analysis
            .impact_of_move(
                chapter.key(),
                &EpubPath::new("EPUB/chapters/chapter.xhtml").unwrap(),
            )
            .unwrap();
        assert_eq!(movement.incoming().len(), 2);
        assert_eq!(movement.outgoing_rebased().len(), 4);
        assert!(
            movement
                .structural_changes()
                .iter()
                .any(|impact| matches!(impact, StructuralChange::NavigationReference(_)))
        );

        let limited = epub
            .analyze_with_limits(AnalysisLimits::default().with_max_analyzed_resources(Some(0)));
        let limited_chapter = limited
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();
        let closure = limited
            .dependency_closure(Root::Resource(limited_chapter.key()))
            .unwrap();
        assert!(!closure.incomplete_sources().is_empty());
        assert!(!closure.is_complete());
    }

    #[test]
    fn reading_order_closure_roots_preserve_resolution_errors() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="dup" href="a.xhtml" media-type="application/xhtml+xml"/><item id="dup" href="b.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref/><itemref idref="missing"/><itemref idref="dup"/></spine></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/a.xhtml", b"<html/>".to_vec()),
            ("EPUB/b.xhtml", b"<html/>".to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let entries = analysis.resources().reading_order().collect::<Vec<_>>();

        assert!(matches!(
            analysis.dependency_closure(Root::ReadingOrderOccurrence(entries[0].key())),
            Err(RootError::MissingReadingOrderIdref(_))
        ));
        assert!(matches!(
            analysis.dependency_closure(Root::ReadingOrderOccurrence(entries[1].key())),
            Err(RootError::MissingManifestId { .. })
        ));
        assert!(matches!(
            analysis.dependency_closure(Root::ReadingOrderOccurrence(entries[2].key())),
            Err(RootError::AmbiguousManifestId { .. })
        ));

        let foreign = Epub::from_provider(memory_provider(), "EPUB/package.opf")
            .unwrap()
            .analyze();
        let foreign_key = foreign
            .resources()
            .find_unique_resource_by_id("chap")
            .unwrap()
            .key();
        assert!(matches!(
            analysis.dependency_closure(Root::Resource(foreign_key)),
            Err(RootError::Index(IndexKeyError::ForeignIndex))
        ));
        assert_eq!(
            analysis.impact_of_removal(foreign_key),
            Err(ImpactError::Index(IndexKeyError::ForeignIndex))
        );
        let package = analysis.resources().package().key();
        assert_eq!(
            analysis.impact_of_removal(package),
            Err(ImpactError::PackageDocument(package))
        );
        assert_eq!(
            analysis.impact_of_move(package, &EpubPath::new("package.opf").unwrap()),
            Err(ImpactError::PackageDocument(package))
        );
    }

    #[test]
    fn closure_reports_missing_fragments_and_unresolved_declaration_roots() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="figure" href="figure.svg" media-type="image/svg+xml"/><item id="broken" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="broken"/></spine></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            (
                "EPUB/figure.svg",
                br##"<svg xmlns="http://www.w3.org/2000/svg"><use href="#missing"/></svg>"##
                    .to_vec(),
            ),
        ])
        .unwrap();
        let analysis = Epub::from_provider(provider, "EPUB/package.opf")
            .unwrap()
            .analyze();
        let figure = analysis
            .resources()
            .find_unique_resource_by_id("figure")
            .unwrap();
        let broken = analysis.resources().find_unique_by_id("broken").unwrap();

        let figure_closure = analysis
            .dependency_closure(Root::Resource(figure.key()))
            .unwrap();
        assert_eq!(figure_closure.unresolved().len(), 1);
        assert!(matches!(
            figure_closure.unresolved(),
            [AuthoredReference::Href(reference)]
                if matches!(reference.target(), HrefTarget::Fragment { exists: Some(false), .. })
        ));
        assert!(!figure_closure.is_complete());

        let declaration_closure = analysis
            .dependency_closure(Root::Declaration(broken.key()))
            .unwrap();
        assert!(declaration_closure.resources().is_empty());
        assert_eq!(
            declaration_closure.incomplete_sources(),
            &[ReferenceSource::Declaration(broken.key())]
        );
        assert!(!declaration_closure.is_complete());

        let reading_order = analysis.resources().reading_order().next().unwrap();
        let reading_order_closure = analysis
            .dependency_closure(Root::ReadingOrderOccurrence(reading_order.key()))
            .unwrap();
        assert_eq!(
            reading_order_closure.incomplete_sources(),
            &[ReferenceSource::Declaration(broken.key())]
        );
        assert!(!reading_order_closure.is_complete());
    }

    #[test]
    fn move_impact_conservatively_covers_authored_bases_and_self_references() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/><item id="icons" href="assets/icons.svg" media-type="image/svg+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
        let chapter = br##"<html xmlns="http://www.w3.org/1999/xhtml"><head><base href="../assets/icons.svg"/></head><body><img src="#icon"/><a href="?view">View</a><a href="chapter.xhtml#self">Self</a></body></html>"##;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/text/chapter.xhtml", chapter.to_vec()),
            (
                "EPUB/assets/icons.svg",
                br#"<svg xmlns="http://www.w3.org/2000/svg"><g id="icon"/></svg>"#.to_vec(),
            ),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();
        let impact = analysis
            .impact_of_move(
                chapter.key(),
                &EpubPath::new("EPUB/chapters/chapter.xhtml").unwrap(),
            )
            .unwrap();

        assert_eq!(impact.outgoing_rebased().len(), 3);
        assert!(impact.outgoing_rebased().iter().any(|reference| matches!(
            reference,
            AuthoredReference::Href(reference) if reference.declared().as_str() == "#icon"
        )));
        assert!(impact.outgoing_rebased().iter().any(|reference| matches!(
            reference,
            AuthoredReference::Href(reference) if reference.declared().as_str() == "?view"
        )));
        assert!(impact.incoming().iter().all(|reference| {
            !matches!(reference, AuthoredReference::Href(reference) if reference.source() == chapter.key())
        }));
    }

    #[test]
    fn authored_base_does_not_normalize_invalid_epub_paths() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
        let chapter = br#"<html><head><base href="../assets/"/></head><body><img src="../../../outside.png"/><img src="bad\path.png"/></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/text/chapter.xhtml", chapter.to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();

        let references = analysis
            .references_from_resource(chapter.key())
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 2);
        assert!(
            references
                .iter()
                .all(|reference| matches!(reference.target(), HrefTarget::Invalid(_)))
        );
    }

    #[test]
    fn standalone_svg_scripts_are_dependencies() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="figure" href="figure.svg" media-type="image/svg+xml"/><item id="script" href="app.js" media-type="text/javascript"/></manifest><spine/></package>"#;
        let figure = br#"<svg xmlns="http://www.w3.org/2000/svg"><script href="app.js"/></svg>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/figure.svg", figure.to_vec()),
            ("EPUB/app.js", b"run()".to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .find_unique_resource_by_id("figure")
            .unwrap();
        let script = analysis
            .resources()
            .find_unique_resource_by_id("script")
            .unwrap();

        let references = analysis
            .references_from_resource(figure.key())
            .unwrap()
            .collect::<Vec<_>>();
        assert!(matches!(
            references.as_slice(),
            [reference]
                if reference.role() == HrefRole::Script
                    && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == script.key())
                    && matches!(reference.context(), ReferenceContext::Svg(context) if context.element() == "script" && context.attribute() == "href")
        ));
        let closure = analysis
            .dependency_closure(Root::Resource(figure.key()))
            .unwrap();
        assert!(closure.resources().contains(&script.key()));
    }

    #[test]
    fn closure_and_removal_preserve_remote_and_declaration_specific_uncertainty() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="remote" href="https://example.com/book.css" media-type="text/css"/><item id="one" href="shared.xhtml" media-type="application/xhtml+xml" fallback="a"/><item id="two" href="shared.xhtml" media-type="application/xhtml+xml" fallback="b"/><item id="ambiguous" href="ambiguous.xhtml" media-type="application/xhtml+xml" fallback="dup"/><item id="dup" href="a.xhtml" media-type="application/xhtml+xml"/><item id="dup" href="b.xhtml" media-type="application/xhtml+xml"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            (
                "EPUB/chapter.xhtml",
                br#"<html><body><iframe src="https://example.com/book.css"/></body></html>"#
                    .to_vec(),
            ),
            ("EPUB/shared.xhtml", b"<html/>".to_vec()),
            ("EPUB/ambiguous.xhtml", b"<html/>".to_vec()),
            ("EPUB/a.xhtml", b"<html/>".to_vec()),
            ("EPUB/b.xhtml", b"<html/>".to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();
        let remote = analysis
            .resources()
            .find_unique_resource_by_id("remote")
            .unwrap();
        let closure = analysis
            .dependency_closure(Root::Resource(chapter.key()))
            .unwrap();
        assert!(closure.resources().contains(&remote.key()));
        assert!(
            closure
                .incomplete_sources()
                .contains(&ReferenceSource::Resource(remote.key()))
        );

        let one = analysis.resources().find_unique_by_id("one").unwrap();
        let two = analysis.resources().find_unique_by_id("two").unwrap();
        let a = analysis
            .resources()
            .find_unique_resource_by_id("a")
            .unwrap();
        let b = analysis
            .resources()
            .find_unique_resource_by_id("b")
            .unwrap();
        let one_closure = analysis
            .dependency_closure(Root::Declaration(one.key()))
            .unwrap();
        let two_closure = analysis
            .dependency_closure(Root::Declaration(two.key()))
            .unwrap();
        assert!(one_closure.resources().contains(&a.key()));
        assert!(!one_closure.resources().contains(&b.key()));
        assert!(two_closure.resources().contains(&b.key()));
        assert!(!two_closure.resources().contains(&a.key()));

        let removal = analysis.impact_of_removal(a.key()).unwrap();
        assert!(removal.incoming().iter().any(|reference| matches!(
            reference,
            AuthoredReference::Manifest(reference)
                if matches!(reference.target(), ManifestTarget::Ambiguous { .. })
        )));
        assert!(
            removal
                .incomplete_sources()
                .contains(&ReferenceSource::Resource(remote.key()))
        );
        assert!(!removal.is_complete());

        let remote_move = analysis
            .impact_of_move(
                remote.key(),
                &EpubPath::new("EPUB/styles/book.css").unwrap(),
            )
            .unwrap();
        assert!(
            remote_move
                .incomplete_sources()
                .contains(&ReferenceSource::Resource(remote.key()))
        );
        assert!(!remote_move.is_complete());
    }

    #[test]
    fn publication_analysis_owns_svg_references_accessibility_and_coverage() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata>
    <dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language>
    <meta property="schema:accessibilityFeature">alternativeText</meta>
    <meta property="schema:accessibilityFeature">structuralNavigation</meta>
    <meta property="dcterms:conformsTo">EPUB Accessibility 1.2 - WCAG 2.2 Level AA</meta>
    <meta property="page-source">print</meta>
    <link rel="a11y:certifierReport" href="report.html"/>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="figure" href="images/figure.svg" media-type="image/svg+xml"/>
    <item id="image" href="images/paper.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#;
        let nav = br#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav><nav epub:type="page-list"><ol><li><a href="chapter.xhtml#p1">1</a></li></ol></nav></body></html>"#;
        let chapter = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="p1">Chapter</h1><img src="images/paper.png" alt="Paper"/></body></html>"#;
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" id="root" width="20" height="10"><title id="title">Figure title</title><desc>Figure description</desc><g xml:base="../"><a href="chapter.xhtml#p1" xlink:href="ignored.xhtml"/></g><image xlink:href="paper.png"/><use href="#root"/><use href="#"/></svg>"##;
        let provider_svg =
            br#"<svg xmlns="http://www.w3.org/2000/svg"><title>Provider figure</title></svg>"#;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png.extend_from_slice(b"\0\0\0\0IEND\0\0\0\0");
        let provider = opening_provider(
            "svg-analysis-open-count",
            package.to_vec(),
            [
                ("EPUB/nav.xhtml", nav.to_vec()),
                ("EPUB/chapter.xhtml", chapter.to_vec()),
                ("EPUB/images/figure.svg", svg.to_vec()),
                ("EPUB/images/provider.svg", provider_svg.to_vec()),
                ("EPUB/images/paper.png", png),
                ("EPUB/report.html", b"report".to_vec()),
            ],
        );
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .find_unique_resource_by_id("figure")
            .unwrap();
        let chapter = analysis
            .resources()
            .find_unique_resource_by_id("chapter")
            .unwrap();
        let provider_path = EpubPath::new("EPUB/images/provider.svg").unwrap();
        let provider_figure = analysis
            .resources()
            .resources()
            .iter()
            .find(|resource| resource.local_path() == Some(&provider_path))
            .unwrap();
        let figure_facts = analysis
            .content_for(figure.key())
            .unwrap()
            .value()
            .and_then(ContentFacts::as_svg)
            .unwrap();
        assert!(
            figure_facts
                .fragments()
                .iter()
                .any(|fact| fact.id() == "root")
        );
        assert!(
            analysis
                .content_for(provider_figure.key())
                .unwrap()
                .value()
                .and_then(ContentFacts::as_svg)
                .is_some()
        );
        assert!(matches!(
            analysis
                .facts_for(provider_figure.key())
                .unwrap()
                .classification()
                .value(),
            Some(ResourceClassification::Identified(SemanticFormat::Svg))
        ));
        let inspection = analysis
            .inspection_for(figure.key())
            .unwrap()
            .value()
            .unwrap();
        let InspectionKind::SvgImage(inspection) = inspection.kind() else {
            panic!("expected SVG inspection");
        };
        assert_eq!(inspection.title(), Some("Figure title"));
        assert_eq!(inspection.description(), Some("Figure description"));
        let svg_references = analysis
            .references_from_resource(figure.key())
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(svg_references.len(), 4);
        assert!(
            svg_references
                .iter()
                .all(|reference| matches!(reference.context(), ReferenceContext::Svg(_)))
        );
        assert!(svg_references.iter().any(|reference| {
            reference.declared().as_str() == "chapter.xhtml#p1"
                && matches!(
                    reference.target(),
                    HrefTarget::Fragment {
                        resource,
                        exists: Some(true),
                        ..
                    } if *resource == chapter.key()
                )
        }));
        assert!(svg_references.iter().any(|reference| {
            reference.role() == HrefRole::Svg
                && matches!(
                    reference.target(),
                    HrefTarget::Fragment {
                        exists: Some(true),
                        ..
                    }
                )
        }));
        assert!(svg_references.iter().any(|reference| {
            reference.declared().as_str() == "#"
                && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == figure.key())
        }));
        let svg_coverage = analysis
            .coverage()
            .relationships()
            .iter()
            .filter(|coverage| matches!(coverage.source(), RelationshipSource::Svg(_)))
            .collect::<Vec<_>>();
        assert_eq!(svg_coverage.len(), 2);
        assert!(
            svg_coverage
                .iter()
                .all(|coverage| matches!(coverage.state(), CoverageState::Complete))
        );

        let accessibility = analysis.accessibility();
        assert_eq!(
            accessibility
                .metadata()
                .values_of(AccessibilityMetadataKind::Feature)
                .count(),
            2
        );
        assert!(accessibility.metadata().values().any(|value| value.kind()
            == AccessibilityMetadataKind::PageBreakSource(PageBreakSourceTerm::LegacyPageSource)));
        assert_eq!(
            accessibility
                .claims()
                .next()
                .unwrap()
                .conformance()
                .unwrap()
                .level(),
            WcagLevel::Aa
        );
        let report = accessibility.metadata().certifier_reports().next().unwrap();
        assert!(matches!(
            analysis
                .accessibility_certifier_report_reference(report)
                .unwrap()
                .target(),
            HrefTarget::Resource { .. }
        ));
        let svg_title = analysis
            .accessibility_observations()
            .find(|observation| {
                matches!(
                    observation,
                    AccessibilityObservationRef::Content {
                        resource,
                        fact: AccessibilityFact::SvgTitle(_),
                    } if resource.key() == figure.key()
                )
            })
            .unwrap();
        assert!(matches!(
            svg_title,
            AccessibilityObservationRef::Content {
                fact: AccessibilityFact::SvgTitle(_),
                ..
            }
        ));
        assert!(
            analysis
                .accessibility_observations()
                .any(|observation| matches!(
                    observation,
                    AccessibilityObservationRef::Structure { .. }
                ))
        );
        assert!(
            analysis.coverage().content().is_complete()
                && analysis.coverage().inspection().is_complete()
                && analysis.coverage().fragments().is_complete()
                && analysis
                    .coverage()
                    .relationships()
                    .iter()
                    .all(|coverage| matches!(coverage.state(), CoverageState::Complete)),
            "{:#?}",
            analysis.coverage()
        );
        assert_eq!(epub.container.open_count("EPUB/images/figure.svg"), 1);
        assert_eq!(epub.container.open_count("EPUB/images/provider.svg"), 1);
        assert_eq!(scan_count(), 2);
    }

    #[test]
    fn missing_svg_has_explicit_content_and_relationship_coverage() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="figure" href="missing.svg" media-type="image/svg+xml"/></manifest>
  <spine/>
</package>"#;
        let provider =
            MemoryResourceProvider::from_entries([("EPUB/package.opf", package.to_vec())]).unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .find_unique_resource_by_id("figure")
            .unwrap()
            .key();
        assert!(matches!(
            analysis.content_for(figure).unwrap(),
            AnalysisOutcome::Unavailable(AnalysisIssue::Missing)
        ));
        assert!(
            analysis
                .coverage()
                .content()
                .unavailable()
                .iter()
                .any(|work| {
                    work.resource() == figure && work.issue() == AnalysisIssue::Missing
                })
        );
        assert!(
            analysis
                .coverage()
                .relationships()
                .iter()
                .any(|coverage| matches!(
                    (coverage.source(), coverage.state()),
                    (RelationshipSource::Svg(key), CoverageState::Unavailable(AnalysisIssue::Missing))
                        if *key == figure
                ))
        );
        assert!(!analysis.coverage().content().is_complete());
    }

    #[test]
    fn raster_limit_retains_facts_and_marks_inspection_coverage_partial() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="image" href="image.png" media-type="image/png"/></manifest>
  <spine/>
</package>"#;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&200u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        png.extend_from_slice(b"\0\0\0\0IEND\0\0\0\0");
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/image.png", png),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze_with_limits(
            AnalysisLimits::default().with_max_resource_analysis_bytes(Some(40)),
        );
        let image = analysis
            .resources()
            .find_unique_resource_by_id("image")
            .unwrap()
            .key();

        let AnalysisOutcome::Partial { value, issue } = analysis.inspection_for(image).unwrap()
        else {
            panic!("expected partial image inspection")
        };
        assert_eq!(*issue, AnalysisIssue::PerResourceAnalysisLimit);
        let InspectionKind::RasterImage(facts) = value.kind() else {
            panic!("expected raster facts")
        };
        assert_eq!((facts.width(), facts.height()), (Some(320), Some(200)));
        assert!(
            analysis
                .coverage()
                .inspection()
                .partial()
                .iter()
                .any(|work| work.resource() == image)
        );
    }

    #[test]
    fn empty_unknown_resource_is_complete_at_a_zero_byte_budget() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="empty" href="empty.bin" media-type="application/octet-stream"/></manifest><spine/></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/empty.bin", Vec::new()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze_with_limits(
            AnalysisLimits::default()
                .with_max_resource_analysis_bytes(Some(0))
                .with_max_total_analysis_bytes(Some(0)),
        );
        let empty = analysis
            .resources()
            .find_unique_resource_by_id("empty")
            .unwrap()
            .key();

        assert!(matches!(
            analysis.facts_for(empty).unwrap().classification(),
            AnalysisOutcome::Complete(ResourceClassification::Unknown)
        ));
        assert!(
            analysis
                .coverage()
                .classification()
                .completed()
                .contains(&empty)
        );
    }

    #[test]
    fn false_empty_hint_cannot_bypass_a_zero_byte_budget() {
        let mut reader = Cursor::new(b"not empty");
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                classification: ResourceClassification::Unknown,
                known_empty: true,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(0),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert!(matches!(
            ingest.classification,
            AnalysisOutcome::Unavailable(AnalysisIssue::PerResourceAnalysisLimit)
        ));
        assert_eq!(ingest.analysis_bytes, 0);
    }

    #[test]
    fn fingerprint_budget_above_u32_max_does_not_truncate_on_32_bit_targets() {
        let bytes = b"fingerprint";
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                classification: ResourceClassification::Unknown,
                known_empty: false,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: Some(u64::from(u32::MAX) + 1),
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert_eq!(
            ingest.fingerprint,
            AnalysisOutcome::Complete(Blake3Hash::hash(bytes))
        );
    }

    #[test]
    fn tolerated_smil_attribute_loss_preserves_typed_analysis() {
        let epub = smil_epub(
            r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL">
  <body><seq bad="one" bad="two" textref="../text/chapter.xhtml"><par><text src="../text/chapter.xhtml#p1"/><audio src="../audio/chapter.mp3"/></par></seq></body>
</smil>"#,
        );

        let analysis = epub.analyze();
        assert!(analysis.resource_facts().any(|facts| matches!(
            facts.content(),
            AnalysisOutcome::Complete(ContentFacts::Smil(_))
        )));
        assert!(
            analysis
                .accessibility_observations()
                .any(|observation| matches!(observation, AccessibilityObservationRef::Smil { .. }))
        );
    }

    #[test]
    fn smil_node_references_are_borrowed_and_resource_scoped() {
        let epub = smil_epub(
            r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops">
  <body><seq epub:textref="../text/chapter.xhtml"><par><text src="../text/chapter.xhtml#p1"/><audio src="../audio/chapter.mp3"/></par></seq></body>
</smil>"#,
        );
        let analysis = epub.analyze();
        let smil = analysis
            .resources()
            .find_unique_resource_by_id("overlay")
            .unwrap()
            .key();
        let reading_order = analysis.resources().reading_order().next().unwrap().key();
        assert_eq!(
            analysis
                .media_overlay_associations()
                .find(|association| association.reading_order().key() == reading_order)
                .unwrap()
                .reference()
                .declared()
                .as_str(),
            "overlay"
        );
        let sequence = analysis
            .smil_roots_for(smil)
            .unwrap()
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(
            sequence.text_reference().unwrap().declared().as_str(),
            "../text/chapter.xhtml"
        );
        let parallel = sequence.children().next().unwrap();
        let children = parallel.children().collect::<Vec<_>>();
        let text = children
            .iter()
            .copied()
            .find(|node| matches!(node.fact(), SmilNodeFact::Text { .. }))
            .unwrap();
        let audio = children
            .iter()
            .copied()
            .find(|node| matches!(node.fact(), SmilNodeFact::Audio { .. }))
            .unwrap();
        assert_eq!(
            text.text_reference().unwrap().declared().as_str(),
            "../text/chapter.xhtml#p1"
        );
        assert!(text.audio_reference().is_none());
        assert_eq!(
            audio.audio_reference().unwrap().declared().as_str(),
            "../audio/chapter.mp3"
        );
    }

    #[test]
    fn smil_node_views_keep_same_slots_scoped_to_their_resource() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml"/>
    <item id="one" href="one.smil" media-type="application/smil+xml"/>
    <item id="two" href="two.smil" media-type="application/smil+xml"/>
  </manifest>
  <spine><itemref idref="chap"/></spine>
</package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/chapter.xhtml", b"<html/>".to_vec()),
            (
                "EPUB/one.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops"><body><seq epub:textref="one.xhtml"/></body></smil>"#.to_vec(),
            ),
            (
                "EPUB/two.smil",
                br#"<smil xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops"><body><seq epub:textref="two.xhtml"/></body></smil>"#.to_vec(),
            ),
        ])
        .unwrap();
        let analysis = Epub::from_provider(provider, "EPUB/package.opf")
            .unwrap()
            .analyze();

        for (id, declared) in [("one", "one.xhtml"), ("two", "two.xhtml")] {
            let resource = analysis
                .resources()
                .find_unique_resource_by_id(id)
                .unwrap()
                .key();
            let root = analysis
                .smil_roots_for(resource)
                .unwrap()
                .unwrap()
                .next()
                .unwrap();
            assert_eq!(root.text_reference().unwrap().declared().as_str(), declared);
        }
    }

    #[test]
    fn broken_smil_textref_is_exposed_as_a_typed_analysis_target() {
        let epub = smil_epub(
            r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL" xmlns:epub="http://www.idpf.org/2007/ops">
  <body><seq epub:textref="../missing.xhtml"><par><text src="../text/chapter.xhtml#p1"/><audio src="../audio/chapter.mp3"/></par></seq></body>
</smil>"#,
        );
        let analysis = epub.analyze();
        assert!(analysis.references().any(|reference| matches!(
            reference,
            AuthoredReference::Href(reference)
                if reference.declared().as_str() == "../missing.xhtml"
                    && matches!(reference.target(), HrefTarget::MissingLocal(_))
        )));
    }

    #[derive(Debug)]
    struct StreamingOnlyContainer {
        entries: HashMap<String, Vec<u8>>,
        reads: RefCell<Vec<String>>,
    }

    impl StreamingOnlyContainer {
        fn new(entries: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                entries: entries
                    .into_iter()
                    .map(|(path, contents)| (path.to_string(), contents.as_bytes().to_vec()))
                    .collect(),
                reads: RefCell::new(Vec::new()),
            }
        }

        fn was_read(&self, path: &str) -> bool {
            self.reads.borrow().iter().any(|read| read == path)
        }
    }

    impl ResourceProvider for StreamingOnlyContainer {
        fn read(&self, _path: &EpubPath) -> crate::resource::provider::ReadResult<Vec<u8>> {
            panic!("buffered entry read should not be used")
        }

        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> crate::resource::provider::ReadResult<T> {
            let path_value = path.as_str().to_string();
            self.reads.borrow_mut().push(path_value.clone());
            let bytes = self
                .entries
                .get(&path_value)
                .ok_or_else(|| ProviderReadError::MissingResource { path: path.clone() })?;
            let mut cursor = Cursor::new(bytes.as_slice());
            Ok(read(&mut cursor))
        }

        fn index(
            &self,
            limits: &ResourceProviderIndexLimits,
        ) -> std::result::Result<ResourceProviderIndex, ResourceProviderIndexError> {
            ResourceProviderIndex::try_from_entries(
                self.entries.iter().map(|(path, bytes)| {
                    ResourceProviderEntry::new(
                        EpubPath::new(path).expect("test paths are valid"),
                        Some(bytes.len() as u64),
                    )
                }),
                limits,
            )
        }
    }

    #[test]
    fn publication_analysis_uses_scoped_reader_without_buffered_entry() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            (
                "EPUB/text/chapter.xhtml",
                "<html><body><p id=\"p1\">Text</p></body></html>",
            ),
        ]);
        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();

        let analysis = epub.analyze();

        assert_eq!(analysis.search_entries().count(), 1);
    }

    #[test]
    fn from_provider_uses_scoped_reader_without_buffered_entry() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml" />
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#;
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap><navPoint><navLabel><text>Fallback</text></navLabel><content src="text/chapter.xhtml" /></navPoint></navMap></ncx>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            ("EPUB/nav.xhtml", nav),
            ("EPUB/toc.ncx", ncx),
            (
                "EPUB/text/chapter.xhtml",
                "<html><body><p>Text</p></body></html>",
            ),
        ]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().epub_nav().is_some());
        assert!(epub.navigation().ncx().is_none());
        assert_eq!(epub.reading_order().count(), 1);
        let container = epub.into_base_provider();
        assert!(container.was_read("EPUB/nav.xhtml"));
        assert!(!container.was_read("EPUB/toc.ncx"));
    }

    #[test]
    fn structural_navigation_href_uses_canonical_resource_path() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="nav" properties="nav" href="nav%20doc.xhtml?view=full" media-type="application/xhtml+xml"/></manifest><spine/></package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"/></body></html>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav doc.xhtml", nav.as_bytes().to_vec()),
        ])
        .unwrap();

        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().epub_nav().is_some());
    }

    #[test]
    fn from_provider_invalid_only_nav_href_succeeds_with_empty_navigation() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="" media-type="application/xhtml+xml" />
  </manifest>
  <spine />
</package>"#;
        let container = StreamingOnlyContainer::new([("EPUB/package.opf", package_xml)]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().is_empty());
    }

    #[test]
    fn from_provider_invalid_epub_nav_href_uses_valid_ncx() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="" media-type="application/xhtml+xml" />
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml" />
    <item id="chap" href="chapter.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine toc="ncx"><itemref idref="chap" /></spine>
</package>"#;
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap><navPoint><navLabel><text>Chapter</text></navLabel><content src="chapter.xhtml" /></navPoint></navMap></ncx>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            ("EPUB/toc.ncx", ncx),
            (
                "EPUB/chapter.xhtml",
                "<html><body><p>Text</p></body></html>",
            ),
        ]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();
        assert!(epub.navigation().epub_nav().is_none());
        assert!(epub.navigation().ncx().is_some());
        assert_eq!(
            epub.package()
                .manifest_item_by_id("nav")
                .and_then(ManifestItem::authored_href)
                .map(AuthoredHref::as_str),
            Some("")
        );
    }

    #[test]
    fn truncated_epub_nav_uses_valid_ncx() {
        let package = r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="nav" properties="nav" href="nav.xhtml" media-type="application/xhtml+xml"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"/></package>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>"#;
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap/></ncx>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.as_bytes().to_vec()),
            ("EPUB/nav.xhtml", nav.as_bytes().to_vec()),
            ("EPUB/toc.ncx", ncx.as_bytes().to_vec()),
        ])
        .unwrap();

        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().epub_nav().is_none());
        assert!(epub.navigation().ncx().is_some());
    }

    #[test]
    fn from_provider_missing_or_malformed_only_nav_succeeds_with_empty_navigation() {
        for nav in [None, Some("not XML")] {
            let mut provider = memory_provider();
            let nav_path = EpubPath::new("EPUB/nav.xhtml").unwrap();
            match nav {
                Some(nav) => provider.insert(&nav_path, nav.as_bytes().to_vec()).unwrap(),
                None => {
                    provider.remove(&nav_path);
                }
            }

            let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();

            assert!(epub.navigation().is_empty());
        }
    }

    #[test]
    fn from_provider_ignores_fragment_only_navigation_href() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="nav.xhtml#toc" media-type="application/xhtml+xml" />
  </manifest>
  <spine />
</package>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            (
                "EPUB/nav.xhtml",
                r#"<html><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
            ),
        ]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().is_empty());
    }

    #[test]
    fn from_provider_ignores_remote_navigation_href() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" properties="nav" href="https://example.com/nav.xhtml" media-type="application/xhtml+xml" />
  </manifest>
  <spine />
</package>"#;
        let container = StreamingOnlyContainer::new([("EPUB/package.opf", package_xml)]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();

        assert!(epub.navigation().is_empty());
    }

    #[test]
    fn smil_analysis_uses_scoped_reader_without_buffered_entry() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay" />
    <item id="overlay" href="overlays/chapter.smil" media-type="application/smil+xml" />
    <item id="audio" href="audio/chapter.mp3" media-type="audio/mpeg" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let smil = r#"<smil version="3.0" xmlns="http://www.w3.org/ns/SMIL">
  <body><seq textref="../text/chapter.xhtml"><par><text src="../text/chapter.xhtml#p1"/><audio src="../audio/chapter.mp3"/></par></seq></body>
</smil>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            (
                "EPUB/text/chapter.xhtml",
                "<html><body><p id=\"p1\">Text</p></body></html>",
            ),
            ("EPUB/overlays/chapter.smil", smil),
            ("EPUB/audio/chapter.mp3", "audio"),
        ]);

        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        assert!(analysis.resource_facts().any(|facts| matches!(
            facts.content(),
            AnalysisOutcome::Complete(ContentFacts::Smil(_))
        )));
    }

    #[test]
    fn cfi_text_uses_scoped_reader_without_buffered_entry() {
        let package_xml = r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" /></manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#;
        let container = StreamingOnlyContainer::new([
            ("EPUB/package.opf", package_xml),
            (
                "EPUB/text/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Hello world</p></body></html>"#,
            ),
        ]);
        let epub = Epub::from_provider(container, "EPUB/package.opf").unwrap();
        let cfi = Cfi::from_str("epubcfi(/6/2!/2/2,/1:0,/1:5)").unwrap();

        let text = epub.text_from_cfi_range(cfi.range().unwrap()).unwrap();

        assert_eq!(text, "Hello");
    }

    fn smil_epub(smil: &str) -> Epub<EpubZip<Cursor<Vec<u8>>>> {
        let mut data = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut data);
            let options = SimpleFileOptions::default();
            write_zip_entry(&mut zip, "mimetype", "application/epub+zip", options);
            write_zip_entry(
                &mut zip,
                "META-INF/container.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/package.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav" />
    <item id="chap" href="text/chapter.xhtml" media-type="application/xhtml+xml" media-overlay="overlay" />
    <item id="overlay" href="overlays/chapter.smil" media-type="application/smil+xml" />
    <item id="audio" href="audio/chapter.mp3" media-type="audio/mpeg" />
  </manifest>
  <spine><itemref idref="chap" /></spine>
</package>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/nav.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
                options,
            );
            write_zip_entry(
                &mut zip,
                "EPUB/text/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p id="p1">Text</p></body></html>"#,
                options,
            );
            write_zip_entry(&mut zip, "EPUB/overlays/chapter.smil", smil, options);
            write_zip_entry(&mut zip, "EPUB/audio/chapter.mp3", "audio", options);
            zip.finish().unwrap();
        }
        data.set_position(0);
        EpubZip::from_reader(data)
            .unwrap()
            .default_rendition()
            .unwrap()
    }

    #[test]
    fn fingerprint_drain_does_not_change_an_early_semantic_failure() {
        let mut bytes = b"<wrong xmlns=\"http://www.w3.org/ns/SMIL\"/>".to_vec();
        bytes.resize(20_000, b'x');
        let mut reader = Cursor::new(bytes.clone());

        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Smil),
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(10_000),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert!(matches!(
            ingest.extractions.as_slice(),
            [ExtractionOutcome::Smil(Err(AnalysisIssue::Malformed))]
        ));
        assert_eq!(
            ingest.fingerprint,
            AnalysisOutcome::Complete(Blake3Hash::hash(&bytes))
        );
    }

    #[test]
    fn non_svg_bytes_do_not_replace_byte_detected_inspection() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(b"\0\0\0\0IEND\0\0\0\0");
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Svg),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Svg),
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let inspection = ingest.inspection.value().unwrap();
        assert_eq!(
            inspection.detected_media_type().map(MediaType::as_str),
            Some("image/png")
        );
        assert!(matches!(inspection.kind(), InspectionKind::RasterImage(_)));
    }

    #[test]
    fn svg_limit_retains_early_facts_as_partial() {
        let mut bytes =
            br##"<svg xmlns="http://www.w3.org/2000/svg"><title>Title</title><use href="#root"/>"##
                .to_vec();
        bytes.resize(256, b' ');
        bytes.extend_from_slice(b"</svg>");
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Svg),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Svg),
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(128),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let [ExtractionOutcome::Svg(Ok(extraction), Some(issue))] = ingest.extractions.as_slice()
        else {
            panic!("expected partial SVG extraction")
        };
        assert_eq!(*issue, AnalysisIssue::PerResourceAnalysisLimit);
        assert_eq!(extraction.accessibility.len(), 1);
        assert_eq!(extraction.references.len(), 1);
        assert!(matches!(
            ingest.inspection,
            AnalysisOutcome::Partial {
                issue: AnalysisIssue::PerResourceAnalysisLimit,
                ..
            }
        ));
    }

    #[test]
    fn media_ingest_captures_complete_resource_once_for_symphonia() {
        let bytes = include_bytes!("../tests/fixtures/media-cbr.mp3").to_vec();
        let mut reader = Cursor::new(bytes.clone());
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Media(MediaContainer::Mp3)),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Unknown,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(bytes.len() as u64),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let AnalysisOutcome::Complete(inspection) = ingest.inspection else {
            panic!("expected complete media inspection")
        };
        let InspectionKind::Media(media) = inspection.kind() else {
            panic!("expected media facts")
        };
        assert_eq!(media.container(), "mp3");
        assert_eq!(media.tracks()[0].codec(), Some("mp3"));
        assert_eq!(ingest.analysis_bytes, bytes.len() as u64);
        assert_eq!(
            ingest.fingerprint,
            AnalysisOutcome::Complete(Blake3Hash::hash(&bytes))
        );
    }

    #[test]
    fn media_provider_failure_is_unreadable_and_never_complete() {
        struct FailAfter {
            inner: Cursor<Vec<u8>>,
            fail_at: u64,
        }

        impl Read for FailAfter {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.inner.position() >= self.fail_at {
                    return Err(std::io::Error::other("simulated media read failure"));
                }
                let remaining = usize::try_from(self.fail_at - self.inner.position()).unwrap();
                let read_len = buffer.len().min(remaining);
                self.inner.read(&mut buffer[..read_len])
            }
        }

        let bytes = include_bytes!("../tests/fixtures/media-cbr.mp3").to_vec();
        let mut reader = FailAfter {
            inner: Cursor::new(bytes),
            fail_at: 1_024,
        };
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Media(MediaContainer::Mp3)),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Unknown,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
            panic!("expected partial media inspection")
        };
        assert_eq!(issue, AnalysisIssue::Unreadable);
        let InspectionKind::Media(media) = value.kind() else {
            panic!("expected media facts")
        };
        assert_eq!(media.duration(), None);
        assert_eq!(
            ingest.fingerprint,
            AnalysisOutcome::Unavailable(AnalysisIssue::Unreadable)
        );
    }

    #[test]
    fn detected_non_media_format_controls_capture_over_declared_media() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&320u32.to_be_bytes());
        bytes.extend_from_slice(&200u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        bytes.resize(1_024, 0);
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Media(MediaContainer::Mp3)),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Unknown,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(64),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let AnalysisOutcome::Partial {
            value: inspection,
            issue,
        } = ingest.inspection
        else {
            panic!("expected partial PNG inspection")
        };
        assert_eq!(issue, AnalysisIssue::PerResourceAnalysisLimit);
        let InspectionKind::RasterImage(image) = inspection.kind() else {
            panic!("expected raster facts")
        };
        assert_eq!((image.width(), image.height()), (Some(320), Some(200)));
        assert_eq!(image.has_alpha(), None);
        assert_eq!(image.animated(), None);
        assert_eq!(image.has_icc_profile(), None);
        assert_eq!(ingest.analysis_bytes, 64);
    }

    #[test]
    fn known_and_unknown_media_capture_limits_are_partial_while_fingerprint_completes() {
        let bytes = include_bytes!("../tests/fixtures/media-cbr.mp3").to_vec();
        let limit = 512;
        for preflight in [None, Some(AnalysisIssue::PerResourceAnalysisLimit)] {
            let mut reader = Cursor::new(bytes.clone());
            let ingest = ingest_resource_reader(
                &mut reader,
                IngestPlan {
                    inspection_hint: Some(MediaTypeClassification::Media(MediaContainer::Mp3)),
                    css_candidate: false,
                    known_empty: false,
                    classification: ResourceClassification::Unknown,
                    secondary_ncx: None,
                    semantic_budget: StreamBudget {
                        limit: Some(limit),
                        preflight,
                        exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                    },
                    fingerprint_budget: StreamBudget {
                        limit: None,
                        preflight: None,
                        exceeded: AnalysisIssue::TotalFingerprintLimit,
                    },
                },
            );

            let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
                panic!("expected partial media inspection")
            };
            assert_eq!(issue, AnalysisIssue::PerResourceAnalysisLimit);
            let InspectionKind::Media(media) = value.kind() else {
                panic!("expected media facts")
            };
            assert_eq!(media.container(), "mp3");
            assert_eq!(media.duration(), None);
            assert_eq!(ingest.analysis_bytes, limit);
            assert_eq!(
                ingest.fingerprint,
                AnalysisOutcome::Complete(Blake3Hash::hash(&bytes))
            );
        }
    }

    #[test]
    fn trailing_mp4_metadata_beyond_capture_limit_is_not_malformed() {
        let bytes = include_bytes!("../tests/fixtures/media-h264-aac-tail.mp4").to_vec();
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Media(MediaContainer::Mp4)),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Unknown,
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: Some(512),
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
            panic!("expected partial MP4 inspection")
        };
        assert_eq!(issue, AnalysisIssue::PerResourceAnalysisLimit);
        let InspectionKind::Media(media) = value.kind() else {
            panic!("expected media facts")
        };
        assert_eq!(media.container(), "mp4");
        assert_eq!(media.duration(), None);
    }

    #[test]
    fn root_probe_detects_utf16_smil() {
        let xml =
            r#"<?xml version="1.0" encoding="UTF-16"?><smil xmlns="http://www.w3.org/ns/SMIL"/>"#;
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));

        assert_eq!(probe_root_format(&bytes).0, Some(SemanticFormat::Smil));
    }

    #[test]
    fn ingest_decodes_utf16_xhtml_before_extraction() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p>Hello</p></body></html>"#;
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Xhtml),
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        let [ExtractionOutcome::Xhtml(Ok(facts))] = ingest.extractions.as_slice() else {
            panic!("expected complete XHTML extraction");
        };
        assert!(
            facts
                .facts
                .text()
                .iter()
                .any(|chunk| { chunk.text(facts.facts.text_stream()).unwrap() == "Hello" })
        );
    }

    #[test]
    fn large_secondary_ncx_is_not_blocked_by_the_root_probe() {
        let mut bytes = b"<!--".to_vec();
        bytes.resize(70_000, b'x');
        bytes.extend_from_slice(
            br#"--><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap/></ncx>"#,
        );
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Unknown,
                secondary_ncx: Some(EpubPath::new("EPUB/toc.ncx").unwrap()),
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert!(matches!(
            ingest.extractions.as_slice(),
            [ExtractionOutcome::SecondaryNcx(Ok(_))]
        ));
    }

    #[test]
    fn selected_svg_still_emits_a_secondary_ncx_outcome() {
        let mut reader = Cursor::new(br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: Some(MediaTypeClassification::Svg),
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Svg),
                secondary_ncx: Some(EpubPath::new("EPUB/toc.ncx").unwrap()),
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert!(matches!(
            ingest.extractions.as_slice(),
            [
                ExtractionOutcome::Svg(Ok(_), None),
                ExtractionOutcome::SecondaryNcx(Err(AnalysisIssue::Malformed))
            ]
        ));
    }

    #[test]
    fn malformed_smil_encoding_is_not_a_provider_read_failure() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(
            br#"<?xml version="1.0" encoding="UTF-16"?><smil xmlns="http://www.w3.org/ns/SMIL"/>"#,
        );
        let mut reader = Cursor::new(bytes);
        let ingest = ingest_resource_reader(
            &mut reader,
            IngestPlan {
                inspection_hint: None,
                css_candidate: false,
                known_empty: false,
                classification: ResourceClassification::Identified(SemanticFormat::Smil),
                secondary_ncx: None,
                semantic_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::PerResourceAnalysisLimit,
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::TotalFingerprintLimit,
                },
            },
        );

        assert!(matches!(
            ingest.extractions.as_slice(),
            [ExtractionOutcome::Smil(Err(AnalysisIssue::Malformed))]
        ));
    }

    #[test]
    fn unavailable_smil_does_not_create_xhtml_relationship_coverage() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="overlay" href="missing.smil" media-type="application/smil+xml"/></manifest><spine/></package>"#;
        let provider =
            MemoryResourceProvider::from_entries([("EPUB/package.opf", package.to_vec())]).unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let analysis = epub.analyze();
        let overlay = analysis
            .resources()
            .find_unique_resource_by_id("overlay")
            .unwrap()
            .key();

        assert!(analysis.coverage().relationships().iter().any(|coverage| {
            matches!(
                (coverage.source(), coverage.state()),
                (RelationshipSource::Smil(key), CoverageState::Unavailable(_)) if *key == overlay
            )
        }));
        assert!(!analysis.coverage().relationships().iter().any(|coverage| {
            matches!(coverage.source(), RelationshipSource::Xhtml(key) if *key == overlay)
        }));
    }

    #[test]
    fn partial_smil_facts_produce_partial_relationship_coverage() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="overlay" href="overlay.smil" media-type="application/smil+xml"/></manifest><spine/></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/overlay.smil", b"<smil/>".to_vec()),
        ])
        .unwrap();
        let epub = Epub::from_provider(provider, "EPUB/package.opf").unwrap();
        let resources = epub.resources().clone();
        let overlay = resources
            .find_unique_resource_by_id("overlay")
            .unwrap()
            .key();
        let issue = AnalysisIssue::PerResourceAnalysisLimit;
        let mut facts = resources
            .resources()
            .iter()
            .map(|resource| {
                let content = if resource.key() == overlay {
                    AnalysisOutcome::Partial {
                        value: ContentFacts::Smil(SmilFacts::new(
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )),
                        issue,
                    }
                } else {
                    AnalysisOutcome::NotApplicable
                };
                ResourceFacts::new(
                    resource.key(),
                    AnalysisOutcome::NotApplicable,
                    AnalysisOutcome::NotApplicable,
                    AnalysisOutcome::NotApplicable,
                    content,
                )
            })
            .collect::<Vec<_>>();
        let mut pending = HashMap::new();
        pending.insert(overlay, Vec::new());
        let mut references = Vec::new();
        let mut coverage = Vec::new();

        collect_smil_references(
            &resources,
            &mut facts,
            &HashSet::from([overlay]),
            pending,
            &mut references,
            &mut coverage,
        );

        assert!(matches!(
            coverage.as_slice(),
            [coverage]
                if coverage.source() == &RelationshipSource::Smil(overlay)
                    && matches!(coverage.state(), CoverageState::Partial(actual) if *actual == issue)
        ));
    }

    fn write_zip_entry(
        zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
        path: &str,
        contents: &str,
        options: SimpleFileOptions,
    ) {
        zip.start_file(path, options).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
}
