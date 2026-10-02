//! Vocabulary tokens that keep authored spelling beside a recognized value.
//!
//! EPUB attributes such as manifest `properties`, spine `properties`, `meta` properties, and link
//! `rel` values are open vocabularies: a publication may use terms this crate does not recognize.
//! [`VocabToken`] stores the authored token and, when the term is recognized, its typed value.

use crate::string::{EpubString, EpubStringEmpty};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// One vocabulary token retaining its authored spelling and optional known projection.
///
/// Recognition follows the term type's [`FromStr`]: ASCII case-insensitive for EPUB package
/// vocabularies, case-sensitive for compact URL references such as
/// [`EpubStructuralSemantic`](crate::semantics::EpubStructuralSemantic). Unknown terms are
/// preserved exactly and reported by [`Self::as_str`] with no known value.
pub struct VocabToken<K> {
    raw: EpubString,
    known: Option<K>,
}

impl<K: Copy + FromStr> VocabToken<K> {
    /// Classifies authored text after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for empty or whitespace-only input.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(Self::classify)
    }

    fn classify(raw: EpubString) -> Self {
        let known = K::from_str(raw.as_str()).ok();
        Self { raw, known }
    }

    /// Returns the stored token.
    pub fn as_str(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the recognized term, if the authored spelling is one this crate knows.
    pub fn known_value(&self) -> Option<K> {
        self.known
    }
}

impl<K: Copy + FromStr + fmt::Display> From<K> for VocabToken<K> {
    fn from(value: K) -> Self {
        let raw = EpubString::new(value.to_string()).expect("known term is non-empty");
        Self {
            raw,
            known: Some(value),
        }
    }
}

impl<K: Copy + FromStr> From<EpubString> for VocabToken<K> {
    fn from(raw: EpubString) -> Self {
        Self::classify(raw)
    }
}

impl<K> fmt::Display for VocabToken<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.raw.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de, K: Copy + FromStr> serde::Deserialize<'de> for VocabToken<K> {
    /// Reclassifies the authored spelling rather than trusting the encoded known value.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Encoded {
            raw: EpubString,
        }

        let Encoded { raw } = Encoded::deserialize(deserializer)?;
        Ok(Self::classify(raw))
    }
}
