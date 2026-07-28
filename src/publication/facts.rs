//! Owned structural facts for one committed publication state.

use crate::{
    navigation::{Navigation, NavigationPoint},
    package::Package,
    resource::{
        AuthoredHref, EpubPath, ResolvedHref, ResourceAddress, ResourceIndex, ResourceOrdinal,
        facts::ResourceIndexFacts,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
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
    derive(serde::Serialize),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("publication facts exceed portable navigation ordinals")]
/// Failure to represent a checked publication snapshot in portable facts.
pub struct PublicationFactsError;

/// Resolved outcome of one authored navigation href.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "kebab-case")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NavigationTargetOutcomeFacts {
    /// The address identifies a resource in this snapshot.
    Resolved,
    /// The local target is not represented by an indexed resource.
    MissingResource,
    /// The valid non-local address is not represented by an indexed resource.
    UnindexedAddress,
    /// Resolution found multiple resources at the same address.
    AmbiguousAddress,
}

/// Resolved outcome of one authored navigation href.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(
        tag = "state",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NavigationHrefTargetFacts {
    /// The navigation point omitted its href.
    MissingHref,
    /// The href resolved to an address, whether or not that address is in the inventory.
    Address {
        /// Resolved physical address.
        address: ResourceAddress,
        /// Matching physical resource in this snapshot, when indexed.
        resource: Option<ResourceOrdinal>,
        /// Whether resolution selected an indexed resource or stopped at an address-level outcome.
        outcome: NavigationTargetOutcomeFacts,
        /// Decoded non-empty fragment, when authored.
        fragment: Option<String>,
        /// Fragment existence when known; structural facts normally leave this unknown.
        fragment_exists: Option<bool>,
    },
    /// The authored href could not be resolved.
    Invalid,
}

/// One navigation point's authored href and resolved target in deterministic document order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
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
    pub target: NavigationHrefTargetFacts,
}

/// One coherent, owned structural snapshot of a publication.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize),
    serde(rename_all = "camelCase")
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PublicationFacts {
    /// Canonical provider-relative package document path.
    pub package_path: EpubPath,
    /// Complete modeled OPF package structure.
    pub package: Package,
    /// Selected normalized EPUB NAV or NCX structure.
    pub navigation: Navigation,
    /// Portable resource topology aligned with the package structure.
    pub resource_index: ResourceIndexFacts,
    /// Navigation acquisition observations aligned with the same committed state.
    pub navigation_loading: NavigationLoadingFacts,
    /// Navigation targets in list order and depth-first point order.
    pub navigation_targets: Vec<NavigationTargetFacts>,
}

pub(crate) fn navigation_target_facts(
    navigation: &Navigation,
    resources: &ResourceIndex,
) -> Result<Vec<NavigationTargetFacts>, PublicationFactsError> {
    let Some(document) = navigation.document() else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    for (list_index, list) in document.lists().iter().enumerate() {
        collect_points(
            list.points(),
            portable_index(list_index)?,
            &mut Vec::new(),
            document.path(),
            resources,
            &mut output,
        )?;
    }
    Ok(output)
}

fn collect_points(
    points: &[NavigationPoint],
    list_index: u32,
    path: &mut Vec<u32>,
    source: &EpubPath,
    resources: &ResourceIndex,
    output: &mut Vec<NavigationTargetFacts>,
) -> Result<(), PublicationFactsError> {
    for (index, point) in points.iter().enumerate() {
        path.push(portable_index(index)?);
        let authored_href = point.authored_href().cloned();
        let target =
            authored_href
                .as_ref()
                .map_or(NavigationHrefTargetFacts::MissingHref, |href| {
                    resolved_target(
                        resources.resolve_href_from(href.as_str(), source),
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
            resources,
            output,
        )?;
        path.pop();
    }
    Ok(())
}

fn portable_index(index: usize) -> Result<u32, PublicationFactsError> {
    u32::try_from(index).map_err(|_| PublicationFactsError)
}

fn resolved_target(resolved: ResolvedHref, resources: &ResourceIndex) -> NavigationHrefTargetFacts {
    let (address, fragment, fragment_exists, forced_outcome) = match resolved {
        ResolvedHref::Resource(address) => (address, None, None, None),
        ResolvedHref::Fragment {
            resource,
            fragment,
            exists,
        } => (resource, Some(fragment), exists, None),
        ResolvedHref::RemoteUrl(url) => (ResourceAddress::Remote(url), None, None, None),
        ResolvedHref::Data(url) => (ResourceAddress::Data(url), None, None, None),
        ResolvedHref::External(url) => (ResourceAddress::External(url), None, None, None),
        ResolvedHref::MissingPath(path) => (
            ResourceAddress::Local(path),
            None,
            None,
            Some(NavigationTargetOutcomeFacts::MissingResource),
        ),
        ResolvedHref::AmbiguousAddress { address, .. } => (
            address,
            None,
            None,
            Some(NavigationTargetOutcomeFacts::AmbiguousAddress),
        ),
        ResolvedHref::Invalid(_) | ResolvedHref::MissingManifestId(_) => {
            return NavigationHrefTargetFacts::Invalid;
        }
    };
    let resource = resources
        .resources_at(&address)
        .next()
        .map(|resource| resource.ordinal());
    let outcome = forced_outcome.unwrap_or_else(|| {
        if resource.is_some() {
            NavigationTargetOutcomeFacts::Resolved
        } else if matches!(address, ResourceAddress::Local(_)) {
            NavigationTargetOutcomeFacts::MissingResource
        } else {
            NavigationTargetOutcomeFacts::UnindexedAddress
        }
    });
    NavigationHrefTargetFacts::Address {
        address,
        resource,
        outcome,
        fragment,
        fragment_exists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_navigation_index_rejects_overflow() {
        assert_eq!(portable_index(u32::MAX as usize), Ok(u32::MAX));
        if usize::BITS > 32 {
            assert_eq!(
                portable_index(u32::MAX as usize + 1),
                Err(PublicationFactsError)
            );
        }
    }
}
