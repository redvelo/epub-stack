use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

/// Error returned when an [`EpubString`] would be empty after trimming.
#[derive(Debug, PartialEq, Eq, Clone, Copy, thiserror::Error)]
#[error("EPUB string is empty")]
pub struct EpubStringEmpty;

/// Non-empty EPUB text with surrounding whitespace removed.
///
/// Constructors use [`str::trim`]; interior whitespace, case, and Unicode representation are unchanged.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
#[cfg_attr(feature = "specta", derive(specta::Type), specta(transparent))]
pub struct EpubString(String);

impl EpubString {
    /// Trims `value` and returns it unless the result is empty.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let value = value.as_ref().trim();
        (!value.is_empty()).then(|| Self(value.to_string()))
    }

    /// Trims `value`, returning [`EpubStringEmpty`] if the result is empty.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, EpubStringEmpty> {
        Self::new(value).ok_or(EpubStringEmpty)
    }

    /// The stored, trimmed text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for EpubString {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Deref for EpubString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Borrow<str> for EpubString {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for EpubString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<EpubString> for String {
    fn from(value: EpubString) -> Self {
        value.0
    }
}

impl From<&EpubString> for String {
    fn from(value: &EpubString) -> Self {
        value.as_str().to_string()
    }
}

impl PartialEq<str> for EpubString {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for EpubString {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<EpubString> for str {
    fn eq(&self, other: &EpubString) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<EpubString> for &str {
    fn eq(&self, other: &EpubString) -> bool {
        *self == other.as_str()
    }
}

impl TryFrom<String> for EpubString {
    type Error = EpubStringEmpty;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for EpubString {
    type Error = EpubStringEmpty;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl FromStr for EpubString {
    type Err = EpubStringEmpty;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

pub(crate) fn optional_epub_string(value: Option<String>) -> Option<EpubString> {
    value.and_then(EpubString::new)
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EpubString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| serde::de::Error::custom("EPUB string is empty"))
    }
}
