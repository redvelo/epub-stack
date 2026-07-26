/// A policy-free accessibility observation from XHTML or SVG source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessibilityFact {
    /// A nonempty image alternative.
    ImageAlt(AccessibilityValueFact),
    /// An image element without an `alt` attribute.
    MissingImageAlt(AccessibilityElementFact),
    /// An image with an authored empty `alt` value.
    EmptyImageAlt(AccessibilityElementFact),
    /// An authored `aria-label` value.
    AriaLabel(AccessibilityValueFact),
    /// An authored `aria-labelledby` value.
    AriaLabelledBy(AccessibilityValueFact),
    /// An authored `aria-describedby` value.
    AriaDescribedBy(AccessibilityValueFact),
    /// An authored ARIA `role` value.
    Role(AccessibilityValueFact),
    /// An authored `epub:type` value.
    EpubType(AccessibilityValueFact),
    /// An effective authored language value.
    Lang(AccessibilityValueFact),
    /// An effective authored direction value.
    Dir(AccessibilityValueFact),
    /// An HTML or ARIA heading level.
    HeadingLevel(AccessibilityHeadingLevelFact),
    /// A heading with no extracted text.
    EmptyHeading(AccessibilityElementFact),
    /// Text from an SVG `title` element.
    SvgTitle(SvgAccessibilityTextFact),
    /// Text from an SVG `desc` element.
    SvgDescription(SvgAccessibilityTextFact),
}

/// Text from an SVG `title` or `desc` element and the element it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgAccessibilityTextFact {
    pub(crate) subject_element: String,
    pub(crate) subject_fragment: Option<String>,
    pub(crate) source_fragment: Option<String>,
    pub(crate) value: String,
}

impl SvgAccessibilityTextFact {
    pub(crate) fn new(
        subject_element: String,
        subject_fragment: Option<String>,
        source_fragment: Option<String>,
        value: String,
    ) -> Self {
        Self {
            subject_element,
            subject_fragment,
            source_fragment,
            value,
        }
    }

    /// Returns the local name of the element described by this text.
    pub fn subject_element(&self) -> &str {
        &self.subject_element
    }

    /// Returns the described element's nearest authored fragment identifier.
    pub fn subject_fragment(&self) -> Option<&str> {
        self.subject_fragment.as_deref()
    }

    /// Returns the `title` or `desc` element's own fragment identifier.
    pub fn source_fragment(&self) -> Option<&str> {
        self.source_fragment.as_deref()
    }

    /// Returns the extracted text.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Element identity shared by accessibility observations without a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityElementFact {
    pub(crate) element: String,
    pub(crate) fragment: Option<String>,
}

impl AccessibilityElementFact {
    /// Returns the observed element's local name.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
}

/// Element identity and authored value for an accessibility observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityValueFact {
    pub(crate) element: String,
    pub(crate) fragment: Option<String>,
    pub(crate) value: String,
}

impl AccessibilityValueFact {
    /// Returns the observed element's local name.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Returns the normalized authored value or extracted text.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Element identity and numeric level for a heading observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityHeadingLevelFact {
    pub(crate) element: String,
    pub(crate) fragment: Option<String>,
    pub(crate) level: u8,
}

impl AccessibilityHeadingLevelFact {
    /// Returns the heading element's local name.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Returns the observed heading level.
    pub fn level(&self) -> u8 {
        self.level
    }
}

impl AccessibilityFact {
    /// Returns the observed element's local name.
    pub fn element(&self) -> &str {
        match self {
            Self::ImageAlt(fact)
            | Self::AriaLabel(fact)
            | Self::AriaLabelledBy(fact)
            | Self::AriaDescribedBy(fact)
            | Self::Role(fact)
            | Self::EpubType(fact)
            | Self::Lang(fact)
            | Self::Dir(fact) => fact.element.as_str(),
            Self::SvgTitle(fact) | Self::SvgDescription(fact) => fact.subject_element(),
            Self::MissingImageAlt(fact) | Self::EmptyImageAlt(fact) | Self::EmptyHeading(fact) => {
                fact.element.as_str()
            }
            Self::HeadingLevel(fact) => fact.element.as_str(),
        }
    }

    /// Returns the nearest authored fragment identifier when recorded.
    pub fn fragment(&self) -> Option<&str> {
        match self {
            Self::ImageAlt(fact)
            | Self::AriaLabel(fact)
            | Self::AriaLabelledBy(fact)
            | Self::AriaDescribedBy(fact)
            | Self::Role(fact)
            | Self::EpubType(fact)
            | Self::Lang(fact)
            | Self::Dir(fact) => fact.fragment.as_deref(),
            Self::SvgTitle(fact) | Self::SvgDescription(fact) => fact.subject_fragment(),
            Self::MissingImageAlt(fact) | Self::EmptyImageAlt(fact) | Self::EmptyHeading(fact) => {
                fact.fragment.as_deref()
            }
            Self::HeadingLevel(fact) => fact.fragment.as_deref(),
        }
    }

    /// Returns the level for heading-level observations.
    pub fn heading_level(&self) -> Option<u8> {
        match self {
            Self::HeadingLevel(fact) => Some(fact.level),
            _ => None,
        }
    }
}
