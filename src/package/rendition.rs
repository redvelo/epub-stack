use super::{
    Package, RenditionFlow, RenditionLayout, RenditionOrientation, RenditionSpread,
    metadata::{KnownMetaProperty, Meta},
    spine::{ItemRef, KnownSpineProperty, PageProgressionDirection, PageSpread},
};
use crate::string::EpubString;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// The authored source of a rendition presentation candidate.
pub enum RenditionValueSource {
    /// The candidate came from an unrefined package metadata `meta` element.
    PackageMetadata,
    /// The candidate came from a spine `itemref` property token.
    ItemRefProperty,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// One authored rendition value and its optional recognized semantic projection.
///
/// Package metadata can omit content, so [`Self::authored_value`] and [`Self::value`] are
/// independent. Itemref candidates retain the complete property token spelling as their authored
/// value.
pub struct RenditionCandidate<T> {
    source: RenditionValueSource,
    authored_value: Option<EpubString>,
    value: Option<T>,
}

impl<T> RenditionCandidate<T> {
    /// Returns where this candidate was authored.
    pub fn source(&self) -> RenditionValueSource {
        self.source
    }

    /// Borrows the authored metadata content or complete itemref property token.
    pub fn authored_value(&self) -> Option<&EpubString> {
        self.authored_value.as_ref()
    }

    /// Borrows the recognized typed value, if the authored value was recognized.
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "state", content = "value", rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Authored state of one rendition presentation setting.
///
/// Candidate count is retained without deduplication: repeated declarations are ambiguous even
/// when they project to the same typed value. No reading-system defaults are synthesized.
///
/// For the metadata-backed families, only a recognized itemref property token overrides package
/// metadata. An unrecognized token spelled like one of them, such as
/// `rendition:layout-prepaginated`, leaves the publication-level setting in force; its authored
/// spelling remains available through the occurrence's property tokens.
pub enum RenditionSetting<T> {
    /// No applicable itemref property or package metadata was authored.
    Unspecified,
    /// Exactly one applicable candidate was authored.
    Specified(
        /// The unique authored candidate.
        RenditionCandidate<T>,
    ),
    /// More than one applicable candidate was authored, in source order.
    Ambiguous(
        /// All authored candidates responsible for the ambiguity.
        Vec<RenditionCandidate<T>>,
    ),
}

impl<T> RenditionSetting<T> {
    /// Returns the recognized value only when exactly one candidate was authored and recognized.
    pub fn value(&self) -> Option<&T> {
        self.candidate().and_then(RenditionCandidate::value)
    }

    /// Returns the unique candidate, or `None` when unspecified or ambiguous.
    pub fn candidate(&self) -> Option<&RenditionCandidate<T>> {
        match self {
            Self::Specified(candidate) => Some(candidate),
            Self::Unspecified | Self::Ambiguous(_) => None,
        }
    }

    /// Borrows all authored candidates in source order.
    pub fn candidates(&self) -> &[RenditionCandidate<T>] {
        match self {
            Self::Unspecified => &[],
            Self::Specified(candidate) => std::slice::from_ref(candidate),
            Self::Ambiguous(candidates) => candidates,
        }
    }

    fn from_candidates(candidates: Vec<RenditionCandidate<T>>) -> Self {
        match candidates.len() {
            0 => Self::Unspecified,
            1 => Self::Specified(candidates.into_iter().next().expect("one candidate")),
            _ => Self::Ambiguous(candidates),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Occurrence-owned authored presentation state for one reading-order entry.
///
/// Rendition settings retain provenance, ambiguity, malformed source values, and itemref
/// precedence. Only a recognized itemref token overrides package metadata; see
/// [`RenditionSetting`]. Page progression is copied from the spine and preserves an authored
/// `default`; absent values and reading-system defaults remain absent.
pub struct ReadingOrderPresentation {
    layout: RenditionSetting<RenditionLayout>,
    flow: RenditionSetting<RenditionFlow>,
    orientation: RenditionSetting<RenditionOrientation>,
    spread: RenditionSetting<RenditionSpread>,
    page_spread: RenditionSetting<PageSpread>,
    page_progression_direction: Option<PageProgressionDirection>,
}

impl ReadingOrderPresentation {
    /// Returns the authored rendition layout state.
    pub fn layout(&self) -> &RenditionSetting<RenditionLayout> {
        &self.layout
    }

    /// Returns the authored historical rendition flow state.
    pub fn flow(&self) -> &RenditionSetting<RenditionFlow> {
        &self.flow
    }

    /// Returns the authored historical rendition orientation state.
    pub fn orientation(&self) -> &RenditionSetting<RenditionOrientation> {
        &self.orientation
    }

    /// Returns the authored historical rendition spread state.
    pub fn spread(&self) -> &RenditionSetting<RenditionSpread> {
        &self.spread
    }

    /// Returns the itemref-only synthetic page-spread placement state.
    pub fn page_spread(&self) -> &RenditionSetting<PageSpread> {
        &self.page_spread
    }

    /// Returns the spine-wide authored page progression direction, preserving `default`.
    pub fn page_progression_direction(&self) -> Option<PageProgressionDirection> {
        self.page_progression_direction
    }

    pub(crate) fn of(package: &Package, itemref: &ItemRef) -> Self {
        Self {
            layout: rendition_setting(
                package,
                itemref,
                KnownMetaProperty::RenditionLayout,
                Meta::rendition_layout,
                KnownSpineProperty::rendition_layout,
            ),
            flow: rendition_setting(
                package,
                itemref,
                KnownMetaProperty::RenditionFlow,
                Meta::rendition_flow,
                KnownSpineProperty::rendition_flow,
            ),
            orientation: rendition_setting(
                package,
                itemref,
                KnownMetaProperty::RenditionOrientation,
                Meta::rendition_orientation,
                KnownSpineProperty::rendition_orientation,
            ),
            spread: rendition_setting(
                package,
                itemref,
                KnownMetaProperty::RenditionSpread,
                Meta::rendition_spread,
                KnownSpineProperty::rendition_spread,
            ),
            page_spread: RenditionSetting::from_candidates(itemref_candidates(
                itemref,
                KnownSpineProperty::page_spread,
                &["rendition:page-spread-", "page-spread-"],
            )),
            page_progression_direction: package.spine().page_progression_direction(),
        }
    }
}

fn rendition_setting<T>(
    package: &Package,
    itemref: &ItemRef,
    property: KnownMetaProperty,
    meta_value: impl Fn(&Meta) -> Option<T>,
    itemref_value: impl Fn(KnownSpineProperty) -> Option<T>,
) -> RenditionSetting<T> {
    let overrides = recognized_itemref_candidates(itemref, itemref_value);
    if !overrides.is_empty() {
        return RenditionSetting::from_candidates(overrides);
    }

    RenditionSetting::from_candidates(
        package
            .metadata()
            .meta()
            .iter()
            .filter(|meta| {
                meta.refines().is_none()
                    && meta.property().and_then(|token| token.known_value()) == Some(property)
            })
            .map(|meta| RenditionCandidate {
                source: RenditionValueSource::PackageMetadata,
                authored_value: meta.content().cloned(),
                value: meta_value(meta),
            })
            .collect(),
    )
}

fn recognized_itemref_candidates<T>(
    itemref: &ItemRef,
    value: impl Fn(KnownSpineProperty) -> Option<T>,
) -> Vec<RenditionCandidate<T>> {
    itemref
        .properties()
        .iter()
        .filter_map(|token| {
            let value = token.known_value().and_then(&value)?;
            Some(RenditionCandidate {
                source: RenditionValueSource::ItemRefProperty,
                authored_value: EpubString::new(token.as_str()),
                value: Some(value),
            })
        })
        .collect()
}

fn itemref_candidates<T>(
    itemref: &ItemRef,
    value: impl Fn(KnownSpineProperty) -> Option<T>,
    families: &[&str],
) -> Vec<RenditionCandidate<T>> {
    itemref
        .properties()
        .iter()
        .filter_map(|token| {
            let value = token.known_value().and_then(&value);
            if value.is_none()
                && !families.iter().any(|family| {
                    token
                        .as_str()
                        .get(..family.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(family))
                })
            {
                return None;
            }
            Some(RenditionCandidate {
                source: RenditionValueSource::ItemRefProperty,
                authored_value: EpubString::new(token.as_str()),
                value,
            })
        })
        .collect()
}
