use crate::resource::{EpubPath, ResourceAddress, ResourceReadError};
use crate::xml::XmlDecodeError;

use super::{CfiPath, LocalPath};

/// A failure while resolving valid CFI syntax against a live publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CfiResolveError {
    /// The package portion of the path did not select a spine itemref and its content document.
    #[error("CFI package path could not be resolved: {0}")]
    PackagePath(
        /// Why the package portion could not be resolved.
        PackagePathFailure,
    ),
    /// The selected spine itemref does not resolve to a local resource.
    ///
    /// Inspect the occurrence through [`crate::ResourceIndex::occurrence`] for the reason.
    #[error("CFI spine item does not resolve to a local resource")]
    UnresolvedSpineItem,
    /// The content document could not be read, decoded, or parsed.
    #[error("could not load CFI content document {path}: {failure}")]
    ContentDocument {
        /// Content-document path.
        path: EpubPath,
        /// Why the document could not be loaded.
        #[source]
        failure: ContentDocumentFailure,
    },
    /// A path did not select a node in the content document.
    #[error("CFI content path could not be resolved: {0}")]
    ContentPath(
        /// Why the content path could not be resolved.
        ContentPathFailure,
    ),
    /// An offset could not be applied to the selected node.
    #[error("CFI offset could not be applied: {0}")]
    Offset(
        /// Why the offset could not be applied.
        OffsetFailure,
    ),
    /// An assertion did not match the resolved publication or content.
    #[error("CFI assertion does not match: {0}")]
    AssertionMismatch(
        /// Which assertion did not match.
        AssertionMismatch,
    ),
    /// The resolved range end preceded its start.
    #[error("CFI range end precedes its start")]
    ReversedRange,
}

/// Why the package portion of a CFI could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackagePathFailure {
    /// The path lacked the required spine and itemref steps.
    #[error("package path is too short")]
    TooShort,
    /// The first step did not select the package `spine` element.
    #[error("step {step} does not select the package spine")]
    SpineStep {
        /// Rejected step.
        step: usize,
    },
    /// The second step did not identify a spine itemref.
    #[error("step {step} does not identify a spine itemref")]
    ItemrefStep {
        /// Rejected step.
        step: usize,
    },
    /// The itemref position is outside the spine.
    #[error("spine has no itemref at position {index}")]
    MissingItemref {
        /// Zero-based itemref position.
        index: usize,
    },
    /// The package path lacked `!` indirection into a content document.
    #[error("package path is missing content-document indirection")]
    MissingContentIndirection,
}

/// Why a CFI content document could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ContentDocumentFailure {
    /// The live provider could not read the document.
    #[error("read failed")]
    Read(
        /// Underlying read error.
        #[source]
        ResourceReadError,
    ),
    /// Document bytes could not be decoded as XML text.
    #[error("XML decoding failed")]
    Decode(
        /// Underlying XML decoding error.
        #[source]
        XmlDecodeError,
    ),
    /// Decoded content was not parseable XML.
    #[error("XML parsing failed")]
    Parse(
        /// Underlying XML parser failure.
        #[source]
        Box<dyn std::error::Error + Send + Sync>,
    ),
}

/// Why a CFI content path did not select a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContentPathFailure {
    /// The path did not identify a node in the content document.
    #[error("path does not identify a node")]
    Unresolved,
    /// A content or range-local path attempted further `!` indirection.
    #[error("content paths cannot include redirects")]
    NestedRedirect,
}

/// Why a CFI offset could not be applied to the selected node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OffsetFailure {
    /// The offset was attached to a step that does not select text.
    #[error("offset must reference a text node")]
    NotText,
    /// The offset was outside the selected text.
    #[error("offset is outside the referenced text")]
    OutOfRange,
    /// Source-text resolution encountered a temporal or spatial offset.
    #[error("offset type is not supported by source-text resolution")]
    UnsupportedType,
    /// The selected boundary split a UTF-16 surrogate pair.
    #[error("offset splits a UTF-16 surrogate pair")]
    SplitSurrogate,
}

/// Which CFI assertion did not match the publication or its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AssertionMismatch {
    /// An ID assertion did not match the package spine.
    #[error("spine assertion")]
    Spine,
    /// An ID assertion did not match the selected itemref.
    #[error("spine itemref assertion")]
    SpineItemref,
    /// A text assertion did not match source text around the offset.
    #[error("text assertion")]
    Text,
}

/// A source-document location found by resolving a CFI against a publication.
///
/// The resolver reads and traverses the publication's current content document. Positions count
/// UTF-16 code units, matching EPUB CFI.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCfiLocation {
    document_path: CfiPath,
    relative_path: Option<LocalPath>,
    utf16_position: usize,
}

impl ResolvedCfiLocation {
    pub(crate) fn new(
        document_path: CfiPath,
        relative_path: Option<LocalPath>,
        utf16_position: usize,
    ) -> Self {
        Self {
            document_path,
            relative_path,
            utf16_position,
        }
    }

    /// The path inside the content document, following the package `!` indirection.
    ///
    /// The package portion of the original CFI is not included.
    pub fn document_path(&self) -> &CfiPath {
        &self.document_path
    }

    /// The relative endpoint path, if this location came from a range.
    pub fn relative_path(&self) -> Option<&LocalPath> {
        self.relative_path.as_ref()
    }

    /// The zero-based UTF-16 code-unit position in source text.
    pub fn utf16_position(&self) -> usize {
        self.utf16_position
    }
}

/// A point CFI resolved by live provider access and XML traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCfiPoint {
    source: ResourceAddress,
    location: ResolvedCfiLocation,
}

impl ResolvedCfiPoint {
    pub(crate) fn new(source: ResourceAddress, location: ResolvedCfiLocation) -> Self {
        Self { source, location }
    }

    /// The selected content-document resource.
    pub fn source(&self) -> &ResourceAddress {
        &self.source
    }

    /// The resolved source location.
    pub fn location(&self) -> &ResolvedCfiLocation {
        &self.location
    }
}

/// A range CFI resolved to ordered locations and source text by live publication access.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCfiRange {
    source: ResourceAddress,
    start: ResolvedCfiLocation,
    end: ResolvedCfiLocation,
    text: String,
}

impl ResolvedCfiRange {
    pub(crate) fn new(
        source: ResourceAddress,
        start: ResolvedCfiLocation,
        end: ResolvedCfiLocation,
        text: String,
    ) -> Self {
        Self {
            source,
            start,
            end,
            text,
        }
    }

    /// The selected content-document resource.
    pub fn source(&self) -> &ResourceAddress {
        &self.source
    }

    /// The inclusive resolved start location.
    pub fn start(&self) -> &ResolvedCfiLocation {
        &self.start
    }

    /// The exclusive resolved end location.
    pub fn end(&self) -> &ResolvedCfiLocation {
        &self.end
    }

    /// Source text selected between the resolved UTF-16 boundaries.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Successful live resolution of either a point or range CFI.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedCfi {
    /// A resolved point.
    Point(
        /// Point result.
        Box<ResolvedCfiPoint>,
    ),
    /// A resolved range.
    Range(
        /// Range result.
        Box<ResolvedCfiRange>,
    ),
}

impl ResolvedCfi {
    /// The selected content-document resource for either result kind.
    pub fn source(&self) -> &ResourceAddress {
        match self {
            Self::Point(point) => point.source(),
            Self::Range(range) => range.source(),
        }
    }
}
