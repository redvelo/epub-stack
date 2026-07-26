//! Handle failures while staging or previewing an edit transaction.

use crate::{
    annotation::{AnnotationBundleError, AnnotationError, EmbeddedAnnotationsError},
    navigation::{
        NavigationDepthError, NavigationGenerateError, NavigationXhtmlError,
        parse::NavigationParseError,
    },
    package::PackageError,
    resource::{
        EpubHrefError, EpubPath, EpubPathError, ResourceLookupError,
        provider::{ProviderReadError, ResourceProviderIndexError},
    },
    xml::XmlDecodeError,
};
use std::{error::Error, fmt};

/// Identifies a modeled resource that raw byte edits cannot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralResourceKind {
    /// The loaded package document.
    Package,
    /// The selected EPUB navigation document.
    Navigation,
    /// The selected NCX fallback document.
    Ncx,
}

/// Reports why an edit operation or preview failed.
///
/// Semantic missing or ambiguous states needed by an operation use [`Self::Selection`].
/// The enum is non-exhaustive so applications should retain a fallback match arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// Touched embedded annotations could not be loaded into a bundle.
    #[error("embedded annotations could not be loaded: {source}")]
    EmbeddedAnnotations {
        /// The embedded-annotation loading failure.
        #[source]
        source: EmbeddedAnnotationsError,
    },
    /// A caller-supplied provider path was not canonical EPUB path syntax.
    #[error("Invalid resource path {path}: {source}")]
    InvalidResourcePath {
        /// The noncanonical provider path supplied by the caller.
        path: std::path::PathBuf,
        /// Why the path cannot be represented as an EPUB path.
        source: EpubPathError,
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
    #[error("Invalid OPF2 guide href {href:?}: {reason}")]
    InvalidGuideHref {
        /// The authored guide href, when present.
        href: Option<String>,
        /// A stable description of why conversion was impossible.
        reason: &'static str,
    },
    /// A raw byte operation targeted a loaded structural model source.
    #[error("Raw resource edit targets loaded structural resource {path} ({kind:?})")]
    StructuralResourceEdit {
        /// The protected structural resource path.
        path: EpubPath,
        /// The loaded model that owns the resource.
        kind: StructuralResourceKind,
    },
    /// A resource selector resolved to a non-local address.
    #[error("Resource selector does not resolve to a local provider resource: {selector}")]
    NonLocalResource {
        /// A display form of the selector that resolved non-locally.
        selector: String,
    },
    /// Edited structural XML did not match the requested semantic change.
    #[error("Structural XML edit failed for {path}: {message}")]
    StructuralXml {
        /// The structural document being edited.
        path: EpubPath,
        /// Description of the failed structural check.
        message: String,
    },
    /// Structural XML bytes could not be decoded.
    #[error("Could not decode structural XML at {path}: {source}")]
    StructuralXmlDecode {
        /// The structural document that could not be decoded.
        path: EpubPath,
        /// The decoding failure.
        #[source]
        source: StructuralXmlDecodeError,
    },
    /// Structural XML could not be parsed, changed, or serialized.
    #[error("Structural XML operation failed for {path}: {source}")]
    StructuralXmlOperation {
        /// The structural document being processed.
        path: EpubPath,
        /// The underlying XML-engine failure.
        #[source]
        source: StructuralXmlOperationError,
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
    /// The requested semantic operation is intentionally unsupported for this state.
    #[error("Unsupported semantic edit: {message}")]
    UnsupportedSemanticEdit {
        /// A focused description of the unsupported state or operation.
        message: String,
    },
    /// A semantic selector did not resolve exactly one required value.
    #[error("Edit selection failed for {target} using {selector}: {failure}")]
    Selection {
        /// The kind of semantic value being selected.
        target: &'static str,
        /// A display form of the selector.
        selector: String,
        /// Why exactly one value could not be selected.
        failure: SelectionFailure,
    },
    /// Resource lookup failed before a local edit target could be selected.
    #[error("Resource lookup failed during editing: {source}")]
    ResourceLookup {
        /// The underlying resource-selection failure.
        #[source]
        source: ResourceLookupError,
    },
    /// Current resource bytes could not be read from the provider or session overlay.
    #[error("Provider read failed during editing: {source}")]
    ProviderRead {
        /// The underlying provider read failure.
        #[source]
        source: ProviderReadError,
    },
    /// The current resource index could not be built under the opening limits.
    #[error("Provider index construction failed during editing: {source}")]
    ProviderIndex {
        /// The underlying provider-index failure.
        #[source]
        source: ResourceProviderIndexError,
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
    /// Public normalized navigation XHTML generation failed.
    #[error("Navigation XHTML generation failed during editing: {source}")]
    NavigationXhtml {
        /// The public normalized-XHTML generation failure.
        #[source]
        source: NavigationXhtmlError,
    },
    /// Coordinated navigation generation failed.
    #[error("Navigation generation failed during editing: {source}")]
    NavigationGeneration {
        /// The navigation-generation failure.
        #[source]
        source: NavigationGenerationError,
    },
    /// Annotation JSON or model processing failed.
    #[error("Annotation operation failed during editing: {source}")]
    Annotation {
        /// The annotation processing failure.
        #[source]
        source: AnnotationError,
    },
    /// Embedded annotation resource closure could not be represented.
    #[error("Annotation bundle operation failed during editing: {source}")]
    AnnotationBundle {
        /// The annotation bundle or closure failure.
        #[source]
        source: AnnotationBundleError,
    },
    /// Authored href syntax required by an edit was invalid.
    #[error("Invalid EPUB href during editing: {source}")]
    Href {
        /// The invalid-href failure.
        #[source]
        source: EpubHrefError,
    },
    /// Raw or coordinated edits attempted to alter the export-owned OCF `mimetype` entry.
    #[error("The OCF mimetype entry is generated during export and cannot be edited")]
    MimetypeResourceEdit,
}

/// A structural XML byte-decoding failure encountered by an edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StructuralXmlDecodeError {
    /// The declaration names an encoding unsupported by the structural editor.
    #[error("unsupported XML encoding declaration: {encoding}")]
    UnsupportedEncoding {
        /// The declared encoding label.
        encoding: String,
    },
    /// The XML declaration conflicts with byte-order evidence.
    #[error("XML encoding declaration {declared} conflicts with detected {detected} encoding")]
    ConflictingEncoding {
        /// The encoding label in the XML declaration.
        declared: String,
        /// The encoding inferred from the input bytes.
        detected: &'static str,
    },
    /// The input contains an invalid sequence for its detected encoding.
    #[error("invalid byte sequence for XML encoding {encoding}")]
    InvalidBytes {
        /// The encoding used to decode the bytes.
        encoding: &'static str,
    },
}

impl From<XmlDecodeError> for StructuralXmlDecodeError {
    fn from(source: XmlDecodeError) -> Self {
        match source {
            XmlDecodeError::UnsupportedEncoding { encoding } => {
                Self::UnsupportedEncoding { encoding }
            }
            XmlDecodeError::ConflictingEncoding { declared, detected } => {
                Self::ConflictingEncoding { declared, detected }
            }
            XmlDecodeError::InvalidBytes { encoding } => Self::InvalidBytes { encoding },
        }
    }
}

/// Wraps the underlying structural XML operation failure.
#[derive(Debug)]
pub struct StructuralXmlOperationError {
    source: Box<dyn Error + Send + Sync + 'static>,
}

impl StructuralXmlOperationError {
    pub(crate) fn new(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl fmt::Display for StructuralXmlOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for StructuralXmlOperationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Wraps the underlying coordinated navigation-generation failure.
#[derive(Debug)]
pub struct NavigationGenerationError {
    source: Box<dyn Error + Send + Sync + 'static>,
}

impl NavigationGenerationError {
    fn new(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl fmt::Display for NavigationGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl Error for NavigationGenerationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Why a semantic selector failed to identify exactly one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SelectionFailure {
    /// No value matched.
    #[error("not found")]
    NotFound,
    /// More than one value matched where uniqueness was required.
    #[error("ambiguous")]
    Ambiguous,
    /// The selected source value lacked identity required by the operation.
    #[error("selected value has no required identity")]
    MissingIdentity,
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

edit_error_from!(ResourceLookupError, ResourceLookup);
edit_error_from!(ProviderReadError, ProviderRead);
edit_error_from!(ResourceProviderIndexError, ProviderIndex);
edit_error_from!(PackageError, Package);
edit_error_from!(NavigationParseError, NavigationParse);
edit_error_from!(NavigationDepthError, NavigationDepth);
edit_error_from!(NavigationXhtmlError, NavigationXhtml);
edit_error_from!(AnnotationError, Annotation);
edit_error_from!(AnnotationBundleError, AnnotationBundle);
edit_error_from!(EpubHrefError, Href);

impl From<NavigationGenerateError> for EditError {
    fn from(source: NavigationGenerateError) -> Self {
        Self::NavigationGeneration {
            source: NavigationGenerationError::new(source),
        }
    }
}
