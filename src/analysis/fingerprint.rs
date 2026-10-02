//! BLAKE3 resource fingerprints and duplicate-byte queries.
//!
//! A digest is available only when the complete resource was hashed, subject to
//! [`super::AnalysisLimits`].

use super::PublicationAnalysis;
use crate::resource::{ResourceIndex, ResourceOrdinal, ResourceRef};
use std::fmt;

/// A BLAKE3 digest of one resource's complete analyzed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blake3Hash([u8; 32]);

impl Blake3Hash {
    /// Wraps an already-computed 32-byte BLAKE3 digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Computes the BLAKE3 digest of `bytes`.
    pub fn hash(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Returns the digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Formats the digest as lowercase hexadecimal.
impl fmt::Display for Blake3Hash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(blake3::Hash::from_bytes(self.0).to_hex().as_str())
    }
}

/// Resources whose complete analyzed bytes have the same BLAKE3 digest.
#[derive(Debug, Clone, Copy)]
pub struct DuplicateGroup<'a> {
    hash: Blake3Hash,
    resources: &'a ResourceIndex,
    keys: &'a [ResourceOrdinal],
}

impl<'a> DuplicateGroup<'a> {
    /// Returns the fingerprint shared by every resource in the group.
    pub fn hash(self) -> Blake3Hash {
        self.hash
    }

    /// Iterates the matching resource records.
    pub fn resources(self) -> impl Iterator<Item = ResourceRef<'a>> + 'a {
        self.keys
            .iter()
            .filter_map(move |key| self.resources.resource(*key))
    }
}

impl PublicationAnalysis {
    /// Iterates resources with the requested complete BLAKE3 fingerprint.
    pub fn resources_with_fingerprint(
        &self,
        hash: Blake3Hash,
    ) -> impl Iterator<Item = ResourceRef<'_>> {
        self.fingerprint_index
            .get(&hash)
            .into_iter()
            .flatten()
            .filter_map(|key| self.resources.resource(*key))
    }

    /// Iterates groups of at least two byte-identical resources.
    pub fn duplicate_fingerprint_groups(&self) -> impl Iterator<Item = DuplicateGroup<'_>> {
        self.duplicate_fingerprints.iter().filter_map(|hash| {
            self.fingerprint_index.get(hash).map(|keys| DuplicateGroup {
                hash: *hash,
                resources: &self.resources,
                keys,
            })
        })
    }
}
