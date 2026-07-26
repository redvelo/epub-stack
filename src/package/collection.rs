//! Build and inspect nested EPUB package collections.
//!
//! A [`Collection`] groups metadata links, optional metadata, and child collections. Its role may
//! be a recognized [`CollectionRole`](crate::package::collection::CollectionRole), absent, or
//! unknown in parsed source. Use [`Collection::add_collection`] to enforce the same nesting limit
//! as package parsing and normalized generation.

use super::metadata::{Metadata, MetadataLink};
use super::{Result, required_package_string};
use crate::semantics::TextDirection;
use crate::string::EpubString;

/// Maximum number of collection elements along any nested collection branch.
///
/// Parsing, insertion, and normalized generation enforce this bound.
pub const MAX_COLLECTION_NESTING_DEPTH: usize = 128;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
/// An owned OPF `collection` subtree.
///
/// Children, links, and metadata retain modeled source order. Unknown XML and invalid typed
/// attribute spellings are not preserved by this semantic model.
pub struct Collection {
    dir: Option<TextDirection>,
    id: Option<EpubString>,
    xml_lang: Option<EpubString>,
    role: Option<CollectionRole>,
    link: Vec<MetadataLink>,
    metadata: Option<Metadata>,
    collections: Vec<Collection>,
}

impl Collection {
    pub(super) fn from_parsed(
        dir: Option<TextDirection>,
        id: Option<EpubString>,
        xml_lang: Option<EpubString>,
        role: Option<CollectionRole>,
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
    pub fn new(role: CollectionRole) -> Self {
        Self {
            dir: None,
            id: None,
            xml_lang: None,
            role: Some(role),
            link: Vec::new(),
            metadata: None,
            collections: Vec::new(),
        }
    }

    /// Sets the collection ID without changing other fields.
    ///
    /// # Errors
    ///
    /// Returns [`super::PackageError::EmptyField`] for an empty or whitespace-only ID.
    pub fn with_id(mut self, id: impl AsRef<str>) -> Result<Self> {
        self.id = Some(required_package_string(id, "collection id")?);
        Ok(self)
    }

    /// Sets the collection's text direction.
    pub fn with_dir(mut self, dir: TextDirection) -> Self {
        self.dir = Some(dir);
        self
    }

    /// Sets `xml:lang` without language-tag normalization.
    ///
    /// # Errors
    ///
    /// Returns [`super::PackageError::EmptyField`] for an empty or whitespace-only value.
    pub fn with_xml_lang(mut self, xml_lang: impl AsRef<str>) -> Result<Self> {
        self.xml_lang = Some(required_package_string(xml_lang, "collection xml:lang")?);
        Ok(self)
    }

    /// Replaces the collection role with a canonical known value.
    pub fn with_role(mut self, role: CollectionRole) -> Self {
        self.role = Some(role);
        self
    }

    /// Appends an owned metadata link in collection order.
    pub fn add_link(&mut self, link: MetadataLink) {
        self.link.push(link);
    }

    /// Replaces the collection's optional owned metadata block.
    ///
    /// This does not preserve the original block's XML layout.
    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = Some(metadata);
    }

    /// Appends an owned nested collection.
    ///
    /// # Errors
    ///
    /// Returns [`super::PackageError::CollectionNestingLimitExceeded`] if insertion would
    /// exceed [`MAX_COLLECTION_NESTING_DEPTH`]. Failure leaves `self` unchanged.
    pub fn add_collection(&mut self, collection: Collection) -> Result<()> {
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
    /// Returns the recognized collection role.
    pub fn role(&self) -> Option<CollectionRole> {
        self.role
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

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[strum(serialize_all = "lowercase")]
/// A recognized EPUB collection role.
pub enum CollectionRole {
    /// A dictionary collection.
    Dictionary,
    /// An index collection.
    Index,
    /// A grouping of index collections.
    IndexGroup,
    /// A distributable-object collection.
    DistributableObject,
    /// A resource manifest collection.
    Manifest,
    /// A publication preview collection.
    Preview,
    /// A scriptable-content collection.
    ScriptableContent,
    /// A collection of learning units.
    Units,
    /// A page-list set.
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
}
