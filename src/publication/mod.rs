//! Open an EPUB publication to browse its structure, read resources, analyze content, and
//! export edits.
//!
//! The public facade is [`crate::Epub`]. Opening indexes available paths, parses the package, and
//! loads at most one selected navigation document. Other resource content is read on demand.
//! Committed edits affect publication reads and exports but do not modify the underlying provider.
pub(crate) mod annotation;
mod cfi;
pub(crate) mod persistence;

use crate::navigation::facts::{
    NavigationLoadingFacts, NavigationLoadingOutcome, NavigationPositionOverflow,
    NavigationTargetFacts, navigation_targets,
};

#[cfg(test)]
use crate::analysis::orchestration::{
    ExtractionOutcome, IngestPlan, StreamBudget, collect_smil_references, ingest_resource_reader,
    probe_root_format,
};
#[cfg(test)]
use crate::{
    accessibility::{
        AccessibilityFact, AccessibilityObservation, AccessibilityObservationRef,
        AccessibilityProperty, PageBreakSourceTerm, WcagLevel,
    },
    analysis::{
        AnalysisIssue, AnalysisLimit, AnalysisOutcome, ResourceClassification, SemanticFormat,
        coverage::{Completeness, RelationshipSource},
        fingerprint::Blake3Hash,
        inspection::InspectionData,
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
    container::{ExportError, export_provider},
    edit::{EditError, StructuralResourceKind},
    navigation::{
        Heading, NavigationDepthError, NavigationDocument, NavigationGenerateError, NavigationList,
        NavigationSource, parse, parse::NavigationParseError,
    },
    package::{Package, PackageError, manifest::ManifestItem, metadata::DcElement},
    resource::provider::{
        MemoryResourceProvider, ProviderIndex, ProviderIndexError, ProviderReadError,
        ResourceProvider,
    },
    resource::{
        EpubPath, ResourceIndex, ResourceIndexError, ResourceReadError, ResourceRef,
        resolve_local_href_from_source,
    },
    semantics::EpubStructuralSemantic,
    string::EpubString,
    xml::decode_xml,
};
use persistence::ResourceChanges;
#[cfg(test)]
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
/// Provider-enumeration and structural-document limits used when opening an [`Epub`].
///
/// These limits bound provider path data and the bytes read for the package and each selected
/// navigation candidate. They do not cap ordinary resources; use [`AnalysisLimits`] for a later
/// analysis pass. Values are copied into the publication and continue to govern index rebuilds
/// performed by embedded annotation loading and export.
pub struct EpubOpenLimits {
    /// Inclusive maximum number of enumerated provider entries.
    pub max_provider_entries: NonZeroUsize,
    /// Inclusive maximum sum of canonical provider path byte lengths.
    pub max_provider_path_bytes: NonZeroUsize,
    /// Maximum bytes read for the package document.
    pub max_package_bytes: NonZeroU64,
    /// Maximum bytes read for each selected EPUB NAV or NCX candidate.
    ///
    /// A malformed or unreadable candidate is omitted from the navigation model; exceeding this
    /// limit is a hard open failure.
    pub max_navigation_bytes: NonZeroU64,
}

impl Default for EpubOpenLimits {
    /// Uses 65,536 provider entries, 8 MiB of provider path text, and 16 MiB for each structural
    /// document.
    fn default() -> Self {
        const STRUCTURAL_BYTES: NonZeroU64 = NonZeroU64::new(16 * 1024 * 1024).unwrap();
        Self {
            max_provider_entries: NonZeroUsize::new(65_536).unwrap(),
            max_provider_path_bytes: NonZeroUsize::new(8 * 1024 * 1024).unwrap(),
            max_package_bytes: STRUCTURAL_BYTES,
            max_navigation_bytes: STRUCTURAL_BYTES,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// The reason a provider could not be opened as the requested EPUB rendition.
pub enum EpubOpenFailure {
    /// The resource index cannot be represented by public ordinals.
    #[error("Could not index publication resources: {source}")]
    ResourceIndex {
        /// The resource-index overflow.
        source: ResourceIndexError,
    },
    /// The provider could not produce a complete index within the configured limits.
    #[error("Could not index the resource provider: {source}")]
    ProviderIndex {
        /// The indexing or limit failure.
        source: ProviderIndexError,
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
        source: crate::XmlDecodeError,
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
    /// The container document could not be read, parsed, or discovered.
    ///
    /// Container-level rendition selection produces this variant; [`Epub::from_provider`]
    /// receives a package path directly.
    #[error("Could not use the OCF container document: {source}")]
    ContainerDocument {
        /// The container document failure.
        source: crate::container::ContainerDocumentError,
    },
    /// The container declares no rootfile from which to select a rendition.
    #[error("The OCF container declares no rootfiles")]
    MissingRootfiles,
    /// The requested rendition index is outside the rootfile list.
    #[error("Rootfile index {index} is out of bounds for {len} renditions")]
    RootfileIndexOutOfBounds {
        /// The requested zero-based index.
        index: usize,
        /// The number of declared rootfiles.
        len: usize,
    },
    /// The selected rootfile has no usable canonical `full-path`.
    #[error("The selected rootfile has no usable package path")]
    MissingRootfilePath,
    /// The selected rootfile `full-path` is not a canonical publication path.
    #[error("Rootfile full-path {path:?} is not a canonical publication path")]
    InvalidRootfilePath {
        /// The authored `full-path` value.
        path: String,
    },
}

/// An opening failure that lets an application recover or retry with the original provider.
///
/// Use [`Self::into_provider`] or [`Self::into_parts`] to recover the provider.
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
/// An open book: read its files, inspect its structure, analyze it, edit it, write it back out.
pub struct Epub<R: ResourceProvider> {
    /// the zip archive
    pub(crate) container: R,

    pub(crate) package: Package,
    pub(crate) navigation: Option<NavigationDocument>,
    pub(crate) navigation_loading: NavigationLoadingFacts,

    pub(crate) resources: ResourceIndex,
    pub(crate) open_limits: EpubOpenLimits,
    pub(crate) provider_index: ProviderIndex,
    pub(crate) resource_changes: ResourceChanges,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Failure to construct a new minimal in-memory EPUB.
pub enum EpubCreateError {
    /// The generated resource index cannot be represented by public ordinals.
    #[error("Could not build generated EPUB resource index: {source}")]
    ResourceIndex {
        /// The resource-index overflow.
        #[from]
        source: ResourceIndexError,
    },
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
        source: NavigationGenerateError,
    },
    /// The generated in-memory resources could not be indexed.
    #[error("Could not index generated EPUB resources: {source}")]
    ProviderIndex {
        #[from]
        /// The generated in-memory provider indexing failure.
        source: ProviderIndexError,
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
    /// Starts an empty EPUB 3 book in memory: a container, a package document, and an empty
    /// table of contents.
    ///
    /// It has no chapters yet. Add them with [`Self::edit`], then [`Self::export`].
    ///
    /// # Errors
    ///
    /// [`EpubCreateError`] if the metadata you supplied cannot be represented.
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
        if package_xml.len() as u64 > open_limits.max_package_bytes.get() {
            return Err(EpubCreateError::PackageByteLimit {
                size: package_xml.len() as u64,
                limit: open_limits.max_package_bytes.get(),
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
        let publication_title = package.metadata().elements(DcElement::Title)[0]
            .content()
            .expect("minimal package title has content");
        let nav_xml = navigation.to_normalized_xhtml(publication_title)?;
        let navigation = parse::epub_nav(nav_path.clone(), &nav_xml)?;
        let provider = MemoryResourceProvider::from_entries([
            ("mimetype", b"application/epub+zip".to_vec()),
            ("META-INF/container.xml", CONTAINER_XML.as_bytes().to_vec()),
            (PACKAGE_PATH, package_xml.into_bytes()),
            (NAV_PATH, nav_xml.into_bytes()),
        ])
        .expect("generated EPUB paths are valid");
        let package_path = EpubPath::new(PACKAGE_PATH).expect("static package path is valid");
        let provider_index = ProviderIndex::of(&provider, &open_limits)?;
        let resources = ResourceIndex::new(&package, &package_path, &provider_index)?;

        Ok(Self {
            container: provider,
            package,
            navigation: Some(navigation),
            navigation_loading: NavigationLoadingFacts {
                epub_nav: NavigationLoadingOutcome::Loaded,
                ncx: NavigationLoadingOutcome::NotAttempted,
            },
            resources,
            open_limits,
            provider_index,
            resource_changes: ResourceChanges::new(),
        })
    }
}

impl<R: ResourceProvider> Epub<R> {
    /// Why [`Self::navigation`] is empty, or fell back to the NCX.
    pub fn navigation_loading(&self) -> NavigationLoadingFacts {
        self.navigation_loading
    }

    /// Where each entry in the table of contents points, resolved against this publication.
    ///
    /// Empty when the book has no navigation document.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationPositionOverflow`] if a list or point position exceeds `u32`.
    pub fn navigation_targets(
        &self,
    ) -> std::result::Result<Vec<NavigationTargetFacts>, NavigationPositionOverflow> {
        self.navigation.as_ref().map_or_else(
            || Ok(Vec::new()),
            |document| navigation_targets(document, &self.resources),
        )
    }

    /// Opens one package rendition from application-provided storage with default limits.
    ///
    /// Opening indexes all provider paths, reads and parses the package,
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
        package_path: EpubPath,
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
        package_epub_path: EpubPath,
        open_limits: EpubOpenLimits,
    ) -> std::result::Result<Self, EpubOpenError<R>> {
        let prepared = (|| -> std::result::Result<_, EpubOpenFailure> {
            let provider_index = ProviderIndex::of(&provider, &open_limits)
                .map_err(|source| EpubOpenFailure::ProviderIndex { source })?;
            let package_bytes = provider_bytes_bounded(
                &provider,
                &package_epub_path,
                open_limits.max_package_bytes.get(),
            )
            .map_err(|error| match error {
                BoundedReadError::Provider(source) => EpubOpenFailure::PackageRead {
                    path: package_epub_path.clone(),
                    source,
                },
                BoundedReadError::Limit => EpubOpenFailure::PackageByteLimit {
                    path: package_epub_path.clone(),
                    limit: open_limits.max_package_bytes.get(),
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
            let resources = ResourceIndex::new(&package, &package_epub_path, &provider_index)
                .map_err(|source| EpubOpenFailure::ResourceIndex { source })?;
            let mut navigation = None;
            let mut navigation_loading = NavigationLoadingFacts {
                epub_nav: NavigationLoadingOutcome::NotAttempted,
                ncx: NavigationLoadingOutcome::NotAttempted,
            };
            let nav_document = match resources.epub_nav_declaration() {
                Some(ordinal) => {
                    let nav_item = &package.manifest().items()[ordinal.index()];
                    let (document, outcome) = try_load_navigation(
                        &provider,
                        nav_item,
                        &package_epub_path,
                        NavigationSource::EpubNav,
                        &open_limits,
                    )?;
                    navigation_loading.epub_nav = outcome;
                    document
                }
                None => None,
            };
            if let Some(document) = nav_document {
                navigation = Some(document);
            }
            if navigation.is_none() {
                let ncx_document = match resources.ncx_declaration() {
                    Some(ordinal) => {
                        let ncx_item = &package.manifest().items()[ordinal.index()];
                        let (document, outcome) = try_load_navigation(
                            &provider,
                            ncx_item,
                            &package_epub_path,
                            NavigationSource::Ncx,
                            &open_limits,
                        )?;
                        navigation_loading.ncx = outcome;
                        document
                    }
                    None => None,
                };
                if let Some(document) = ncx_document {
                    navigation = Some(document);
                }
            }

            Ok((
                package,
                navigation,
                navigation_loading,
                resources,
                provider_index,
            ))
        })();

        let (package, navigation, navigation_loading, resources, provider_index) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err(EpubOpenError::new(error, provider)),
        };

        Ok(Self {
            container: provider,
            package,
            navigation,
            navigation_loading,
            resources,
            open_limits,
            provider_index,
            resource_changes: ResourceChanges::new(),
        })
    }

    /// What this publication is made of, and how its declarations resolve.
    pub fn resources(&self) -> &ResourceIndex {
        &self.resources
    }

    /// Reads every resource and works out what the book contains: its text, its images, the
    /// links between its files.
    ///
    /// This is the expensive call — it opens each resource in turn. A resource that cannot be
    /// read does not fail the analysis; it is recorded as incomplete and the rest proceeds.
    ///
    /// The result describes the book as it was when analyzed. After an edit it still answers,
    /// but it answers about the old version; analyze again.
    pub fn analyze(&self) -> PublicationAnalysis {
        crate::analysis::orchestration::analyze(self, AnalysisLimits::default())
    }

    /// Analyzes under your own byte and count budgets, for untrusted or very large books.
    ///
    /// Whatever a budget cut short is reported as incomplete rather than silently skipped.
    pub fn analyze_with_limits(&self, limits: AnalysisLimits) -> PublicationAnalysis {
        crate::analysis::orchestration::analyze(self, limits)
    }

    /// Hands a reader for a file to your callback, without loading it all into memory.
    ///
    /// Any file the container holds can be read this way, including `mimetype` and
    /// `META-INF/container.xml`, which [`Self::resources`] does not list.
    ///
    /// Nothing is cached, so each call reads again.
    ///
    /// # Errors
    ///
    /// [`ResourceReadError::Missing`] if nothing is there, or
    /// [`ResourceReadError::Provider`] if it cannot be opened.
    pub fn read_with<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> std::result::Result<T, ResourceReadError> {
        if !self.is_committed(path) {
            return Err(ResourceReadError::Missing { path: path.clone() });
        }
        self.read_committed(path, read)
    }

    /// Reads a whole file into memory.
    ///
    /// However large it is. For a bound, use [`Self::read_with`] with a limited reader.
    ///
    /// # Errors
    ///
    /// As [`Self::read_with`], plus any failure part-way through the stream.
    pub fn bytes(&self, path: &EpubPath) -> std::result::Result<Vec<u8>, ResourceReadError> {
        self.read_with(path, |reader| read_all(reader, path))?
    }

    /// Reads a whole file as UTF-8 text.
    ///
    /// The bytes must already be UTF-8: an XML declaration naming another encoding is not
    /// honoured here.
    ///
    /// # Errors
    ///
    /// As [`Self::bytes`], plus [`ResourceReadError::InvalidUtf8`].
    pub fn utf8_text(&self, path: &EpubPath) -> std::result::Result<String, ResourceReadError> {
        String::from_utf8(self.bytes(path)?).map_err(|source| ResourceReadError::InvalidUtf8 {
            path: path.clone(),
            source,
        })
    }

    pub(crate) fn read_committed<T>(
        &self,
        path: &EpubPath,
        read: impl FnOnce(&mut dyn Read) -> T,
    ) -> std::result::Result<T, ResourceReadError> {
        read_committed(&self.container, &self.resource_changes, path, read)
    }

    /// Reports whether the committed provider view has bytes at `path`.
    pub(crate) fn is_committed(&self, path: &EpubPath) -> bool {
        match self.resource_changes.entry(path) {
            Some(change) => change.is_some(),
            None => self.provider_index.get(path).is_some(),
        }
    }

    /// Gives back the storage this book was opened from, as it was.
    ///
    /// **Committed edits are discarded.** [`Self::export`] first to keep them.
    pub fn into_base_provider(self) -> R {
        self.container
    }

    /// Returns the parsed package metadata, manifest, and spine for the current state.
    ///
    /// The model is semantic rather than a complete copy of original OPF lexical formatting.
    pub fn package(&self) -> &Package {
        &self.package
    }
    /// The book's table of contents, page list and landmarks.
    ///
    /// `None` when neither the EPUB navigation document nor the NCX could be loaded;
    /// [`Self::navigation_loading`] says why.
    pub fn navigation(&self) -> Option<&NavigationDocument> {
        self.navigation.as_ref()
    }
    /// Writes the book, with every committed edit, as a new EPUB.
    ///
    /// Files you did not touch keep their exact bytes. The archive around them is rewritten:
    /// entry order, compression and timestamps are normalized, so the output will not be
    /// byte-identical to the input even if you changed nothing.
    ///
    /// You can keep editing afterwards; exporting does not end the session or clear anything.
    ///
    /// # Errors
    ///
    /// [`ExportError`] if a resource cannot be read or the output cannot be written. A failure
    /// part-way leaves the writer partially written.
    pub fn export<A: std::io::Write + std::io::Seek>(
        &self,
        writer: A,
    ) -> std::result::Result<A, ExportError> {
        let index = self
            .resource_changes
            .apply_to_index(&self.provider_index, &self.open_limits)
            .map_err(|source| ExportError::ProviderIndex { source })?;
        export_provider(&self.container, &index, &self.resource_changes, writer)
    }

    /// Writes the book to a file, creating or truncating it.
    ///
    /// Not atomic: the file is created first, so a failure can leave it missing, empty or
    /// half-written. Export to a temporary path and rename if that matters.
    ///
    /// # Errors
    ///
    /// [`ExportError::OutputPath`] if the file cannot be created, or anything [`Self::export`]
    /// reports while writing.
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
        navigation: Option<NavigationDocument>,
        resource_changes: ResourceChanges,
        resources: ResourceIndex,
    ) {
        let navigation_loading = self.navigation_loading_after_edit(&navigation);
        self.package = package;
        self.navigation = navigation;
        self.navigation_loading = navigation_loading;
        self.resource_changes = resource_changes;
        self.resources = resources;
    }

    pub(crate) fn navigation_loading_after_edit(
        &self,
        navigation: &Option<NavigationDocument>,
    ) -> NavigationLoadingFacts {
        let previous_source = self
            .navigation
            .as_ref()
            .map(|document| (document.source(), document.path()));
        let next_source = navigation
            .as_ref()
            .map(|document| (document.source(), document.path()));
        if previous_source == next_source {
            self.navigation_loading
        } else {
            match next_source.map(|(source, _)| source) {
                Some(NavigationSource::EpubNav) => NavigationLoadingFacts {
                    epub_nav: NavigationLoadingOutcome::Loaded,
                    ncx: NavigationLoadingOutcome::NotAttempted,
                },
                Some(NavigationSource::Ncx) => NavigationLoadingFacts {
                    epub_nav: NavigationLoadingOutcome::NotAttempted,
                    ncx: NavigationLoadingOutcome::Loaded,
                },
                None => NavigationLoadingFacts {
                    epub_nav: NavigationLoadingOutcome::NotAttempted,
                    ncx: NavigationLoadingOutcome::NotAttempted,
                },
            }
        }
    }

    pub(crate) fn committed_resource_exists(&self, path: &EpubPath) -> bool {
        match self.resource_changes.entry(path) {
            Some(change) => change.is_some(),
            None => self.provider_index.get(path).is_some(),
        }
    }

    pub(crate) fn structural_resource_kind(
        &self,
        path: &EpubPath,
    ) -> Option<StructuralResourceKind> {
        if path == self.resources.package_path() {
            return Some(StructuralResourceKind::Package);
        }
        if self.resources.epub_nav().and_then(ResourceRef::local_path) == Some(path) {
            return Some(StructuralResourceKind::Navigation);
        }
        (self.resources.ncx().and_then(ResourceRef::local_path) == Some(path))
            .then_some(StructuralResourceKind::Ncx)
    }
}

fn try_load_navigation<R: ResourceProvider>(
    provider: &R,
    item: &ManifestItem,
    package_path: &EpubPath,
    source: NavigationSource,
    options: &EpubOpenLimits,
) -> std::result::Result<(Option<NavigationDocument>, NavigationLoadingOutcome), EpubOpenFailure> {
    let Some(epub_path) = structural_manifest_href_path(item, package_path) else {
        return Ok((None, NavigationLoadingOutcome::NotAttempted));
    };
    let bytes =
        match provider_bytes_bounded(provider, &epub_path, options.max_navigation_bytes.get()) {
            Ok(bytes) => bytes,
            Err(BoundedReadError::Limit) => {
                return Err(EpubOpenFailure::SelectedNavigationByteLimit {
                    path: epub_path,
                    limit: options.max_navigation_bytes.get(),
                });
            }
            Err(BoundedReadError::Provider(ProviderReadError::Missing { .. })) => {
                return Ok((None, NavigationLoadingOutcome::MissingResource));
            }
            Err(BoundedReadError::Provider(_)) => {
                return Ok((None, NavigationLoadingOutcome::ReadFailed));
            }
        };
    let xml = match decode_xml(&bytes) {
        Ok(xml) => xml,
        Err(_) => return Ok((None, NavigationLoadingOutcome::InvalidEncoding)),
    };
    let parsed = match source {
        NavigationSource::EpubNav => parse::epub_nav(epub_path, &xml),
        NavigationSource::Ncx => parse::ncx(epub_path, &xml),
    };
    Ok(match parsed {
        Ok(document) => (Some(document), NavigationLoadingOutcome::Loaded),
        Err(_) => (None, NavigationLoadingOutcome::Malformed),
    })
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
                .map_err(|source| ProviderReadError::Io {
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

pub(crate) fn read_committed<R: ResourceProvider, T>(
    provider: &R,
    changes: &ResourceChanges,
    path: &EpubPath,
    read: impl FnOnce(&mut dyn Read) -> T,
) -> std::result::Result<T, ResourceReadError> {
    match changes.entry(path) {
        Some(Some(bytes)) => Ok(read(&mut Cursor::new(bytes))),
        Some(None) => Err(ResourceReadError::Missing { path: path.clone() }),
        None => provider.read_with(path, read).map_err(Into::into),
    }
}

pub(crate) fn committed_bytes<R: ResourceProvider>(
    provider: &R,
    changes: &ResourceChanges,
    path: &EpubPath,
) -> std::result::Result<Vec<u8>, ResourceReadError> {
    read_committed(provider, changes, path, |reader| read_all(reader, path))?
}

fn read_all(
    reader: &mut dyn Read,
    path: &EpubPath,
) -> std::result::Result<Vec<u8>, ResourceReadError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| ProviderReadError::Io {
            path: path.clone(),
            source,
        })?;
    Ok(bytes)
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::analysis::impact::{ImpactError, StructuralChange};
    use crate::cfi::Cfi;
    use crate::container::EpubZip;
    use crate::media_overlay::SmilFacts;
    use crate::resource::provider::{MemoryResourceProvider, ProviderReadError, ResourceProvider};
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
        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> std::result::Result<T, ProviderReadError> {
            let bytes = self
                .entries
                .get(path.as_str())
                .ok_or_else(|| ProviderReadError::Missing { path: path.clone() })?;
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

        fn entries(
            &self,
        ) -> std::result::Result<impl Iterator<Item = (EpubPath, Option<u64>)>, ProviderIndexError>
        {
            Ok(self.entries.iter().map(|(path, bytes)| {
                (
                    EpubPath::new(path).expect("test paths are valid"),
                    Some(bytes.len() as u64),
                )
            }))
        }
    }

    #[test]
    fn epub_open_limits_expose_conservative_defaults() {
        let limits = EpubOpenLimits::default();
        assert_eq!(limits.max_package_bytes.get(), 16 * 1024 * 1024);
        assert_eq!(limits.max_navigation_bytes.get(), 16 * 1024 * 1024);
    }

    #[test]
    fn focused_package_path_read_and_decode_failures_return_provider() {
        let provider =
            MemoryResourceProvider::from_entries([("EPUB/other.txt", b"not a package".to_vec())])
                .unwrap();
        let error =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::PackageRead {
                source: ProviderReadError::Missing { .. },
                ..
            }
        ));

        let provider = MemoryResourceProvider::from_entries([(
            "EPUB/package.opf",
            b"<package>\xff</package>".to_vec(),
        )])
        .unwrap();
        let error =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap_err();
        assert!(matches!(
            error.failure(),
            EpubOpenFailure::PackageXmlDecode {
                source: crate::XmlDecodeError::InvalidBytes { .. },
                ..
            }
        ));
    }

    #[test]
    fn fatal_index_failure_returns_provider() {
        let provider = memory_provider();

        let error = Epub::from_provider_with_limits(
            provider,
            EpubPath::new("EPUB/package.opf").unwrap(),
            EpubOpenLimits {
                max_provider_entries: NonZeroUsize::new(1).unwrap(),
                ..EpubOpenLimits::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            error.failure(),
            EpubOpenFailure::ProviderIndex {
                source: crate::resource::provider::ProviderIndexError::EntryLimit { limit: 1 }
            }
        ));
        let provider = error.into_provider();
        assert!(
            provider
                .get(&EpubPath::new("EPUB/package.opf").unwrap())
                .is_some()
        );
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
        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_some()
        );
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
        assert!(
            epub.navigation()
                .filter(|document| document.is_ncx())
                .is_some()
        );
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
        Epub::from_provider(
            memory_provider(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn resource_reads_separate_provider_and_callback_failures() {
        let mut epub = memory_provider_epub();
        let chapter = EpubPath::new("EPUB/text/chapter.xhtml").unwrap();

        assert_eq!(
            epub.utf8_text(&chapter).unwrap(),
            "<html><body>Chapter</body></html>"
        );
        let callback_result = epub
            .read_with(&chapter, |_| Err::<(), _>("parser rejected resource"))
            .unwrap();
        assert_eq!(callback_result, Err("parser rejected resource"));

        epub.edit()
            .upsert_resource(chapter.clone(), b"chapter \xff".to_vec())
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(matches!(
            epub.utf8_text(&chapter),
            Err(ResourceReadError::InvalidUtf8 { .. })
        ));

        epub.edit()
            .remove_resource(chapter.clone())
            .unwrap()
            .preview()
            .unwrap()
            .commit();
        assert!(matches!(
            epub.bytes(&chapter),
            Err(ResourceReadError::Missing { .. })
        ));
        assert!(matches!(
            epub.bytes(&EpubPath::new("EPUB/absent.xhtml").unwrap()),
            Err(ResourceReadError::Missing { .. })
        ));
    }

    #[test]
    fn reads_serve_ocf_control_entries_outside_the_resource_inventory() {
        let mut provider = memory_provider();
        let mimetype = EpubPath::new("mimetype").unwrap();
        let container = EpubPath::new("META-INF/container.xml").unwrap();
        provider.insert(mimetype.clone(), b"application/epub+zip".to_vec());
        provider.insert(container.clone(), b"<container/>".to_vec());
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert_eq!(epub.bytes(&mimetype).unwrap(), b"application/epub+zip");
        assert_eq!(epub.utf8_text(&container).unwrap(), "<container/>");
        assert!(epub.resources().resource_by_path(&mimetype).is_none());
        assert!(epub.resources().resource_by_path(&container).is_none());
    }

    #[test]
    fn declared_resources_without_provider_bytes_are_missing() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0" unique-identifier="uid">
  <metadata><dc:title>T</dc:title><dc:identifier id="uid">id</dc:identifier><dc:language>en</dc:language></metadata>
  <manifest><item id="ghost" href="ghost.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="ghost"/></spine>
</package>"#;
        let provider =
            MemoryResourceProvider::from_entries([("EPUB/package.opf", package.to_vec())]).unwrap();
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let ghost = EpubPath::new("EPUB/ghost.xhtml").unwrap();
        assert!(epub.resources().resource_by_path(&ghost).is_some());
        assert!(matches!(
            epub.bytes(&ghost),
            Err(ResourceReadError::Missing { .. })
        ));
    }

    fn open_limits_with_byte_limits(package: u64, navigation: u64) -> EpubOpenLimits {
        EpubOpenLimits {
            max_package_bytes: NonZeroU64::new(package).unwrap(),
            max_navigation_bytes: NonZeroU64::new(navigation).unwrap(),
            ..EpubOpenLimits::default()
        }
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
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
        Epub::from_provider_with_limits(
            provider,
            EpubPath::new("EPUB/package.opf").unwrap(),
            limits,
        )
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        reset_scan_count();
        let analysis = epub.analyze();
        let style = analysis
            .resources()
            .declaration_by_id("style")
            .unwrap()
            .resource()
            .unwrap();
        let image = analysis
            .resources()
            .declaration_by_id("image")
            .unwrap()
            .resource()
            .unwrap();
        let not_css = analysis
            .resources()
            .declaration_by_id("not-css")
            .unwrap()
            .resource()
            .unwrap();
        let provider_css_path = EpubPath::new("EPUB/provider.css").unwrap();
        let provider_css = analysis
            .resources()
            .resources()
            .find(|resource| resource.local_path() == Some(&provider_css_path))
            .unwrap();
        assert!(matches!(
            analysis.resource(style.ordinal()).unwrap().content(),
            AnalysisOutcome::Complete(ContentFacts::Css)
        ));
        assert!(analysis
            .resource(style.ordinal()).unwrap().references()
            .any(|reference| {
                reference.role() == HrefRole::CssUrl
                    && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == image.ordinal())
            }));
        let inspection = analysis
            .resource(image.ordinal())
            .unwrap()
            .inspection()
            .value()
            .unwrap();
        assert!(
            !analysis
                .resource(image.ordinal())
                .unwrap()
                .content()
                .is_not_applicable()
        );
        assert!(matches!(
            analysis.resource(provider_css.ordinal()).unwrap().content(),
            AnalysisOutcome::Complete(ContentFacts::Css)
        ));
        assert!(
            analysis
                .resource(not_css.ordinal())
                .unwrap()
                .content()
                .is_not_applicable()
        );
        assert!(
            analysis
                .resource(provider_css.ordinal())
                .unwrap()
                .inspection()
                .value()
                .unwrap()
                .detected_media_type()
                .is_none()
        );
        assert!(matches!(
            analysis
                .resource(provider_css.ordinal())
                .unwrap()
                .inspection()
                .value()
                .unwrap()
                .data(),
            InspectionData::Text(_)
        ));
        let InspectionData::RasterImage(image) = inspection.data() else {
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();
        let fallback = analysis
            .resources()
            .declaration_by_id("fallback")
            .unwrap()
            .resource()
            .unwrap();
        let other = analysis
            .resources()
            .declaration_by_id("other")
            .unwrap()
            .resource()
            .unwrap();
        let declaration = analysis.resources().declaration_by_id("chapter").unwrap();

        let resource_closure = analysis
            .resource(chapter.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(!resource_closure.resources().contains(&fallback.ordinal()));
        assert!(!resource_closure.resources().contains(&other.ordinal()));
        assert_eq!(resource_closure.unresolved().len(), 1);
        assert!(resource_closure.is_complete());
        assert_eq!(
            resource_closure,
            analysis
                .resource(chapter.ordinal())
                .unwrap()
                .dependency_closure()
        );
        assert!(resource_closure.resources().iter().any(|key| {
            analysis.resources().resource(*key).is_some_and(|resource| {
                resource.local_path().and_then(EpubPath::extension) == Some("css")
            })
        }));
        assert!(
            resource_closure.resources().len() < 7,
            "cycles must terminate"
        );

        let declaration_closure = analysis
            .declaration(declaration.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(
            declaration_closure
                .resources()
                .contains(&fallback.ordinal())
        );

        let removal = analysis
            .resource(chapter.ordinal())
            .unwrap()
            .impact_of_removal()
            .unwrap();
        assert_eq!(removal.incoming().len(), 2);
        assert!(removal.outgoing_rebased().is_empty());
        assert!(removal.structural_changes().iter().any(|impact| matches!(
            impact,
            StructuralChange::ManifestDeclaration(key) if *key == declaration.ordinal()
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
            .resource(chapter.ordinal())
            .unwrap()
            .impact_of_move(&EpubPath::new("EPUB/chapters/chapter.xhtml").unwrap())
            .unwrap();
        assert_eq!(movement.incoming().len(), 2);
        assert_eq!(movement.outgoing_rebased().len(), 4);
        assert!(
            movement
                .structural_changes()
                .iter()
                .any(|impact| matches!(impact, StructuralChange::NavigationReference(_)))
        );

        let limited = epub.analyze_with_limits(AnalysisLimits {
            max_analyzed_resources: Some(0),
            ..AnalysisLimits::default()
        });
        let limited_chapter = limited
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();
        let closure = limited
            .resource(limited_chapter.ordinal())
            .unwrap()
            .dependency_closure();
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let entries = analysis.resources().reading_order().collect::<Vec<_>>();

        assert_eq!(entries[0].target(), None);
        assert_eq!(
            entries[1].target(),
            Some(&crate::resource::IdrefTarget::Missing)
        );
        assert_eq!(
            entries[2]
                .target()
                .map(crate::resource::IdrefTarget::candidates),
            Some(
                [
                    crate::resource::ManifestOrdinal::new(0),
                    crate::resource::ManifestOrdinal::new(1)
                ]
                .as_slice()
            )
        );

        let foreign = Epub::from_provider(
            memory_provider(),
            EpubPath::new("EPUB/package.opf").unwrap(),
        )
        .unwrap()
        .analyze();
        let foreign_key = foreign
            .resources()
            .declaration_by_id("chap")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();
        assert!(analysis.resource(foreign_key).is_some());
        assert!(
            analysis
                .resource(foreign_key)
                .unwrap()
                .impact_of_removal()
                .is_ok()
        );
        let package = analysis.resources().package().ordinal();
        assert_eq!(
            analysis.resource(package).unwrap().impact_of_removal(),
            Err(ImpactError)
        );
        assert_eq!(
            analysis
                .resource(package)
                .unwrap()
                .impact_of_move(&EpubPath::new("package.opf").unwrap()),
            Err(ImpactError)
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
        let analysis = Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap()
            .analyze();
        let figure = analysis
            .resources()
            .declaration_by_id("figure")
            .unwrap()
            .resource()
            .unwrap();
        let broken = analysis.resources().declaration_by_id("broken").unwrap();

        let figure_closure = analysis
            .resource(figure.ordinal())
            .unwrap()
            .dependency_closure();
        assert_eq!(figure_closure.unresolved().len(), 1);
        assert!(matches!(
            figure_closure.unresolved(),
            [AuthoredReference::Href(reference)]
                if matches!(reference.target(), HrefTarget::Fragment { exists: Some(false), .. })
        ));
        assert!(figure_closure.is_complete());

        let declaration_closure = analysis
            .declaration(broken.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(declaration_closure.resources().is_empty());
        assert!(declaration_closure.is_complete());

        let reading_order = analysis.resources().reading_order().next().unwrap();
        let reading_order_closure = analysis
            .declaration(reading_order.declaration().unwrap().ordinal())
            .unwrap()
            .dependency_closure();
        assert_eq!(reading_order_closure, declaration_closure);
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();
        let impact = analysis
            .resource(chapter.ordinal())
            .unwrap()
            .impact_of_move(&EpubPath::new("EPUB/chapters/chapter.xhtml").unwrap())
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
            !matches!(reference, AuthoredReference::Href(reference) if reference.source() == chapter.ordinal())
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();

        let references = analysis
            .resource(chapter.ordinal())
            .unwrap()
            .references()
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .declaration_by_id("figure")
            .unwrap()
            .resource()
            .unwrap();
        let script = analysis
            .resources()
            .declaration_by_id("script")
            .unwrap()
            .resource()
            .unwrap();

        let references = analysis
            .resource(figure.ordinal())
            .unwrap()
            .references()
            .collect::<Vec<_>>();
        assert!(matches!(
            references.as_slice(),
            [reference]
                if reference.role() == HrefRole::Script
                    && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == script.ordinal())
                    && matches!(reference.context(), ReferenceContext::Element(context) if context.element() == "script" && context.attribute() == "href")
        ));
        let closure = analysis
            .resource(figure.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(closure.resources().contains(&script.ordinal()));
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let chapter = analysis
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();
        let remote = analysis
            .resources()
            .declaration_by_id("remote")
            .unwrap()
            .resource()
            .unwrap();
        let closure = analysis
            .resource(chapter.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(closure.resources().contains(&remote.ordinal()));
        assert!(
            closure
                .incomplete_sources()
                .contains(&RelationshipSource::Css(remote.ordinal()))
        );

        let one = analysis.resources().declaration_by_id("one").unwrap();
        let two = analysis.resources().declaration_by_id("two").unwrap();
        let a = analysis
            .resources()
            .declaration_by_id("a")
            .unwrap()
            .resource()
            .unwrap();
        let b = analysis
            .resources()
            .declaration_by_id("b")
            .unwrap()
            .resource()
            .unwrap();
        let one_closure = analysis
            .declaration(one.ordinal())
            .unwrap()
            .dependency_closure();
        let two_closure = analysis
            .declaration(two.ordinal())
            .unwrap()
            .dependency_closure();
        assert!(one_closure.resources().contains(&a.ordinal()));
        assert!(!one_closure.resources().contains(&b.ordinal()));
        assert!(two_closure.resources().contains(&b.ordinal()));
        assert!(!two_closure.resources().contains(&a.ordinal()));

        let removal = analysis
            .resource(a.ordinal())
            .unwrap()
            .impact_of_removal()
            .unwrap();
        assert!(removal.incoming().iter().any(|reference| matches!(
            reference,
            AuthoredReference::Manifest(reference)
                if matches!(reference.target(), ManifestTarget::Ambiguous { .. })
        )));
        assert!(
            analysis
                .coverage()
                .incomplete_relationships()
                .any(|source| source == RelationshipSource::Css(remote.ordinal()))
        );

        let remote_move = analysis
            .resource(remote.ordinal())
            .unwrap()
            .impact_of_move(&EpubPath::new("EPUB/styles/book.css").unwrap())
            .unwrap();
        assert_eq!(remote_move.resource(), remote.ordinal());
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .declaration_by_id("figure")
            .unwrap()
            .resource()
            .unwrap();
        let chapter = analysis
            .resources()
            .declaration_by_id("chapter")
            .unwrap()
            .resource()
            .unwrap();
        let provider_path = EpubPath::new("EPUB/images/provider.svg").unwrap();
        let provider_figure = analysis
            .resources()
            .resources()
            .find(|resource| resource.local_path() == Some(&provider_path))
            .unwrap();
        let figure_facts = analysis
            .resource(figure.ordinal())
            .unwrap()
            .content()
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
                .resource(provider_figure.ordinal())
                .unwrap()
                .content()
                .value()
                .and_then(ContentFacts::as_svg)
                .is_some()
        );
        let inspection = analysis
            .resource(figure.ordinal())
            .unwrap()
            .inspection()
            .value()
            .unwrap();
        let InspectionData::Svg(inspection) = inspection.data() else {
            panic!("expected SVG inspection");
        };
        assert_eq!(inspection.title(), Some("Figure title"));
        assert_eq!(inspection.description(), Some("Figure description"));
        let svg_references = analysis
            .resource(figure.ordinal())
            .unwrap()
            .references()
            .collect::<Vec<_>>();
        assert_eq!(svg_references.len(), 4);
        assert!(
            svg_references
                .iter()
                .all(|reference| matches!(reference.context(), ReferenceContext::Element(_)))
        );
        assert!(svg_references.iter().any(|reference| {
            reference.declared().as_str() == "chapter.xhtml#p1"
                && matches!(
                    reference.target(),
                    HrefTarget::Fragment {
                        resource,
                        exists: Some(true),
                        ..
                    } if *resource == chapter.ordinal()
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
                && matches!(reference.target(), HrefTarget::Resource { resource, .. } if *resource == figure.ordinal())
        }));
        let svg_coverage = analysis
            .coverage()
            .relationships()
            .iter()
            .filter(|coverage| matches!(coverage.source, RelationshipSource::Svg(_)))
            .collect::<Vec<_>>();
        assert_eq!(svg_coverage.len(), 2);
        assert!(
            svg_coverage
                .iter()
                .all(|coverage| matches!(&coverage.completeness, Completeness::Complete))
        );

        let accessibility = analysis.accessibility();
        assert_eq!(
            accessibility
                .metadata()
                .values_of(AccessibilityProperty::Feature)
                .count(),
            2
        );
        assert!(
            accessibility
                .metadata()
                .values()
                .any(|value| value.property()
                    == AccessibilityProperty::PageBreakSource(
                        PageBreakSourceTerm::LegacyPageSource
                    ))
        );
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
                        fact: AccessibilityFact { observation: AccessibilityObservation::SvgTitle { .. }, .. },
                    } if resource.ordinal() == figure.ordinal()
                )
            })
            .unwrap();
        assert!(matches!(
            svg_title,
            AccessibilityObservationRef::Content {
                fact: AccessibilityFact {
                    observation: AccessibilityObservation::SvgTitle { .. },
                    ..
                },
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
                    .all(|coverage| matches!(&coverage.completeness, Completeness::Complete)),
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let analysis = epub.analyze();
        let figure = analysis
            .resources()
            .declaration_by_id("figure")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();
        assert!(matches!(
            analysis.resource(figure).unwrap().content(),
            AnalysisOutcome::Unavailable(AnalysisIssue::Missing)
        ));
        assert!(analysis.coverage().content().incomplete().any(|work| {
            work.resource == figure
                && work.completeness == Completeness::Unavailable(AnalysisIssue::Missing)
        }));
        assert!(
            analysis
                .coverage()
                .relationships()
                .iter()
                .any(|coverage| matches!(
                    (coverage.source, coverage.completeness),
                    (RelationshipSource::Svg(key), Completeness::Unavailable(AnalysisIssue::Missing))
                        if key == figure
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze_with_limits(AnalysisLimits {
            max_resource_analysis_bytes: Some(40),
            ..AnalysisLimits::default()
        });
        let image = analysis
            .resources()
            .declaration_by_id("image")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();

        let AnalysisOutcome::Partial { value, issue } =
            analysis.resource(image).unwrap().inspection()
        else {
            panic!("expected partial image inspection")
        };
        assert_eq!(
            *issue,
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)
        );
        let InspectionData::RasterImage(facts) = value.data() else {
            panic!("expected raster facts")
        };
        assert_eq!((facts.width(), facts.height()), (Some(320), Some(200)));
        assert!(analysis.coverage().inspection().incomplete().any(|work| {
            work.resource == image && matches!(work.completeness, Completeness::Partial(_))
        }));
    }

    #[test]
    fn empty_unknown_resource_is_complete_at_a_zero_byte_budget() {
        let package = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="empty" href="empty.bin" media-type="application/octet-stream"/></manifest><spine/></package>"#;
        let provider = MemoryResourceProvider::from_entries([
            ("EPUB/package.opf", package.to_vec()),
            ("EPUB/empty.bin", Vec::new()),
        ])
        .unwrap();
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze_with_limits(AnalysisLimits {
            max_resource_analysis_bytes: Some(0),
            max_total_analysis_bytes: Some(0),
            ..AnalysisLimits::default()
        });
        let empty = analysis
            .resources()
            .declaration_by_id("empty")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();

        assert!(
            analysis
                .resource(empty)
                .unwrap()
                .content()
                .is_not_applicable()
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        assert!(matches!(
            ingest.classification,
            AnalysisOutcome::Unavailable(AnalysisIssue::Limit(
                AnalysisLimit::ResourceAnalysisBytes
            ))
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: Some(u64::from(u32::MAX) + 1),
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
        assert!(analysis.analyzed_resources().any(|facts| {
            matches!(facts.content(), AnalysisOutcome::Complete(content) if content.as_smil().is_some())
        }));
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
            .declaration_by_id("overlay")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();
        let reading_order = analysis
            .resources()
            .reading_order()
            .next()
            .unwrap()
            .ordinal();
        assert_eq!(
            analysis
                .media_overlay_associations()
                .find(|association| association.reading_order().ordinal() == reading_order)
                .unwrap()
                .reference()
                .declared()
                .as_str(),
            "overlay"
        );
        let sequence = analysis
            .resource(smil)
            .unwrap()
            .smil_roots()
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
        let analysis = Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap())
            .unwrap()
            .analyze();

        for (id, declared) in [("one", "one.xhtml"), ("two", "two.xhtml")] {
            let resource = analysis
                .resources()
                .declaration_by_id(id)
                .unwrap()
                .resource()
                .unwrap()
                .ordinal();
            let root = analysis
                .resource(resource)
                .unwrap()
                .smil_roots()
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
        fn read_with<T>(
            &self,
            path: &EpubPath,
            read: impl FnOnce(&mut dyn Read) -> T,
        ) -> std::result::Result<T, ProviderReadError> {
            let path_value = path.as_str().to_string();
            self.reads.borrow_mut().push(path_value.clone());
            let bytes = self
                .entries
                .get(&path_value)
                .ok_or_else(|| ProviderReadError::Missing { path: path.clone() })?;
            let mut cursor = Cursor::new(bytes.as_slice());
            Ok(read(&mut cursor))
        }

        fn entries(
            &self,
        ) -> std::result::Result<impl Iterator<Item = (EpubPath, Option<u64>)>, ProviderIndexError>
        {
            Ok(self.entries.iter().map(|(path, bytes)| {
                (
                    EpubPath::new(path).expect("test paths are valid"),
                    Some(bytes.len() as u64),
                )
            }))
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
        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        let analysis = epub.analyze();

        assert_eq!(
            analysis
                .analyzed_resources()
                .filter_map(|resource| resource.content().value()?.as_xhtml())
                .map(crate::content::XhtmlFacts::text_stream)
                .filter(|stream| !stream.text().is_empty())
                .count(),
            1
        );
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_some()
        );
        assert!(
            epub.navigation()
                .filter(|document| document.is_ncx())
                .is_none()
        );
        assert_eq!(epub.resources().reading_order().count(), 1);
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

        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_some()
        );
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(epub.navigation().is_none());
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_none()
        );
        assert!(
            epub.navigation()
                .filter(|document| document.is_ncx())
                .is_some()
        );
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

        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(
            epub.navigation()
                .filter(|document| document.is_epub_nav())
                .is_none()
        );
        assert!(
            epub.navigation()
                .filter(|document| document.is_ncx())
                .is_some()
        );
    }

    #[test]
    fn from_provider_missing_or_malformed_only_nav_succeeds_with_empty_navigation() {
        for nav in [None, Some("not XML")] {
            let mut provider = memory_provider();
            let nav_path = EpubPath::new("EPUB/nav.xhtml").unwrap();
            match nav {
                Some(nav) => {
                    provider.insert(nav_path, nav.as_bytes().to_vec());
                }
                None => {
                    provider.remove(&nav_path);
                }
            }

            let epub =
                Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

            assert!(epub.navigation().is_none());
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(epub.navigation().is_none());
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();

        assert!(epub.navigation().is_none());
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

        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        assert!(analysis.analyzed_resources().any(|facts| {
            matches!(facts.content(), AnalysisOutcome::Complete(content) if content.as_smil().is_some())
        }));
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
        let epub =
            Epub::from_provider(container, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let cfi = Cfi::from_str("epubcfi(/6/2!/2/2,/1:0,/1:5)").unwrap();

        let crate::cfi::ResolvedCfi::Range(range) = epub.resolve_cfi(&cfi).unwrap() else {
            panic!("expected a range");
        };

        assert_eq!(range.text(), "Hello");
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let inspection = ingest.inspection.value().unwrap();
        assert_eq!(
            inspection.detected_media_type().map(MediaType::as_str),
            Some("image/png")
        );
        assert!(matches!(inspection.data(), InspectionData::RasterImage(_)));
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let [ExtractionOutcome::Svg(Ok(extraction), Some(issue))] = ingest.extractions.as_slice()
        else {
            panic!("expected partial SVG extraction")
        };
        assert_eq!(
            *issue,
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)
        );
        assert_eq!(extraction.accessibility.len(), 1);
        assert_eq!(extraction.references.len(), 1);
        assert!(matches!(
            ingest.inspection,
            AnalysisOutcome::Partial {
                issue: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let AnalysisOutcome::Complete(inspection) = ingest.inspection else {
            panic!("expected complete media inspection")
        };
        let InspectionData::Media(media) = inspection.data() else {
            panic!("expected media facts")
        };
        assert_eq!(media.container(), MediaContainer::Mp3);
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
            panic!("expected partial media inspection")
        };
        assert_eq!(issue, AnalysisIssue::Unreadable);
        let InspectionData::Media(media) = value.data() else {
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
        assert_eq!(
            issue,
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)
        );
        let InspectionData::RasterImage(image) = inspection.data() else {
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
        for preflight in [
            None,
            Some(AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)),
        ] {
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
                        exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                    },
                    fingerprint_budget: StreamBudget {
                        limit: None,
                        preflight: None,
                        exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                    },
                },
            );

            let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
                panic!("expected partial media inspection")
            };
            assert_eq!(
                issue,
                AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)
            );
            let InspectionData::Media(media) = value.data() else {
                panic!("expected media facts")
            };
            assert_eq!(media.container(), MediaContainer::Mp3);
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let AnalysisOutcome::Partial { value, issue } = ingest.inspection else {
            panic!("expected partial MP4 inspection")
        };
        assert_eq!(
            issue,
            AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes)
        );
        let InspectionData::Media(media) = value.data() else {
            panic!("expected media facts")
        };
        assert_eq!(media.container(), MediaContainer::Mp4);
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
                },
            },
        );

        let [ExtractionOutcome::Xhtml(Ok(facts))] = ingest.extractions.as_slice() else {
            panic!("expected complete XHTML extraction");
        };
        assert!(
            facts
                .facts
                .text_stream()
                .spans()
                .any(|span| span.text() == "Hello")
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes),
                },
                fingerprint_budget: StreamBudget {
                    limit: None,
                    preflight: None,
                    exceeded: AnalysisIssue::Limit(AnalysisLimit::TotalFingerprintBytes),
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let analysis = epub.analyze();
        let overlay = analysis
            .resources()
            .declaration_by_id("overlay")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();

        assert!(analysis.coverage().relationships().iter().any(|coverage| {
            matches!(
                (coverage.source, coverage.completeness),
                (RelationshipSource::Smil(key), Completeness::Unavailable(_)) if key == overlay
            )
        }));
        assert!(!analysis.coverage().relationships().iter().any(|coverage| {
            matches!(coverage.source, RelationshipSource::Xhtml(key) if key == overlay)
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
        let epub =
            Epub::from_provider(provider, EpubPath::new("EPUB/package.opf").unwrap()).unwrap();
        let resources = epub.resources().clone();
        let overlay = resources
            .declaration_by_id("overlay")
            .unwrap()
            .resource()
            .unwrap()
            .ordinal();
        let issue = AnalysisIssue::Limit(AnalysisLimit::ResourceAnalysisBytes);
        let mut facts = resources
            .resources()
            .map(|resource| {
                let content = if resource.ordinal() == overlay {
                    AnalysisOutcome::Partial {
                        value: ContentFacts::from_smil(SmilFacts::new(
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
                crate::analysis::ResourceAnalysis::new(
                    resource.ordinal(),
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
                if coverage.source == RelationshipSource::Smil(overlay)
                    && matches!(&coverage.completeness, Completeness::Partial(actual) if *actual == issue)
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
