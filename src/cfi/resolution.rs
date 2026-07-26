use crate::resource::{
    EpubPath, ResourceAddress, ResourceLookupError, provider::ProviderReadError,
};

use super::{CfiPath, CfiXmlDecodeError, LocalPath};

/// A failure while resolving valid CFI syntax against a live publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CfiResolveError {
    /// The range common parent had no steps.
    #[error("CFI parent path is empty")]
    ParentPathEmpty,
    /// The package portion did not contain the required spine and itemref steps.
    #[error("CFI parent path is too short")]
    ParentPathTooShort,
    /// The package spine step was odd and therefore selected text rather than an element.
    #[error("CFI spine step must be even: {step}")]
    SpineStepOdd {
        /// Rejected step.
        step: usize,
    },
    /// The package step did not select the package `spine` element.
    #[error("CFI package path does not select the package spine: {step}")]
    SpineStepInvalid {
        /// Rejected step.
        step: usize,
    },
    /// An ID assertion did not match the package spine.
    #[error("CFI spine assertion does not match the package spine")]
    SpineAssertionMismatch,
    /// The spine itemref step was odd.
    #[error("CFI spine itemref step must be even: {step}")]
    SpineItemrefStepOdd {
        /// Rejected step.
        step: usize,
    },
    /// The spine itemref step did not identify an itemref.
    #[error("CFI spine itemref step is invalid")]
    InvalidSpineItemrefStep,
    /// An ID assertion did not match the selected itemref.
    #[error("CFI spine itemref assertion does not match the selected itemref")]
    SpineItemrefAssertionMismatch,
    /// A path selected no valid source location.
    #[error("CFI location is invalid")]
    InvalidLocation,
    /// A character offset was outside the selected text node.
    #[error("CFI offset range is invalid")]
    InvalidOffsetRange,
    /// Package-to-content indirection redirected directly to an offset.
    #[error("CFI package path cannot redirect to an offset")]
    PackageRedirectOffset,
    /// The package path lacked required `!` content-document indirection.
    #[error("CFI package path is missing content-document indirection")]
    PackageRedirectMissing,
    /// A redirected content path attempted another redirect.
    #[error("CFI redirected paths cannot include nested redirects")]
    RedirectedPathUnsupported,
    /// The common parent node did not exist.
    #[error("CFI parent node could not be resolved")]
    ParentNodeUnresolved,
    /// A range-local path contained unsupported indirection.
    #[error("CFI local paths cannot include redirects")]
    LocalPathRedirectUnsupported,
    /// A range-local path did not identify a node.
    #[error("CFI local path could not be resolved")]
    LocalPathUnresolved,
    /// The resolved range end preceded its start.
    #[error("CFI range end precedes its start")]
    ReversedRange,
    /// A range endpoint offset was attached to a non-text step.
    #[error("CFI range endpoint offset must reference a text node")]
    OffsetNotText,
    /// The selected text-node slot did not contain text.
    #[error("CFI text node is missing")]
    MissingTextNode,
    /// Source-text resolution encountered temporal or spatial offsets.
    #[error("CFI offset type is not supported by source-text resolution")]
    UnsupportedOffset,
    /// A text assertion did not match source text around the offset.
    #[error("CFI text assertion does not match the referenced text")]
    TextAssertionMismatch,
    /// Publication resource lookup failed.
    #[error("CFI resource lookup failed: {source}")]
    ResourceLookup {
        /// Underlying lookup error.
        #[source]
        source: ResourceLookupError,
    },
    /// The live provider could not read the selected content document.
    #[error("could not read CFI content document {path}: {source}")]
    ResourceRead {
        /// Content-document path.
        path: EpubPath,
        /// Underlying provider read error.
        #[source]
        source: ProviderReadError,
    },
    /// Content-document bytes could not be decoded as XML text.
    #[error("could not decode CFI content document XML at {path}: {source}")]
    XmlDecode {
        /// Content-document path.
        path: EpubPath,
        /// Underlying XML decoding error.
        #[source]
        source: CfiXmlDecodeError,
    },
    /// Decoded content was not parseable XML.
    #[error("CFI content document XML could not be parsed: {source}")]
    Xml {
        /// Underlying XML parser error.
        #[source]
        source: xot::Error,
    },
    /// A selected source range split an invalid UTF-16 sequence.
    #[error("CFI text could not be decoded from UTF-16: {source}")]
    Utf16 {
        /// Underlying UTF-16 conversion error.
        #[source]
        source: std::string::FromUtf16Error,
    },
}

impl From<xot::ParseError> for CfiResolveError {
    fn from(err: xot::ParseError) -> Self {
        Self::Xml { source: err.into() }
    }
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

    /// The absolute path through package and content-document indirection.
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
