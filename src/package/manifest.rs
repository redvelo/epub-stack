//! Declare publication resources and inspect manifest properties.
//!
//! [`Manifest`] stores [`ManifestItem`] values in package order. Use [`EpubHref`] for a usable
//! href and [`AuthoredHref`](crate::resource::AuthoredHref) when inspecting parsed source
//! evidence. Manifest item IDs, fallback IDREFs, and media-overlay IDREFs retain exact decoded
//! source text. [`ManifestPropertyToken`](crate::package::manifest::ManifestPropertyToken) keeps
//! token spelling after surrounding Unicode whitespace is trimmed and exposes recognized values
//! through [`KnownManifestProperty`].

use super::{PackageError, Result, normalize_manifest_id, required_package_string};
use crate::media_type::MediaType;
use crate::resource::{AuthoredHref, EpubHref};
use crate::string::{EpubString, EpubStringEmpty};
use std::str::FromStr;

fn dedup_manifest_properties(properties: &mut Vec<ManifestPropertyToken>) {
    let mut seen_known = Vec::new();
    let mut seen_unknown = Vec::new();
    properties.retain(|property| {
        if let Some(known) = property.known_value() {
            if seen_known.contains(&known) {
                return false;
            }
            seen_known.push(known);
            return true;
        }

        if seen_unknown
            .iter()
            .any(|seen: &EpubString| seen == property.raw_value())
        {
            return false;
        }
        seen_unknown.push(property.raw_value().clone());
        true
    });
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned OPF manifest and its items in modeled source order.
///
/// Parsed manifests can contain missing or duplicate authored fields. Programmatic mutations
/// enforce unique modeled IDs and authored hrefs and normalize duplicate property tokens.
pub struct Manifest {
    id: Option<EpubString>,
    items: Vec<ManifestItem>,
}

impl Manifest {
    /// Creates an empty manifest with no ID.
    pub fn new_empty() -> Self {
        Self {
            id: None,
            items: Vec::new(),
        }
    }

    /// Sets the manifest ID.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::EmptyField`] for an empty or whitespace-only ID.
    pub fn with_id(mut self, id: impl AsRef<str>) -> Result<Self> {
        self.id = Some(required_package_string(id, "manifest id")?);
        Ok(self)
    }

    /// Borrows the optional manifest ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }

    /// Borrows manifest items in modeled source order.
    pub fn items(&self) -> &[ManifestItem] {
        &self.items
    }

    pub(super) fn from_parsed(id: Option<EpubString>, items: Vec<ManifestItem>) -> Self {
        Self { id, items }
    }

    pub(super) fn append(&mut self, mut other: Self) {
        if self.id.is_none() {
            self.id = other.id.take();
        }
        self.items.append(&mut other.items);
    }

    /// Appends an owned item after enforcing manifest uniqueness.
    ///
    /// Known properties are deduplicated semantically and unknown properties by stored spelling,
    /// preserving first occurrence order.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-ID or duplicate-authored-href error. Failure leaves the manifest
    /// unchanged.
    pub fn add_item(&mut self, mut item: ManifestItem) -> Result<()> {
        if item.id().is_some_and(|id| {
            self.items.iter().any(|existing| {
                existing
                    .id()
                    .and_then(|value| normalize_manifest_id(value).ok())
                    == Some(id)
            })
        }) {
            return Err(PackageError::ManifestIdDuplicate {
                id: item.id().map(ToString::to_string).unwrap_or_default(),
            });
        }
        if item.authored_href().is_some()
            && self
                .items
                .iter()
                .any(|existing| existing.authored_href() == item.authored_href())
        {
            return Err(PackageError::ManifestHrefDuplicate {
                href: item
                    .authored_href()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        }
        item.normalize();
        self.items.push(item);
        Ok(())
    }

    /// Removes every item whose ID equals `id`.
    ///
    /// This detached manifest operation does not check spine references; use
    /// [`super::Package::remove_manifest_item`] when editing a package.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ManifestItemMissing`] without mutation if no item matches.
    pub fn remove_item(&mut self, id: impl AsRef<str>) -> Result<()> {
        let id = normalize_manifest_id(id.as_ref())?;
        let len = self.items.len();
        self.items.retain(|item| {
            item.id()
                .and_then(|item_id| normalize_manifest_id(item_id).ok())
                != Some(id)
        });
        if self.items.len() == len {
            return Err(PackageError::ManifestItemMissing { id: id.to_string() });
        }
        Ok(())
    }

    pub(super) fn remove_item_at(&mut self, index: usize) -> Option<ManifestItem> {
        (index < self.items.len()).then(|| self.items.remove(index))
    }

    /// Replaces the first item with `id`, preserving its list position.
    ///
    /// The replacement's property tokens are deduplicated. This detached operation does not
    /// update or check spine references.
    ///
    /// # Errors
    ///
    /// Returns a missing-item, duplicate-ID, or duplicate-authored-href error. Failure leaves the
    /// manifest unchanged.
    pub fn replace_item(&mut self, id: impl AsRef<str>, mut item: ManifestItem) -> Result<()> {
        let id = normalize_manifest_id(id.as_ref())?;
        let Some(index) = self.items.iter().position(|existing| {
            existing
                .id()
                .and_then(|item_id| normalize_manifest_id(item_id).ok())
                == Some(id)
        }) else {
            return Err(PackageError::ManifestItemMissing { id: id.to_string() });
        };
        if item.id().is_some_and(|item_id| item_id != id)
            && self.items.iter().any(|existing| {
                existing
                    .id()
                    .and_then(|value| normalize_manifest_id(value).ok())
                    == item.id()
            })
        {
            return Err(PackageError::ManifestIdDuplicate {
                id: item.id().map(ToString::to_string).unwrap_or_default(),
            });
        }
        if item.authored_href().is_some()
            && self
                .items
                .iter()
                .enumerate()
                .any(|(candidate_index, existing)| {
                    candidate_index != index && existing.authored_href() == item.authored_href()
                })
        {
            return Err(PackageError::ManifestHrefDuplicate {
                href: item
                    .authored_href()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        }
        item.normalize();
        self.items[index] = item;
        Ok(())
    }

    pub(super) fn replace_item_at(&mut self, index: usize, mut item: ManifestItem) -> Result<()> {
        if index >= self.items.len() {
            return Err(PackageError::ManifestItemMissing {
                id: index.to_string(),
            });
        }
        let selected_id = self.items[index]
            .id()
            .and_then(|id| normalize_manifest_id(id).ok());
        if item
            .id()
            .is_some_and(|item_id| Some(item_id) != selected_id)
            && item.id().is_some_and(|item_id| {
                self.items
                    .iter()
                    .enumerate()
                    .any(|(candidate_index, existing)| {
                        candidate_index != index
                            && existing
                                .id()
                                .and_then(|value| normalize_manifest_id(value).ok())
                                == Some(item_id)
                    })
            })
        {
            return Err(PackageError::ManifestIdDuplicate {
                id: item.id().map(ToString::to_string).unwrap_or_default(),
            });
        }
        if item.authored_href().is_some()
            && self
                .items
                .iter()
                .enumerate()
                .any(|(candidate_index, existing)| {
                    candidate_index != index && existing.authored_href() == item.authored_href()
                })
        {
            return Err(PackageError::ManifestHrefDuplicate {
                href: item
                    .authored_href()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        }
        item.normalize();
        self.items[index] = item;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An owned semantic OPF manifest item.
///
/// Parsed instances may omit required OPF attributes. Relationship IDs, authored href text, and
/// vocabulary token spellings are retained where represented; unknown attributes and XML
/// formatting are not.
pub struct ManifestItem {
    fallback: Option<String>,
    href: Option<AuthoredHref>,
    media_type: Option<MediaType>,
    media_overlay: Option<String>,
    id: Option<String>,
    properties: Vec<ManifestPropertyToken>,
}

impl ManifestItem {
    pub(super) fn from_parsed(
        fallback: Option<String>,
        href: Option<AuthoredHref>,
        media_type: Option<MediaType>,
        media_overlay: Option<String>,
        id: Option<String>,
        properties: Vec<ManifestPropertyToken>,
    ) -> Self {
        Self {
            fallback,
            href,
            media_type,
            media_overlay,
            id,
            properties,
        }
    }

    /// Starts construction of a complete manifest item.
    pub fn builder() -> ManifestItemBuilder {
        ManifestItemBuilder::default()
    }

    fn new(
        id: String,
        href: EpubHref,
        media_type: MediaType,
        fallback: Option<String>,
        media_overlay: Option<String>,
        mut properties: Vec<ManifestPropertyToken>,
    ) -> Result<Self> {
        let id = normalize_manifest_id(&id)?.to_string();
        let fallback = fallback
            .map(|value| normalize_manifest_id(&value).map(str::to_string))
            .transpose()?;
        let media_overlay = media_overlay
            .map(|value| normalize_manifest_id(&value).map(str::to_string))
            .transpose()?;
        dedup_manifest_properties(&mut properties);
        Ok(Self {
            id: Some(id),
            href: Some(AuthoredHref::from(href)),
            media_type: Some(media_type),
            fallback,
            media_overlay,
            properties,
        })
    }

    /// Borrows the optional item ID.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Returns an owned usable EPUB href projection.
    ///
    /// Parsed empty or whitespace-only hrefs return `None`; inspect [`Self::authored_href`]
    /// to distinguish them from a missing attribute.
    pub fn href(&self) -> Option<EpubHref> {
        self.href.as_ref().and_then(AuthoredHref::to_epub_href)
    }
    /// Borrows the source-preserving authored href, including unusable text.
    pub fn authored_href(&self) -> Option<&AuthoredHref> {
        self.href.as_ref()
    }
    /// Borrows the modeled media type without canonicalizing its authored spelling.
    pub fn media_type(&self) -> Option<&MediaType> {
        self.media_type.as_ref()
    }
    /// Borrows the fallback manifest ID.
    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
    }
    /// Borrows the media-overlay manifest ID.
    pub fn media_overlay(&self) -> Option<&str> {
        self.media_overlay.as_deref()
    }
    /// Borrows property tokens in authored order after any programmatic deduplication.
    pub fn properties(&self) -> &[ManifestPropertyToken] {
        self.properties.as_slice()
    }

    /// Reports whether any token projects to `property`.
    ///
    /// Matching is semantic and case-insensitive for recognized parsed tokens.
    pub fn has_property(&self, property: KnownManifestProperty) -> bool {
        self.properties
            .iter()
            .any(|token| token.known_value() == Some(property))
    }

    fn normalize(&mut self) {
        dedup_manifest_properties(&mut self.properties);
    }
}

#[derive(Debug, Default)]
/// Builder for a validated programmatic [`ManifestItem`].
pub struct ManifestItemBuilder {
    id: Option<String>,
    href: Option<EpubHref>,
    media_type: Option<MediaType>,
    fallback: Option<String>,
    media_overlay: Option<String>,
    properties: Vec<ManifestPropertyToken>,
}

impl ManifestItemBuilder {
    /// Sets the manifest ID from untrimmed caller text.
    pub fn id(mut self, id: impl AsRef<str>) -> Self {
        self.id = Some(id.as_ref().to_string());
        self
    }

    /// Sets the authored href.
    pub fn href(mut self, href: EpubHref) -> Self {
        self.href = Some(href);
        self
    }

    /// Sets the declared media type.
    pub fn media_type(mut self, media_type: MediaType) -> Self {
        self.media_type = Some(media_type);
        self
    }

    /// Sets the fallback manifest IDREF from untrimmed caller text.
    pub fn fallback(mut self, fallback: impl AsRef<str>) -> Self {
        self.fallback = Some(fallback.as_ref().to_string());
        self
    }

    /// Sets the media-overlay manifest IDREF from untrimmed caller text.
    pub fn media_overlay(mut self, media_overlay: impl AsRef<str>) -> Self {
        self.media_overlay = Some(media_overlay.as_ref().to_string());
        self
    }

    /// Sets manifest property tokens.
    pub fn properties(mut self, properties: Vec<ManifestPropertyToken>) -> Self {
        self.properties = properties;
        self
    }

    /// Validates relationship IDs and finishes the manifest item.
    pub fn build(self) -> Result<ManifestItem> {
        let id = self.id.ok_or(PackageError::EmptyField {
            field: "manifest item id",
        })?;
        let href = self.href.ok_or(PackageError::EmptyField {
            field: "manifest item href",
        })?;
        let media_type = self.media_type.ok_or(PackageError::EmptyField {
            field: "manifest item media-type",
        })?;
        ManifestItem::new(
            id,
            href,
            media_type,
            self.fallback,
            self.media_overlay,
            self.properties,
        )
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// A manifest property token retaining its authored spelling and optional known projection.
pub struct ManifestPropertyToken {
    raw: EpubString,
    known: Option<KnownManifestProperty>,
}

impl ManifestPropertyToken {
    /// Creates a token using the canonical spelling of a known property.
    pub fn known(property: KnownManifestProperty) -> Self {
        let raw = EpubString::new(property.to_string()).expect("known property is non-empty");
        Self {
            raw,
            known: Some(property),
        }
    }

    /// Parses a token while preserving its spelling after trimming surrounding whitespace.
    ///
    /// Returns `None` for empty or whitespace-only input. Known-value recognition is ASCII
    /// case-insensitive; unknown values remain available through [`Self::raw_value`].
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let raw = EpubString::new(value.as_ref())?;
        let known = KnownManifestProperty::from_str(raw.as_str()).ok();
        Some(Self { raw, known })
    }

    /// Creates a token from authored text after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`EpubStringEmpty`] for empty or whitespace-only input.
    pub fn raw(value: impl AsRef<str>) -> std::result::Result<Self, EpubStringEmpty> {
        EpubString::try_new(value).map(Self::from_raw)
    }

    /// Classifies an already-trimmed token without changing its stored spelling.
    pub fn from_raw(raw: EpubString) -> Self {
        let known = KnownManifestProperty::from_str(raw.as_str()).ok();
        Self { raw, known }
    }

    /// The stored token.
    pub fn raw_value(&self) -> &EpubString {
        &self.raw
    }

    /// The stored token as `str`.
    pub fn as_str(&self) -> &str {
        self.raw.as_str()
    }

    /// Returns the recognized semantic property, if any.
    pub fn known_value(&self) -> Option<KnownManifestProperty> {
        self.known
    }
}

impl From<KnownManifestProperty> for ManifestPropertyToken {
    fn from(value: KnownManifestProperty) -> Self {
        Self::known(value)
    }
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
/// A recognized EPUB manifest `properties` token.
pub enum KnownManifestProperty {
    /// Identifies the publication cover image.
    CoverImage,
    /// Declares MathML content.
    Mathml,
    /// Identifies the EPUB navigation document.
    Nav,
    /// Declares references to remote resources.
    RemoteResources,
    /// Declares scripted content.
    Scripted,
    /// Declares SVG content.
    Svg,
    /// Declares the obsolete EPUB `switch` construct.
    Switch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programmatic_relationship_ids_reject_invalid_ncname_values() {
        assert!(matches!(
            ManifestItem::builder()
                .id("1bad")
                .href(EpubHref::try_new("chapter.xhtml").unwrap())
                .media_type(MediaType::from(
                    EpubString::try_new("application/xhtml+xml").unwrap(),
                ))
                .build(),
            Err(PackageError::InvalidManifestId(_))
        ));
        assert!(matches!(
            ManifestItem::builder()
                .id("chapter")
                .href(EpubHref::try_new("chapter.xhtml").unwrap())
                .media_type(MediaType::from(
                    EpubString::try_new("application/xhtml+xml").unwrap(),
                ))
                .fallback("\u{a0}fallback")
                .build(),
            Err(PackageError::InvalidManifestId(_))
        ));
    }

    fn item(id: &str, href: &str) -> ManifestItem {
        ManifestItem::builder()
            .id(EpubString::try_new(id).unwrap())
            .href(EpubHref::try_new(href).unwrap())
            .media_type(MediaType::from(
                EpubString::try_new("application/xhtml+xml").unwrap(),
            ))
            .build()
            .unwrap()
    }

    #[test]
    fn manifest_mutations_reject_duplicates_and_missing_targets_atomically() {
        let mut manifest = Manifest::new_empty();
        manifest.add_item(item("one", "one.xhtml")).unwrap();
        manifest.add_item(item("two", "two.xhtml")).unwrap();

        for (replacement, expected) in [
            (
                item("one", "other.xhtml"),
                PackageError::ManifestIdDuplicate {
                    id: "one".to_string(),
                },
            ),
            (
                item("other", "one.xhtml"),
                PackageError::ManifestHrefDuplicate {
                    href: "one.xhtml".to_string(),
                },
            ),
        ] {
            let before = manifest.clone();
            let error = manifest.add_item(replacement).unwrap_err();
            assert!(
                matches!(
                    (&error, &expected),
                    (
                        PackageError::ManifestIdDuplicate { id: actual },
                        PackageError::ManifestIdDuplicate { id: wanted }
                    ) if actual == wanted
                ) || matches!(
                    (&error, &expected),
                    (
                        PackageError::ManifestHrefDuplicate { href: actual },
                        PackageError::ManifestHrefDuplicate { href: wanted }
                    ) if actual == wanted
                )
            );
            assert_eq!(manifest, before);
        }

        let before = manifest.clone();
        assert!(matches!(
            manifest.remove_item("missing"),
            Err(PackageError::ManifestItemMissing { id }) if id == "missing"
        ));
        assert_eq!(manifest, before);

        for replacement in [item("one", "replacement.xhtml"), item("other", "one.xhtml")] {
            let before = manifest.clone();
            let error = manifest.replace_item("missing", replacement).unwrap_err();
            assert!(matches!(
                error,
                PackageError::ManifestItemMissing { id } if id == "missing"
            ));
            assert_eq!(manifest, before);
        }
    }

    #[test]
    fn manifest_replacements_reject_other_items_duplicates_atomically() {
        let mut manifest = Manifest::new_empty();
        manifest.add_item(item("one", "one.xhtml")).unwrap();
        manifest.add_item(item("two", "two.xhtml")).unwrap();

        let before = manifest.clone();
        assert!(matches!(
            manifest.replace_item("one", item("two", "replacement.xhtml")),
            Err(PackageError::ManifestIdDuplicate { id }) if id == "two"
        ));
        assert_eq!(manifest, before);

        let before = manifest.clone();
        assert!(matches!(
            manifest.replace_item("one", item("replacement", "two.xhtml")),
            Err(PackageError::ManifestHrefDuplicate { href }) if href == "two.xhtml"
        ));
        assert_eq!(manifest, before);
    }

    #[test]
    fn manifest_item_builder_deduplicates_properties_by_semantics() {
        let item = ManifestItem::builder()
            .id(EpubString::try_new("nav").unwrap())
            .href(EpubHref::try_new("toc.xhtml").unwrap())
            .media_type(MediaType::from(
                EpubString::try_new("application/xhtml+xml").unwrap(),
            ))
            .properties(vec![
                ManifestPropertyToken::raw("NAV").unwrap(),
                KnownManifestProperty::Nav.into(),
                KnownManifestProperty::Scripted.into(),
                KnownManifestProperty::Scripted.into(),
            ])
            .build()
            .unwrap();

        assert_eq!(
            item.properties()
                .iter()
                .map(ManifestPropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["NAV", "scripted"]
        );
        assert!(item.has_property(KnownManifestProperty::Nav));
    }

    #[test]
    fn manifest_item_builder_accepts_unknown_property_tokens() {
        let item = ManifestItem::builder()
            .id(EpubString::try_new("nav").unwrap())
            .href(EpubHref::try_new("toc.xhtml").unwrap())
            .media_type(MediaType::from(
                EpubString::try_new("application/xhtml+xml").unwrap(),
            ))
            .properties(vec![
                KnownManifestProperty::Nav.into(),
                ManifestPropertyToken::raw("custom:foo").unwrap(),
            ])
            .build()
            .unwrap();

        assert_eq!(
            item.properties()
                .iter()
                .map(ManifestPropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["nav", "custom:foo"]
        );
    }
}
