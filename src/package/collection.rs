//! Build and inspect nested EPUB package collections.
//!
//! A [`Collection`] groups metadata links, optional metadata, and child collections. Its role may
//! be a recognized [`CollectionRole`], absent, or
//! unknown in parsed source; unknown roles keep their authored spelling.

use super::Result;
use super::metadata::{Metadata, MetadataLink};
use crate::semantics::TextDirection;
use crate::string::EpubString;

/// Maximum number of collection elements along any nested collection branch.
///
/// Parsing, insertion, and normalized generation enforce this bound.
pub const MAX_COLLECTION_NESTING_DEPTH: usize = 128;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned OPF `collection` subtree.
///
/// Children, links, and metadata retain modeled source order. Unknown XML and invalid typed
/// attribute spellings are not preserved by this semantic model.
pub struct Collection {
    dir: Option<TextDirection>,
    id: Option<EpubString>,
    xml_lang: Option<EpubString>,
    role: Option<CollectionRoleToken>,
    link: Vec<MetadataLink>,
    metadata: Option<Metadata>,
    collections: Vec<Collection>,
}

impl Collection {
    pub(super) fn from_parsed(
        dir: Option<TextDirection>,
        id: Option<EpubString>,
        xml_lang: Option<EpubString>,
        role: Option<CollectionRoleToken>,
    ) -> Self {
        Self {
            dir,
            id,
            xml_lang,
            role,
            link: Vec::new(),
            metadata: None,
            collections: Vec::new(),
        }
    }

    pub(super) fn append_metadata(&mut self, metadata: Metadata) {
        self.metadata
            .get_or_insert_with(Metadata::empty)
            .append(metadata);
    }

    /// Creates an empty collection with the supplied canonical role.
    ///
    /// Collections are currently read-only in publications: this crate parses and regenerates
    /// them, but no publication edit installs a constructed [`Collection`].
    pub fn new(role: CollectionRole) -> Self {
        Self {
            dir: None,
            id: None,
            xml_lang: None,
            role: Some(role.into()),
            link: Vec::new(),
            metadata: None,
            collections: Vec::new(),
        }
    }

    /// Appends an owned metadata link in collection order.
    pub(crate) fn add_link(&mut self, link: MetadataLink) {
        self.link.push(link);
    }

    /// Appends an owned nested collection.
    ///
    /// # Errors
    ///
    /// Returns [`super::PackageError::CollectionNestingLimitExceeded`] if insertion would
    /// exceed [`MAX_COLLECTION_NESTING_DEPTH`]. Failure leaves `self` unchanged.
    pub(crate) fn add_collection(&mut self, collection: Collection) -> Result<()> {
        if collection.max_nesting_depth() >= MAX_COLLECTION_NESTING_DEPTH {
            return Err(super::PackageError::CollectionNestingLimitExceeded {
                limit: MAX_COLLECTION_NESTING_DEPTH,
            });
        }
        self.collections.push(collection);
        Ok(())
    }

    /// Borrows the optional collection ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Returns the recognized collection text direction.
    pub fn dir(&self) -> Option<TextDirection> {
        self.dir
    }
    /// Borrows `xml:lang` without language-tag normalization.
    pub fn xml_lang(&self) -> Option<&EpubString> {
        self.xml_lang.as_ref()
    }
    /// Borrows the authored collection role, recognized or not.
    pub fn role(&self) -> Option<&CollectionRoleToken> {
        self.role.as_ref()
    }
    /// Borrows metadata links in modeled source order.
    pub fn link(&self) -> &[MetadataLink] {
        &self.link
    }
    /// Borrows the collection's optional owned metadata block.
    pub fn metadata(&self) -> Option<&Metadata> {
        self.metadata.as_ref()
    }
    /// Borrows nested collections in modeled source order.
    pub fn collections(&self) -> &[Collection] {
        self.collections.as_slice()
    }

    fn max_nesting_depth(&self) -> usize {
        let mut maximum = 0;
        let mut pending = vec![(self, 1)];
        while let Some((collection, depth)) = pending.pop() {
            maximum = maximum.max(depth);
            pending.extend(
                collection
                    .collections
                    .iter()
                    .map(|nested| (nested, depth + 1)),
            );
        }
        maximum
    }
}

/// A collection `role` token retaining its authored spelling and optional known projection.
///
/// Roles are an open vocabulary: a publication may use an absolute URL or a term this crate does
/// not recognize, and that spelling is preserved through parsing and generation.
pub type CollectionRoleToken = crate::vocab::VocabToken<CollectionRole>;

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// A recognized EPUB collection role.
pub enum CollectionRole {
    /// A dictionary collection.
    Dictionary,
    /// An index collection.
    Index,
    /// A grouping of index collections.
    #[cfg_attr(feature = "serde", serde(rename = "index-group"))]
    IndexGroup,
    /// A distributable-object collection.
    #[cfg_attr(feature = "serde", serde(rename = "distributable-object"))]
    DistributableObject,
    /// A resource manifest collection.
    Manifest,
    /// A publication preview collection.
    Preview,
    /// A scriptable-content collection.
    #[cfg_attr(feature = "serde", serde(rename = "scriptable-content"))]
    ScriptableContent,
    /// A collection of learning units.
    Units,
    /// A page-list set.
    #[cfg_attr(feature = "serde", serde(rename = "page-set"))]
    PageSet,
    /// A single page collection.
    Page,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;

    #[test]
    fn repeated_collections_preserve_order_at_every_level() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf">
                <collection id="first"><collection id="a"/><collection id="b"/></collection>
                <collection id="second"/>
            </package>"#,
        )
        .unwrap();

        assert_eq!(package.collections().len(), 2);
        assert_eq!(
            package.collections()[0].id(),
            EpubString::new("first").as_ref()
        );
        assert_eq!(
            package.collections()[1].id(),
            EpubString::new("second").as_ref()
        );
        assert_eq!(package.collections()[0].collections().len(), 2);
        assert_eq!(
            package.collections()[0].collections()[0].id(),
            EpubString::new("a").as_ref()
        );
        assert_eq!(
            package.collections()[0].collections()[1].id(),
            EpubString::new("b").as_ref()
        );
    }

    #[test]
    fn collection_construction_enforces_the_nesting_limit() {
        let mut collection = Collection::new(CollectionRole::Index);
        for _ in 1..MAX_COLLECTION_NESTING_DEPTH {
            let mut parent = Collection::new(CollectionRole::Index);
            parent.add_collection(collection).unwrap();
            collection = parent;
        }

        let mut parent = Collection::new(CollectionRole::Index);
        assert!(matches!(
            parent.add_collection(collection),
            Err(super::super::PackageError::CollectionNestingLimitExceeded { limit })
                if limit == MAX_COLLECTION_NESTING_DEPTH
        ));
    }
    #[test]
    fn hyphenated_and_unknown_roles_survive_parse_and_generation() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf">
                <collection role="index-group"/>
                <collection role="https://example.com/vocab#custom"/>
            </package>"#,
        )
        .unwrap();

        let roles = package.collections();
        assert_eq!(
            roles[0].role().and_then(CollectionRoleToken::known_value),
            Some(CollectionRole::IndexGroup)
        );
        assert_eq!(
            roles[0].role().map(CollectionRoleToken::as_str),
            Some("index-group")
        );
        assert_eq!(
            roles[1].role().and_then(CollectionRoleToken::known_value),
            None
        );
        assert_eq!(
            roles[1].role().map(CollectionRoleToken::as_str),
            Some("https://example.com/vocab#custom")
        );

        let xml = package.to_normalized_xml().unwrap();
        assert!(xml.contains(r#"role="index-group""#), "{xml}");
        assert!(
            xml.contains(r#"role="https://example.com/vocab#custom""#),
            "{xml}"
        );
    }
}
