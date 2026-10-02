//! Resolve navigation links, and report which navigation documents loaded.
//!
//! [`navigation_targets`] resolves each point's authored href against a [`ResourceIndex`] in
//! document order, reporting where the link lands without reading the target's content.
//!
//! [`NavigationLoadingFacts`] says how each selected EPUB NAV and NCX fared when the publication
//! was opened; not attempted and missing are separate outcomes.

use crate::{
    navigation::{NavigationDocument, NavigationPoint},
    resource::{
        AuthoredHref, EpubPath, InvalidHref, ResolvedHref, ResourceAddress, ResourceIndex,
        ResourceOrdinal,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Result of attempting to acquire and parse one selected navigation declaration.
pub enum NavigationLoadingOutcome {
    /// The declaration was not attempted, including when selection facts explain its absence.
    NotAttempted,
    /// The selected local resource was absent from the provider.
    MissingResource,
    /// The provider failed while reading the selected resource.
    ReadFailed,
    /// XML encoding detection or decoding failed.
    InvalidEncoding,
    /// The decoded document was not usable EPUB NAV or NCX.
    Malformed,
    /// The selected document loaded successfully.
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
/// Acquisition observations for the package-selected EPUB NAV and NCX declarations.
pub struct NavigationLoadingFacts {
    /// EPUB NAV acquisition outcome.
    pub epub_nav: NavigationLoadingOutcome,
    /// NCX acquisition outcome.
    pub ncx: NavigationLoadingOutcome,
}

/// Resolved outcome of one authored navigation href.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NavigationTarget {
    /// The navigation point omitted its href.
    MissingHref,
    /// The href resolved to a resource in this snapshot.
    Resource {
        /// Matching physical resource in this snapshot.
        resource: ResourceOrdinal,
        /// Decoded non-empty fragment, when authored.
        fragment: Option<String>,
    },
    /// The href resolved to a local path with no indexed resource.
    MissingResource {
        /// Resolved local path.
        path: EpubPath,
        /// Decoded non-empty fragment, when authored.
        fragment: Option<String>,
    },
    /// The href resolved to a valid non-local address with no indexed resource.
    UnindexedAddress {
        /// Resolved remote, data, or other external address.
        address: ResourceAddress,
        /// Decoded non-empty fragment, when authored.
        fragment: Option<String>,
    },
    /// The authored href could not be resolved, including against an authored base.
    Invalid,
}

/// One navigation point's authored href and resolved target in deterministic document order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NavigationTargetFacts {
    /// Zero-based navigation-list position.
    pub list_index: u32,
    /// Point indices from the list root to this point.
    pub point_path: Vec<u32>,
    /// Exact authored href, when present.
    pub authored_href: Option<AuthoredHref>,
    /// Resolution against the selected navigation document path and resource inventory.
    pub target: NavigationTarget,
}

/// A navigation document has more lists or points than portable 32-bit positions can address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("navigation positions exceed 32-bit facts indices")]
pub struct NavigationPositionOverflow;

/// Resolves every navigation point's authored href against `resources`.
///
/// Targets are returned in list order and depth-first point order. Resolution reports authored
/// state and does not read the target's content.
///
/// # Errors
///
/// Returns [`NavigationPositionOverflow`] if a list or point position exceeds `u32`.
pub fn navigation_targets(
    document: &NavigationDocument,
    resources: &ResourceIndex,
) -> Result<Vec<NavigationTargetFacts>, NavigationPositionOverflow> {
    let mut output = Vec::new();
    for (list_index, list) in document.lists().iter().enumerate() {
        collect_points(
            list.points(),
            portable_index(list_index)?,
            &mut Vec::new(),
            document.path(),
            document.authored_base(),
            resources,
            &mut output,
        )?;
    }
    Ok(output)
}

fn portable_index(index: usize) -> Result<u32, NavigationPositionOverflow> {
    u32::try_from(index).map_err(|_| NavigationPositionOverflow)
}

fn collect_points(
    points: &[NavigationPoint],
    list_index: u32,
    path: &mut Vec<u32>,
    source: &EpubPath,
    authored_base: Option<&AuthoredHref>,
    resources: &ResourceIndex,
    output: &mut Vec<NavigationTargetFacts>,
) -> Result<(), NavigationPositionOverflow> {
    for (index, point) in points.iter().enumerate() {
        path.push(portable_index(index)?);
        let authored_href = point.authored_href().cloned();
        let target = authored_href
            .as_ref()
            .map_or(NavigationTarget::MissingHref, |href| {
                resolved_target(
                    authored_base.map_or_else(
                        || crate::resource::resolve_href(href, source),
                        |base| {
                            crate::resource::base::resolve_href_with_bases(source, &[base], href)
                        },
                    ),
                    resources,
                )
            });
        output.push(NavigationTargetFacts {
            list_index,
            point_path: path.clone(),
            authored_href,
            target,
        });
        collect_points(
            point.children(),
            list_index,
            path,
            source,
            authored_base,
            resources,
            output,
        )?;
        path.pop();
    }
    Ok(())
}

fn resolved_target(
    resolved: Result<ResolvedHref, InvalidHref>,
    resources: &ResourceIndex,
) -> NavigationTarget {
    let Ok(ResolvedHref {
        address, fragment, ..
    }) = resolved
    else {
        return NavigationTarget::Invalid;
    };
    match resources.resource_at(&address) {
        Some(resource) => NavigationTarget::Resource {
            resource: resource.ordinal(),
            fragment,
        },
        None => match address {
            ResourceAddress::Local(path) => NavigationTarget::MissingResource { path, fragment },
            address => NavigationTarget::UnindexedAddress { address, fragment },
        },
    }
}
