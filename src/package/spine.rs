//! Inspect and edit a publication's reading order.
//!
//! [`Spine`] contains [`ItemRef`] entries in reading order. Use [`Package`](crate::package::Package)
//! mutation methods when manifest-target consistency matters; detached `Spine` methods do not
//! resolve `idref` values. [`SpinePropertyToken`]
//! retains spelling after surrounding Unicode whitespace is trimmed and exposes recognized
//! [`KnownSpineProperty`] values separately. Spine
//! `toc` and itemref `idref` values retain exact decoded source text.

use super::{
    PackageError, PackageField, RenditionFlow, RenditionLayout, RenditionOrientation,
    RenditionSpread, Result, required_package_string,
};
use crate::string::EpubString;

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
/// Reading progression accepted by the spine `page-progression-direction` attribute.
pub enum PageProgressionDirection {
    /// Pages progress from left to right.
    Ltr,
    /// Pages progress from right to left.
    Rtl,
    /// The reading system chooses its default progression.
    Default,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// An OPF spine and its itemrefs in reading order.
pub struct Spine {
    id: Option<EpubString>,
    page_progression_direction: Option<PageProgressionDirection>,
    toc: Option<String>,
    itemrefs: Vec<ItemRef>,
}
impl Spine {
    pub(super) fn from_parsed(
        id: Option<EpubString>,
        page_progression_direction: Option<PageProgressionDirection>,
        toc: Option<String>,
    ) -> Self {
        Self {
            id,
            page_progression_direction,
            toc,
            itemrefs: Vec::new(),
        }
    }

    /// Creates an empty spine with no optional attributes.
    pub(crate) fn new_empty() -> Self {
        Self {
            id: None,
            page_progression_direction: None,
            toc: None,
            itemrefs: Vec::new(),
        }
    }

    /// Borrows the optional spine ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Borrows the optional EPUB 2 NCX manifest ID reference.
    pub fn toc(&self) -> Option<&str> {
        self.toc.as_deref()
    }
    /// Returns the recognized page progression direction.
    pub fn page_progression_direction(&self) -> Option<PageProgressionDirection> {
        self.page_progression_direction
    }
    /// Borrows itemrefs in reading order.
    pub fn itemrefs(&self) -> &[ItemRef] {
        &self.itemrefs
    }

    /// Appends an owned itemref without resolving its manifest target.
    ///
    /// Use [`super::Package::add_spine_itemref`] when target consistency is required.
    pub(crate) fn add_itemref(&mut self, itemref: ItemRef) {
        self.itemrefs.push(itemref);
    }

    /// Removes the itemref at `index`, shifting later entries.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] without mutation when out of range.
    pub(crate) fn remove_itemref_at(&mut self, index: usize) -> Result<()> {
        if index >= self.itemrefs.len() {
            return Err(PackageError::SpineItemrefIndexMissing { index });
        }
        self.itemrefs.remove(index);
        Ok(())
    }

    /// Replaces the itemref at `index` without resolving its manifest target.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] without mutation when out of range.
    pub(crate) fn replace_itemref_at(&mut self, index: usize, itemref: ItemRef) -> Result<()> {
        let Some(existing) = self.itemrefs.get_mut(index) else {
            return Err(PackageError::SpineItemrefIndexMissing { index });
        };
        *existing = itemref;
        Ok(())
    }

    /// Moves one itemref to a final zero-based position.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SpineItemrefIndexMissing`] without mutation if either index is
    /// out of range. Moving shifts intervening entries.
    pub(crate) fn move_itemref(&mut self, from: usize, to: usize) -> Result<()> {
        if from >= self.itemrefs.len() {
            return Err(PackageError::SpineItemrefIndexMissing { index: from });
        }
        if to >= self.itemrefs.len() {
            return Err(PackageError::SpineItemrefIndexMissing { index: to });
        }
        if from == to {
            return Ok(());
        }
        let itemref = self.itemrefs.remove(from);
        self.itemrefs.insert(to, itemref);
        Ok(())
    }
}
/// An OPF spine `itemref`.
///
/// Parsed instances can lack a required `idref`; programmatic construction requires one.
/// See <https://www.w3.org/TR/epub-34/#attrdef-itemref-linear>.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ItemRef {
    id: Option<EpubString>,
    idref: Option<String>,
    linear: Linear,
    properties: Vec<SpinePropertyToken>,
}
impl ItemRef {
    pub(super) fn from_parsed(
        id: Option<EpubString>,
        idref: Option<String>,
        linear: Linear,
        properties: Vec<SpinePropertyToken>,
    ) -> Self {
        Self {
            id,
            idref,
            linear,
            properties,
        }
    }

    /// Creates a linear itemref targeting a non-empty manifest ID.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::InvalidManifestId`] when `idref` is not a valid manifest ID.
    pub fn new(idref: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            id: None,
            idref: Some(super::normalize_manifest_id(idref.as_ref())?.to_string()),
            linear: Linear::Yes,
            properties: Vec::new(),
        })
    }

    /// Sets an optional itemref ID.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::EmptyField`] for an empty or whitespace-only ID.
    pub fn with_id(mut self, id: impl AsRef<str>) -> Result<Self> {
        self.id = Some(required_package_string(id, PackageField::ItemRefId)?);
        Ok(self)
    }

    /// Sets whether the item participates in the primary reading order.
    pub fn with_linear(mut self, linear: Linear) -> Self {
        self.linear = linear;
        self
    }

    /// Replaces property tokens without deduplication or spelling normalization.
    pub fn with_properties(mut self, properties: Vec<SpinePropertyToken>) -> Self {
        self.properties = properties;
        self
    }

    /// Appends a canonically spelled known property.
    pub fn with_property(mut self, property: KnownSpineProperty) -> Self {
        self.properties.push(SpinePropertyToken::from(property));
        self
    }

    /// Borrows the optional itemref ID.
    pub fn id(&self) -> Option<&EpubString> {
        self.id.as_ref()
    }
    /// Borrows the target manifest ID, or `None` for a malformed parsed itemref.
    pub fn idref(&self) -> Option<&str> {
        self.idref.as_deref()
    }
    /// Returns linearity; missing or invalid parsed values default to [`Linear::Yes`].
    pub fn linear(&self) -> Linear {
        self.linear
    }
    /// Borrows property tokens in modeled source order.
    pub fn properties(&self) -> &[SpinePropertyToken] {
        self.properties.as_slice()
    }

    /// Reports whether any token projects to `property`.
    ///
    /// Matching uses each token's recognized semantic value.
    pub fn has_property(&self, property: KnownSpineProperty) -> bool {
        self.properties
            .iter()
            .any(|token| token.known_value() == Some(property))
    }

    /// Returns the first recognized current rendition layout override.
    pub fn rendition_layout_override(&self) -> Option<RenditionLayout> {
        self.projected(KnownSpineProperty::rendition_layout)
    }

    /// Returns the first recognized historical rendition flow override.
    pub fn rendition_flow_override(&self) -> Option<RenditionFlow> {
        self.projected(KnownSpineProperty::rendition_flow)
    }

    /// Returns the first recognized historical rendition orientation override.
    pub fn rendition_orientation_override(&self) -> Option<RenditionOrientation> {
        self.projected(KnownSpineProperty::rendition_orientation)
    }

    /// Returns the first recognized historical rendition spread override.
    pub fn rendition_spread_override(&self) -> Option<RenditionSpread> {
        self.projected(KnownSpineProperty::rendition_spread)
    }

    /// Returns the first recognized current synthetic page-spread placement token.
    pub fn page_spread(&self) -> Option<PageSpread> {
        self.projected(KnownSpineProperty::page_spread)
    }

    /// Reports whether this itemref carries the horizontal centering token.
    pub fn align_x_center(&self) -> bool {
        self.properties
            .iter()
            .any(|token| token.known_value() == Some(KnownSpineProperty::RenditionAlignXCenter))
    }

    fn projected<T>(&self, project: impl Fn(KnownSpineProperty) -> Option<T>) -> Option<T> {
        self.properties
            .iter()
            .find_map(|token| project(token.known_value()?))
    }
}

/// A spine `properties` token retaining its authored spelling and optional known projection.
pub type SpinePropertyToken = crate::vocab::VocabToken<KnownSpineProperty>;

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
/// A recognized EPUB spine `properties` token.
pub enum KnownSpineProperty {
    #[strum(
        serialize = "rendition:align-x-center",
        to_string = "rendition:align-x-center"
    )]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:align-x-center"))]
    /// Centers each rendered page on the horizontal axis.
    RenditionAlignXCenter,
    #[strum(serialize = "rendition:layout-pre-paginated")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:layout-pre-paginated"))]
    /// Overrides a spine item to use a pre-paginated layout.
    RenditionLayoutPrePaginated,
    #[strum(serialize = "rendition:layout-reflowable")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:layout-reflowable"))]
    /// Overrides a spine item to use a reflowable layout.
    RenditionLayoutReflowable,
    #[strum(serialize = "rendition:flow-auto")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:flow-auto"))]
    /// Historical automatic flow override.
    RenditionFlowAuto,
    #[strum(serialize = "rendition:flow-paginated")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:flow-paginated"))]
    /// Historical paginated flow override.
    RenditionFlowPaginated,
    #[strum(serialize = "rendition:flow-scrolled-continuous")]
    #[cfg_attr(
        feature = "serde",
        serde(rename = "rendition:flow-scrolled-continuous")
    )]
    /// Historical continuous-scrolling flow override.
    RenditionFlowScrolledContinuous,
    #[strum(serialize = "rendition:flow-scrolled-doc")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:flow-scrolled-doc"))]
    /// Historical per-document scrolling flow override.
    RenditionFlowScrolledDoc,
    #[strum(serialize = "rendition:orientation-auto")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:orientation-auto"))]
    /// Historical automatic orientation override.
    RenditionOrientationAuto,
    #[strum(serialize = "rendition:orientation-landscape")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:orientation-landscape"))]
    /// Historical landscape orientation override.
    RenditionOrientationLandscape,
    #[strum(serialize = "rendition:orientation-portrait")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:orientation-portrait"))]
    /// Historical portrait orientation override.
    RenditionOrientationPortrait,
    #[strum(serialize = "rendition:spread-auto")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread-auto"))]
    /// Historical automatic spread override.
    RenditionSpreadAuto,
    #[strum(serialize = "rendition:spread-both")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread-both"))]
    /// Historical both-orientations spread override.
    RenditionSpreadBoth,
    #[strum(serialize = "rendition:spread-landscape")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread-landscape"))]
    /// Historical landscape spread override.
    RenditionSpreadLandscape,
    #[strum(serialize = "rendition:spread-none")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread-none"))]
    /// Historical no-spread override.
    RenditionSpreadNone,
    #[strum(serialize = "rendition:spread-portrait")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:spread-portrait"))]
    /// Historical portrait spread override.
    RenditionSpreadPortrait,
    #[strum(serialize = "rendition:page-spread-left")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:page-spread-left"))]
    /// Places the page on the left side of a synthetic spread.
    PageSpreadLeft,
    #[strum(serialize = "rendition:page-spread-right")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:page-spread-right"))]
    /// Places the page on the right side of a synthetic spread.
    PageSpreadRight,
    #[strum(serialize = "rendition:page-spread-center")]
    #[cfg_attr(feature = "serde", serde(rename = "rendition:page-spread-center"))]
    /// Centers the page across a synthetic spread.
    PageSpreadCenter,
    #[strum(serialize = "page-spread-left")]
    #[cfg_attr(feature = "serde", serde(rename = "page-spread-left"))]
    /// The unprefixed spine-vocabulary alias for left-page placement.
    UnprefixedPageSpreadLeft,
    #[strum(serialize = "page-spread-right")]
    #[cfg_attr(feature = "serde", serde(rename = "page-spread-right"))]
    /// The unprefixed spine-vocabulary alias for right-page placement.
    UnprefixedPageSpreadRight,
}

impl KnownSpineProperty {
    /// Projects a current rendition layout override, if this token declares one.
    pub fn rendition_layout(self) -> Option<RenditionLayout> {
        match self {
            Self::RenditionLayoutPrePaginated => Some(RenditionLayout::PrePaginated),
            Self::RenditionLayoutReflowable => Some(RenditionLayout::Reflowable),
            _ => None,
        }
    }

    /// Projects a historical rendition flow override, if this token declares one.
    pub fn rendition_flow(self) -> Option<RenditionFlow> {
        match self {
            Self::RenditionFlowAuto => Some(RenditionFlow::Auto),
            Self::RenditionFlowPaginated => Some(RenditionFlow::Paginated),
            Self::RenditionFlowScrolledContinuous => Some(RenditionFlow::ScrolledContinuous),
            Self::RenditionFlowScrolledDoc => Some(RenditionFlow::ScrolledDoc),
            _ => None,
        }
    }

    /// Projects a historical rendition orientation override, if this token declares one.
    pub fn rendition_orientation(self) -> Option<RenditionOrientation> {
        match self {
            Self::RenditionOrientationAuto => Some(RenditionOrientation::Auto),
            Self::RenditionOrientationLandscape => Some(RenditionOrientation::Landscape),
            Self::RenditionOrientationPortrait => Some(RenditionOrientation::Portrait),
            _ => None,
        }
    }

    /// Projects a historical rendition spread override, if this token declares one.
    pub fn rendition_spread(self) -> Option<RenditionSpread> {
        match self {
            Self::RenditionSpreadAuto => Some(RenditionSpread::Auto),
            Self::RenditionSpreadBoth => Some(RenditionSpread::Both),
            Self::RenditionSpreadLandscape => Some(RenditionSpread::Landscape),
            Self::RenditionSpreadNone => Some(RenditionSpread::None),
            Self::RenditionSpreadPortrait => Some(RenditionSpread::Portrait),
            _ => None,
        }
    }

    /// Projects a synthetic page-spread placement, including unprefixed legacy spellings.
    pub fn page_spread(self) -> Option<PageSpread> {
        match self {
            Self::PageSpreadLeft | Self::UnprefixedPageSpreadLeft => Some(PageSpread::Left),
            Self::PageSpreadRight | Self::UnprefixedPageSpreadRight => Some(PageSpread::Right),
            Self::PageSpreadCenter => Some(PageSpread::Center),
            _ => None,
        }
    }
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, Hash, strum_macros::Display, strum_macros::EnumString,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
/// A current EPUB synthetic page-spread placement projected from an itemref property.
pub enum PageSpread {
    /// Places the page on the left side of a synthetic spread.
    Left,
    /// Places the page on the right side of a synthetic spread.
    Right,
    /// Centers the page across a synthetic spread.
    Center,
}

#[derive(
    Debug, PartialEq, Eq, Clone, Copy, strum_macros::Display, strum_macros::EnumString, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
#[derive(Default)]
/// Whether a spine item belongs to the primary reading order.
pub enum Linear {
    #[default]
    /// The item belongs to the primary reading order.
    Yes,
    /// The item is auxiliary to the primary reading order.
    No,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn programmatic_spine_relationships_reject_invalid_ids() {
        assert!(matches!(
            ItemRef::new("\u{a0}chapter"),
            Err(PackageError::InvalidManifestId(_))
        ));
        assert!(crate::package::normalize_manifest_id("1ncx").is_err());
    }

    fn spine_with(idrefs: &[&str]) -> Spine {
        let mut spine = Spine::new_empty();
        for idref in idrefs {
            spine.add_itemref(ItemRef::new(idref).unwrap());
        }
        spine
    }

    fn idrefs(spine: &Spine) -> Vec<&str> {
        spine.itemrefs().iter().filter_map(ItemRef::idref).collect()
    }

    #[test]
    fn missing_and_out_of_range_mutations_are_atomic() {
        let mut spine = spine_with(&["one", "two"]);

        for index in [2, usize::MAX] {
            let before = spine.clone();
            assert!(matches!(
                spine.remove_itemref_at(index),
                Err(PackageError::SpineItemrefIndexMissing { index: actual }) if actual == index
            ));
            assert_eq!(spine, before);

            let before = spine.clone();
            assert!(matches!(
                spine.replace_itemref_at(index, ItemRef::new("replacement").unwrap()),
                Err(PackageError::SpineItemrefIndexMissing { index: actual }) if actual == index
            ));
            assert_eq!(spine, before);
        }
    }

    #[test]
    fn move_itemref_uses_final_indices_and_is_atomic_on_failure() {
        for (from, to, expected) in [
            (0, 2, vec!["two", "three", "one"]),
            (2, 0, vec!["three", "one", "two"]),
            (1, 1, vec!["one", "two", "three"]),
        ] {
            let mut spine = spine_with(&["one", "two", "three"]);
            spine.move_itemref(from, to).unwrap();
            assert_eq!(idrefs(&spine), expected);
        }

        for (from, to, missing) in [(3, 0, 3), (0, 3, 3)] {
            let mut spine = spine_with(&["one", "two", "three"]);
            let before = spine.clone();
            assert!(matches!(
                spine.move_itemref(from, to),
                Err(PackageError::SpineItemrefIndexMissing { index }) if index == missing
            ));
            assert_eq!(spine, before);
        }
    }

    #[test]
    fn remove_itemref_at_allows_an_empty_spine() {
        let mut spine = spine_with(&["chapter"]);
        spine.remove_itemref_at(0).unwrap();
        assert!(spine.itemrefs().is_empty());
    }

    #[test]
    fn itemref_builder_accepts_unknown_property_tokens() {
        let itemref = ItemRef::new("chapter").unwrap().with_properties(vec![
            KnownSpineProperty::PageSpreadCenter.into(),
            SpinePropertyToken::try_new("custom:foo").unwrap(),
        ]);

        assert_eq!(
            itemref
                .properties()
                .iter()
                .map(SpinePropertyToken::as_str)
                .collect::<Vec<_>>(),
            vec!["rendition:page-spread-center", "custom:foo"]
        );
    }

    #[test]
    fn spine_rendition_properties_have_exact_canonical_spellings() {
        let values = [
            (
                KnownSpineProperty::RenditionLayoutPrePaginated,
                "rendition:layout-pre-paginated",
            ),
            (
                KnownSpineProperty::RenditionLayoutReflowable,
                "rendition:layout-reflowable",
            ),
            (KnownSpineProperty::RenditionFlowAuto, "rendition:flow-auto"),
            (
                KnownSpineProperty::RenditionFlowPaginated,
                "rendition:flow-paginated",
            ),
            (
                KnownSpineProperty::RenditionFlowScrolledContinuous,
                "rendition:flow-scrolled-continuous",
            ),
            (
                KnownSpineProperty::RenditionFlowScrolledDoc,
                "rendition:flow-scrolled-doc",
            ),
            (
                KnownSpineProperty::RenditionOrientationAuto,
                "rendition:orientation-auto",
            ),
            (
                KnownSpineProperty::RenditionOrientationLandscape,
                "rendition:orientation-landscape",
            ),
            (
                KnownSpineProperty::RenditionOrientationPortrait,
                "rendition:orientation-portrait",
            ),
            (
                KnownSpineProperty::RenditionSpreadAuto,
                "rendition:spread-auto",
            ),
            (
                KnownSpineProperty::RenditionSpreadBoth,
                "rendition:spread-both",
            ),
            (
                KnownSpineProperty::RenditionSpreadLandscape,
                "rendition:spread-landscape",
            ),
            (
                KnownSpineProperty::RenditionSpreadNone,
                "rendition:spread-none",
            ),
            (
                KnownSpineProperty::RenditionSpreadPortrait,
                "rendition:spread-portrait",
            ),
            (
                KnownSpineProperty::PageSpreadLeft,
                "rendition:page-spread-left",
            ),
            (
                KnownSpineProperty::PageSpreadRight,
                "rendition:page-spread-right",
            ),
            (
                KnownSpineProperty::PageSpreadCenter,
                "rendition:page-spread-center",
            ),
            (
                KnownSpineProperty::UnprefixedPageSpreadLeft,
                "page-spread-left",
            ),
            (
                KnownSpineProperty::UnprefixedPageSpreadRight,
                "page-spread-right",
            ),
        ];

        for (property, spelling) in values {
            assert_eq!(property.to_string(), spelling);
            assert_eq!(
                KnownSpineProperty::from_str(&spelling.to_ascii_uppercase()),
                Ok(property)
            );
        }
    }

    #[test]
    fn itemref_projects_first_recognized_rendition_tokens_and_preserves_raw_tokens() {
        let itemref = ItemRef::new("chapter").unwrap().with_properties(vec![
            SpinePropertyToken::try_new("custom:unknown").unwrap(),
            SpinePropertyToken::try_new("RENDITION:LAYOUT-PRE-PAGINATED").unwrap(),
            SpinePropertyToken::try_new("RENDITION:FLOW-SCROLLED-DOC").unwrap(),
            SpinePropertyToken::try_new("rendition:orientation-landscape").unwrap(),
            SpinePropertyToken::try_new("rendition:spread-none").unwrap(),
            SpinePropertyToken::try_new("RENDITION:PAGE-SPREAD-RIGHT").unwrap(),
            SpinePropertyToken::try_new("rendition:layout-reflowable").unwrap(),
        ]);

        assert_eq!(
            itemref.rendition_layout_override(),
            Some(RenditionLayout::PrePaginated)
        );
        assert_eq!(
            itemref.rendition_flow_override(),
            Some(RenditionFlow::ScrolledDoc)
        );
        assert_eq!(
            itemref.rendition_orientation_override(),
            Some(RenditionOrientation::Landscape)
        );
        assert_eq!(
            itemref.rendition_spread_override(),
            Some(RenditionSpread::None)
        );
        assert_eq!(itemref.page_spread(), Some(PageSpread::Right));
        assert_eq!(itemref.properties()[0].as_str(), "custom:unknown");
        assert_eq!(itemref.properties()[0].known_value(), None);
        assert_eq!(
            itemref.properties()[1].as_str(),
            "RENDITION:LAYOUT-PRE-PAGINATED"
        );

        let aliases = ItemRef::new("chapter").unwrap().with_properties(vec![
            SpinePropertyToken::try_new("page-spread-left").unwrap(),
            SpinePropertyToken::try_new("page-spread-right").unwrap(),
        ]);
        assert_eq!(aliases.page_spread(), Some(PageSpread::Left));
        assert_eq!(
            aliases.properties()[1].known_value(),
            Some(KnownSpineProperty::UnprefixedPageSpreadRight)
        );
    }
}
