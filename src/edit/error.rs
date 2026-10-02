//! Handle failures while staging or previewing an edit transaction.

use crate::{
    XmlDecodeError,
    annotation::{AnnotationBundleError, AnnotationError, EmbeddedAnnotationsError},
    edit::select::SelectionTarget,
    navigation::{NavigationDepthError, parse::NavigationParseError},
    package::PackageError,
    resource::{
        EpubPath, ResourceReadError,
        provider::{ProviderIndexError, ProviderReadError},
    },
};

/// Identifies a structural document owned by a loaded semantic model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralResourceKind {
    /// The loaded package document.
    Package,
    /// The selected EPUB navigation document.
    Navigation,
    /// The selected NCX fallback document.
    Ncx,
}

/// Why rewritten structural XML could not be verified against the staged model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralVerificationFailure {
    /// A node selected through the model has no counterpart in the source XML.
    SourceNodeMissing,
    /// The reparsed source does not match the staged model.
    ModelMismatch,
}

/// Why an OPF 2 guide href could not enter the EPUB 3 navigation model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuideHrefFailure {
    /// The `reference` element had no `href` attribute.
    #[error("href is missing")]
    Missing,
    /// The authored href was empty or contained only whitespace.
    #[error("href is blank: {href:?}")]
    Blank {
        /// The authored href, retained exactly.
        href: String,
    },
    /// The authored href was not valid href syntax.
    #[error("href has invalid syntax: {href:?}")]
    InvalidSyntax {
        /// The authored href, retained exactly.
        href: String,
    },
}

/// Reports why an edit operation or preview failed.
///
/// Missing or ambiguous targets are reported through [`Self::Selection`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// The staged resource index cannot be represented by public ordinals.
    #[error("staged resource index cannot be represented: {source}")]
    ResourceIndex {
        /// The resource-index overflow.
        #[from]
        source: crate::resource::ResourceIndexError,
    },
    /// Embedded annotation JSON, resources, or bundle limits were invalid.
    #[error("embedded annotation edit failed: {source}")]
    Annotations {
        /// The annotation failure.
        #[source]
        source: EmbeddedAnnotationsError,
    },
    /// A navigation manifest item did not resolve to a usable local path.
    #[error("Invalid navigation manifest href for item {id}: {href}")]
    InvalidNavigationHref {
        /// The manifest item ID carrying the href.
        id: String,
        /// The unusable authored href.
        href: String,
    },
    /// An OPF 2 guide href could not enter the EPUB 3 navigation model.
    #[error("Invalid OPF2 guide href: {failure}")]
    InvalidGuideHref {
        /// Why the authored href could not be converted.
        failure: GuideHrefFailure,
    },
    /// A navigation href could not be rebased into the generated navigation document.
    #[error("Navigation href {href:?} cannot be rebased into the generated navigation document")]
    NavigationHrefRebase {
        /// The authored navigation href.
        href: String,
    },
    /// A raw or generic operation targeted a structural document owned by a semantic model.
    #[error("Edit targets structural resource {path} ({kind:?})")]
    StructuralResourceEdit {
        /// The protected structural resource path.
        path: EpubPath,
        /// The loaded model that owns the resource.
        kind: StructuralResourceKind,
    },
    /// A raw resource edit targeted a path with no staged bytes.
    #[error("Resource is missing from the staged publication: {path}")]
    MissingResource {
        /// The path without staged bytes.
        path: EpubPath,
    },
    /// Rewritten structural XML did not verify against the staged model.
    #[error("Structural XML verification failed for {path}: {failure:?}")]
    StructuralVerification {
        /// The structural document being edited.
        path: EpubPath,
        /// The failed check.
        failure: StructuralVerificationFailure,
    },
    /// Structural XML bytes could not be decoded.
    #[error("Could not decode structural XML at {path}: {source}")]
    StructuralXmlDecode {
        /// The structural document that could not be decoded.
        path: EpubPath,
        /// The decoding failure.
        #[source]
        source: XmlDecodeError,
    },
    /// Structural XML could not be parsed or serialized.
    #[error("Structural XML processing failed for {path}: {source}")]
    StructuralXml {
        /// The structural document being processed.
        path: EpubPath,
        /// The XML engine failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A targeted package element used an unsupported namespace.
    #[error(
        "Unsupported package XML namespace in {path}: element {element} uses namespace {namespace}, expected {expected}"
    )]
    UnsupportedPackageNamespace {
        /// The package document path.
        path: EpubPath,
        /// The local name of the targeted element.
        element: String,
        /// The namespace found on the element.
        namespace: String,
        /// The namespace required for the operation.
        expected: &'static str,
    },
    /// A generic manifest operation targeted a navigation document item.
    #[error("Manifest item {id:?} is a navigation document and requires a navigation edit")]
    NavigationManifestItem {
        /// The navigation manifest item ID, when present.
        id: Option<String>,
    },
    /// A navigation edit requires an EPUB navigation document.
    #[error("Navigation edits require an EPUB navigation document")]
    MissingEpubNavigation,
    /// A navigation point cannot move into itself or its descendant.
    #[error("A navigation point cannot move into itself or its descendant")]
    NavigationPointCycle,
    /// A navigation point label contains inline markup that a text update would discard.
    #[error("Navigation point label contains inline markup")]
    NavigationLabelMarkup,
    /// A navigation point carries NCX class semantics, which EPUB NAV cannot represent.
    #[error("NCX class semantics cannot be inserted into an EPUB navigation document")]
    NcxSemanticInNavigation,
    /// A navigation point with semantics has no label element to carry them.
    #[error("Navigation point semantics require an anchor or span label")]
    NavigationSemanticsWithoutLabel,
    /// A resource removal would break another manifest item sharing the same path.
    #[error("Resource {path} is referenced by another manifest item")]
    SharedResourcePath {
        /// The shared resource path.
        path: EpubPath,
    },
    /// A spine resource removal targets a manifest item referenced by several itemrefs.
    #[error("Manifest item {idref} is referenced by multiple spine itemrefs")]
    SharedSpineItem {
        /// The shared manifest item ID.
        idref: String,
    },
    /// A manifest item href does not resolve to the resource path it is staged with.
    #[error("Manifest item href resolves to {actual:?}, not {expected}")]
    ManifestHrefMismatch {
        /// The staged resource path.
        expected: EpubPath,
        /// The local path the href resolves to, or `None` when it is not local.
        actual: Option<EpubPath>,
    },
    /// A manifest item selected for resource removal does not target a local resource.
    #[error("Manifest item {id:?} does not target a local resource")]
    NonLocalManifestItem {
        /// The manifest item ID, when present.
        id: Option<String>,
    },
    /// A spine itemref does not target the manifest item staged with it.
    #[error("Spine itemref {idref} does not target manifest item {id}")]
    ItemRefTargetMismatch {
        /// The itemref `idref`.
        idref: String,
        /// The manifest item ID.
        id: String,
    },
    /// OPF 2 migration requires an OPF 2.0 package.
    #[error("Migration requires an OPF 2.0 package")]
    NotOpf2,
    /// OPF 2 migration requires EPUB NAV or NCX navigation to convert.
    #[error("Migration requires existing EPUB NAV or NCX navigation")]
    MissingSourceNavigation,
    /// OPF 2 cover metadata is empty or references a missing manifest item.
    #[error("OPF2 cover metadata references missing manifest item {id:?}")]
    InvalidOpf2Cover {
        /// The referenced manifest ID, or `None` when the metadata has no content.
        id: Option<String>,
    },
    /// An embedded annotation resource would overwrite an unrelated existing resource.
    #[error("Embedded annotation resource already exists: {path}")]
    AnnotationResourceExists {
        /// The annotation-relative resource path.
        path: String,
    },
    /// A selector did not identify exactly one staged value.
    #[error("Edit selection failed for {target:?}: {failure}")]
    Selection {
        /// The selector that failed.
        target: SelectionTarget,
        /// Why exactly one value could not be selected.
        failure: SelectionFailure,
    },
    /// Current resource bytes could not be read from the provider or session overlay.
    #[error("Resource read failed during editing: {source}")]
    ResourceRead {
        /// The underlying read failure.
        #[source]
        source: ResourceReadError,
    },
    /// The staged resource index could not be built under the opening limits.
    #[error("Provider index construction failed during editing: {source}")]
    ProviderIndex {
        /// The underlying provider-index failure.
        #[source]
        source: ProviderIndexError,
    },
    /// The package model rejected the requested state.
    #[error("Package operation failed during editing: {source}")]
    Package {
        /// The package-model failure.
        #[source]
        source: PackageError,
    },
    /// Edited navigation source could not be reparsed.
    #[error("Navigation parsing failed during editing: {source}")]
    NavigationParse {
        /// The navigation parser failure.
        #[source]
        source: NavigationParseError,
    },
    /// Navigation construction exceeded its supported nesting depth.
    #[error("Navigation depth is unsupported during editing: {source}")]
    NavigationDepth {
        /// The navigation depth-limit failure.
        #[source]
        source: NavigationDepthError,
    },
    /// Raw or coordinated edits attempted to alter the export-owned OCF `mimetype` entry.
    #[error("The OCF mimetype entry is generated during export and cannot be edited")]
    MimetypeResourceEdit,
}

/// Why a selector failed to identify exactly one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SelectionFailure {
    /// No value matched.
    #[error("not found")]
    NotFound,
    /// More than one value matched where uniqueness was required.
    #[error("ambiguous")]
    Ambiguous,
    /// The selected value lacks identity required by the operation.
    #[error("selected value has no required identity")]
    MissingIdentity,
}

impl EditError {
    pub(crate) fn selection(target: impl Into<SelectionTarget>, failure: SelectionFailure) -> Self {
        Self::Selection {
            target: target.into(),
            failure,
        }
    }

    pub(crate) fn source_node_missing(path: &EpubPath) -> Self {
        Self::StructuralVerification {
            path: path.clone(),
            failure: StructuralVerificationFailure::SourceNodeMissing,
        }
    }

    pub(crate) fn model_mismatch(path: &EpubPath) -> Self {
        Self::StructuralVerification {
            path: path.clone(),
            failure: StructuralVerificationFailure::ModelMismatch,
        }
    }

    pub(crate) fn structural_xml(
        path: &EpubPath,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::StructuralXml {
            path: path.clone(),
            source: Box::new(source),
        }
    }
}

macro_rules! edit_error_from {
    ($source:ty, $variant:ident) => {
        impl From<$source> for EditError {
            fn from(source: $source) -> Self {
                Self::$variant { source }
            }
        }
    };
}

edit_error_from!(ResourceReadError, ResourceRead);
edit_error_from!(ProviderIndexError, ProviderIndex);
edit_error_from!(PackageError, Package);
edit_error_from!(NavigationParseError, NavigationParse);
edit_error_from!(NavigationDepthError, NavigationDepth);
edit_error_from!(EmbeddedAnnotationsError, Annotations);

impl From<ProviderReadError> for EditError {
    fn from(source: ProviderReadError) -> Self {
        Self::ResourceRead {
            source: source.into(),
        }
    }
}

impl From<AnnotationError> for EditError {
    fn from(source: AnnotationError) -> Self {
        EmbeddedAnnotationsError::from(source).into()
    }
}

impl From<AnnotationBundleError> for EditError {
    fn from(source: AnnotationBundleError) -> Self {
        EmbeddedAnnotationsError::from(source).into()
    }
}

macro_rules! selection_target_from {
    ($source:ident, $variant:ident) => {
        impl From<crate::edit::select::$source> for SelectionTarget {
            fn from(value: crate::edit::select::$source) -> Self {
                Self::$variant(value)
            }
        }
    };
}

selection_target_from!(ManifestItemSelector, ManifestItem);
selection_target_from!(SpineItemRefSelector, SpineItemRef);
selection_target_from!(MetadataElementSelector, MetadataElement);
selection_target_from!(MetaSelector, Meta);
selection_target_from!(MetadataLinkSelector, MetadataLink);
selection_target_from!(ListSelector, NavigationList);
selection_target_from!(PointSelector, NavigationPoint);
