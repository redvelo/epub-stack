use crate::content::FragmentFact;

/// A policy-free accessibility observation from XHTML or SVG source.
///
/// Every observation identifies the element it was read from and that element's nearest authored
/// fragment, when one was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityFact {
    pub(crate) element: String,
    pub(crate) fragment: Option<FragmentFact>,
    pub(crate) observation: AccessibilityObservation,
}

impl AccessibilityFact {
    pub(crate) fn new(
        element: impl Into<String>,
        fragment: Option<FragmentFact>,
        observation: AccessibilityObservation,
    ) -> Self {
        Self {
            element: element.into(),
            fragment,
            observation,
        }
    }

    /// Returns the observed element's local name.
    ///
    /// For SVG `title` and `desc` text this is the element the text describes.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns the nearest authored fragment when recorded.
    pub fn fragment(&self) -> Option<&FragmentFact> {
        self.fragment.as_ref()
    }

    /// Returns what was observed on the element.
    pub fn observation(&self) -> &AccessibilityObservation {
        &self.observation
    }
}

/// What an accessibility observation recorded on its element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessibilityObservation {
    /// A nonempty image alternative.
    ImageAlt(
        /// Authored `alt` value.
        String,
    ),
    /// An image element without an `alt` attribute.
    MissingImageAlt,
    /// An image with an authored empty `alt` value.
    EmptyImageAlt,
    /// An authored `aria-label` value.
    AriaLabel(
        /// Authored value.
        String,
    ),
    /// An authored `aria-labelledby` value.
    AriaLabelledBy(
        /// Authored value.
        String,
    ),
    /// An authored `aria-describedby` value.
    AriaDescribedBy(
        /// Authored value.
        String,
    ),
    /// An authored ARIA `role` value.
    Role(
        /// Authored value.
        String,
    ),
    /// An authored `epub:type` value.
    EpubType(
        /// Authored value.
        String,
    ),
    /// An effective authored language value.
    Lang(
        /// Authored value.
        String,
    ),
    /// An effective authored direction value.
    Dir(
        /// Authored value.
        String,
    ),
    /// An HTML or ARIA heading level.
    HeadingLevel(
        /// Observed level.
        u8,
    ),
    /// A heading with no extracted text.
    EmptyHeading,
    /// Text from an SVG `title` element describing [`AccessibilityFact::element`].
    SvgTitle {
        /// The `title` element's own fragment.
        source_fragment: Option<FragmentFact>,
        /// Extracted text.
        value: String,
    },
    /// Text from an SVG `desc` element describing [`AccessibilityFact::element`].
    SvgDescription {
        /// The `desc` element's own fragment.
        source_fragment: Option<FragmentFact>,
        /// Extracted text.
        value: String,
    },
}

impl AccessibilityObservation {
    /// Returns the authored value or extracted text, when the observation carries one.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::ImageAlt(value)
            | Self::AriaLabel(value)
            | Self::AriaLabelledBy(value)
            | Self::AriaDescribedBy(value)
            | Self::Role(value)
            | Self::EpubType(value)
            | Self::Lang(value)
            | Self::Dir(value) => Some(value),
            Self::SvgTitle { value, .. } | Self::SvgDescription { value, .. } => Some(value),
            Self::MissingImageAlt
            | Self::EmptyImageAlt
            | Self::HeadingLevel(_)
            | Self::EmptyHeading => None,
        }
    }
}
