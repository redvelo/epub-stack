use super::{
    AccessibilityCertifierReport, AccessibilityClaim, AccessibilityConformance,
    AccessibilityMetadata, AccessibilityMetadataValue, AccessibilityProperty,
    EpubAccessibilityVersion, PageBreakSourceTerm, WcagLevel, WcagVersion,
};
use crate::analysis::reference::ReferenceSlot;
use crate::package::Package;

pub(super) fn collect_metadata(
    package: &Package,
    package_link_references: &[Option<ReferenceSlot>],
) -> (AccessibilityMetadata, Vec<AccessibilityClaim>) {
    let mut metadata = AccessibilityMetadata::default();
    let mut claims = Vec::new();
    for meta in package.metadata().meta() {
        let Some(property) = meta.property().map(|property| property.as_str()) else {
            continue;
        };
        let property = resolve_accessibility_property(package, property);
        if property == Some(AccessibilityMetadataTerm::ConformsTo) {
            claims.push(AccessibilityClaim {
                conformance: meta
                    .content()
                    .and_then(|value| parse_conformance(value.as_str())),
                authored: meta.clone(),
            });
            continue;
        }
        let Some(property) = property.and_then(AccessibilityMetadataTerm::property) else {
            continue;
        };
        metadata.values.push(AccessibilityMetadataValue {
            property,
            authored: meta.clone(),
        });
    }
    metadata.certifier_reports = package
        .metadata()
        .link()
        .iter()
        .enumerate()
        .filter(|(_, link)| {
            link.rel().is_some_and(|rel| {
                resolve_accessibility_property(package, rel.as_str())
                    == Some(AccessibilityMetadataTerm::CertifierReport)
            })
        })
        .map(|(index, authored)| AccessibilityCertifierReport {
            authored: authored.clone(),
            reference: package_link_references.get(index).copied().flatten(),
        })
        .collect();
    (metadata, claims)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessibilityMetadataTerm {
    AccessMode,
    AccessModeSufficient,
    Feature,
    Hazard,
    Summary,
    ContactEmail,
    CertifiedBy,
    CertifierCredential,
    CertifierReport,
    PageBreakSource,
    LegacyPageSource,
    ConformsTo,
}

impl AccessibilityMetadataTerm {
    fn property(self) -> Option<AccessibilityProperty> {
        Some(match self {
            Self::AccessMode => AccessibilityProperty::AccessMode,
            Self::AccessModeSufficient => AccessibilityProperty::AccessModeSufficient,
            Self::Feature => AccessibilityProperty::Feature,
            Self::Hazard => AccessibilityProperty::Hazard,
            Self::Summary => AccessibilityProperty::Summary,
            Self::ContactEmail => AccessibilityProperty::ContactEmail,
            Self::CertifiedBy => AccessibilityProperty::CertifiedBy,
            Self::CertifierCredential => AccessibilityProperty::CertifierCredential,
            Self::PageBreakSource => {
                AccessibilityProperty::PageBreakSource(PageBreakSourceTerm::Current)
            }
            Self::LegacyPageSource => {
                AccessibilityProperty::PageBreakSource(PageBreakSourceTerm::LegacyPageSource)
            }
            Self::CertifierReport | Self::ConformsTo => return None,
        })
    }
}

fn resolve_accessibility_property(
    package: &Package,
    property: &str,
) -> Option<AccessibilityMetadataTerm> {
    let (prefix, local) = property.split_once(':').unwrap_or(("", property));
    let vocabulary = match prefix {
        "schema" => Some("schema"),
        "dcterms" => Some("dcterms"),
        "a11y" => Some("a11y"),
        "" => None,
        custom => package
            .prefix()
            .and_then(|prefixes| vocabulary_for_prefix(prefixes.as_str(), custom)),
    };
    match (vocabulary, local) {
        (Some("schema"), "accessMode") => Some(AccessibilityMetadataTerm::AccessMode),
        (Some("schema"), "accessModeSufficient") => {
            Some(AccessibilityMetadataTerm::AccessModeSufficient)
        }
        (Some("schema"), "accessibilityFeature") => Some(AccessibilityMetadataTerm::Feature),
        (Some("schema"), "accessibilityHazard") => Some(AccessibilityMetadataTerm::Hazard),
        (Some("schema"), "accessibilitySummary") => Some(AccessibilityMetadataTerm::Summary),
        (Some("a11y"), "contactEmail") => Some(AccessibilityMetadataTerm::ContactEmail),
        (Some("a11y"), "certifiedBy") => Some(AccessibilityMetadataTerm::CertifiedBy),
        (Some("a11y"), "certifierCredential") => {
            Some(AccessibilityMetadataTerm::CertifierCredential)
        }
        (Some("a11y"), "certifierReport") => Some(AccessibilityMetadataTerm::CertifierReport),
        (None, "pageBreakSource") => Some(AccessibilityMetadataTerm::PageBreakSource),
        (None, "page-source") => Some(AccessibilityMetadataTerm::LegacyPageSource),
        (Some("dcterms"), "conformsTo") => Some(AccessibilityMetadataTerm::ConformsTo),
        _ => None,
    }
}

fn vocabulary_for_prefix<'a>(prefixes: &'a str, wanted: &str) -> Option<&'a str> {
    let mut tokens = prefixes.split_whitespace();
    while let (Some(prefix), Some(vocabulary)) = (tokens.next(), tokens.next()) {
        if prefix.strip_suffix(':') != Some(wanted) {
            continue;
        }
        return match vocabulary {
            "http://schema.org/" | "https://schema.org/" => Some("schema"),
            "http://purl.org/dc/terms/" => Some("dcterms"),
            "http://www.idpf.org/epub/vocab/package/a11y/#" => Some("a11y"),
            _ => None,
        };
    }
    None
}

fn parse_conformance(value: &str) -> Option<AccessibilityConformance> {
    let rest = value.strip_prefix("EPUB Accessibility ")?;
    let (epub, rest) = rest.split_once(" - WCAG ")?;
    let (wcag, level) = rest.split_once(" Level ")?;
    Some(AccessibilityConformance {
        epub_accessibility: match epub {
            "1.0" => EpubAccessibilityVersion::V1_0,
            "1.1" => EpubAccessibilityVersion::V1_1,
            "1.2" => EpubAccessibilityVersion::V1_2,
            _ => return None,
        },
        wcag: match wcag {
            "2.0" => WcagVersion::V2_0,
            "2.1" => WcagVersion::V2_1,
            "2.2" => WcagVersion::V2_2,
            _ => return None,
        },
        level: match level {
            "A" => WcagLevel::A,
            "AA" => WcagLevel::Aa,
            "AAA" => WcagLevel::Aaa,
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{AccessMode, AccessibilityFeature, AccessibilityHazard};

    #[test]
    fn parses_exact_accessibility_conformance_claims() {
        let claim = parse_conformance("EPUB Accessibility 1.2 - WCAG 2.2 Level AA").unwrap();
        assert_eq!(claim.epub_accessibility(), EpubAccessibilityVersion::V1_2);
        assert_eq!(claim.wcag(), WcagVersion::V2_2);
        assert_eq!(claim.level(), WcagLevel::Aa);
        assert!(parse_conformance("WCAG 2.2 AA").is_none());
    }

    #[test]
    fn recognizes_current_and_legacy_page_break_source_terms() {
        assert_eq!(
            AccessibilityMetadataTerm::PageBreakSource.property(),
            Some(AccessibilityProperty::PageBreakSource(
                PageBreakSourceTerm::Current
            ))
        );
        assert_eq!(
            AccessibilityMetadataTerm::LegacyPageSource.property(),
            Some(AccessibilityProperty::PageBreakSource(
                PageBreakSourceTerm::LegacyPageSource
            ))
        );
    }

    #[test]
    fn recognizes_current_accessibility_feature_and_hazard_terms() {
        for value in [
            "pageNavigation",
            "taggedPDF",
            "closedCaptions",
            "openCaptions",
            "ChemML",
            "latex-chemistry",
            "MathML-chemistry",
            "fullRubyAnnotations",
            "horizontalWriting",
            "verticalWriting",
            "withAdditionalWordSegmentation",
            "withoutAdditionalWordSegmentation",
            "unknown",
        ] {
            assert!(AccessibilityFeature::parse(value).is_some(), "{value}");
        }
        for value in [
            "none",
            "unknownFlashingHazard",
            "unknownMotionSimulationHazard",
            "unknownSoundHazard",
        ] {
            assert!(AccessibilityHazard::parse(value).is_some(), "{value}");
        }
    }

    #[test]
    fn access_mode_tokens_keep_unrecognized_authored_spellings() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf">
                <metadata>
                    <meta property="schema:accessModeSufficient">textual unknownMode</meta>
                </metadata>
            </package>"#,
        )
        .unwrap();

        let (metadata, _) = collect_metadata(&package, &[None, None]);
        let tokens = metadata
            .values_of(AccessibilityProperty::AccessModeSufficient)
            .next()
            .map(|value| {
                value
                    .access_modes()
                    .map(|token| (token.as_str().to_string(), token.known_value()))
                    .collect::<Vec<_>>()
            })
            .unwrap();

        assert_eq!(
            tokens,
            [
                ("textual".to_string(), Some(AccessMode::Textual)),
                ("unknownMode".to_string(), None),
            ]
        );
    }

    #[test]
    fn resolves_custom_accessibility_vocab_prefixes_case_sensitively() {
        let package = Package::parse(
            r#"<package xmlns="http://www.idpf.org/2007/opf" prefix="s: https://schema.org/ d: http://purl.org/dc/terms/ ax: http://www.idpf.org/epub/vocab/package/a11y/#">
                <metadata>
                    <meta property="s:accessibilityFeature">alternativeText</meta>
                    <meta property="s:accessModeSufficient">textual, visual</meta>
                    <meta property="s:accessibilityHazard">noFlashingHazard</meta>
                    <meta property="s:AccessibilityFeature">ignored</meta>
                    <meta property="accessibilityFeature">also ignored</meta>
                    <meta property="d:conformsTo">EPUB Accessibility 1.2 - WCAG 2.2 Level AA</meta>
                    <link rel="a11y:certifierReport" href="reserved-report.html"/>
                    <link rel="ax:certifierReport" href="custom-report.html"/>
                </metadata>
            </package>"#,
        )
        .unwrap();

        let (metadata, claims) = collect_metadata(&package, &[None, None]);
        assert_eq!(metadata.values().count(), 3);
        assert_eq!(
            metadata
                .values_of(AccessibilityProperty::Feature)
                .next()
                .and_then(AccessibilityMetadataValue::feature),
            Some(AccessibilityFeature::AlternativeText)
        );
        assert_eq!(
            metadata
                .values_of(AccessibilityProperty::AccessModeSufficient)
                .next()
                .map(|value| {
                    value
                        .access_modes()
                        .map(|token| token.known_value())
                        .collect::<Vec<_>>()
                }),
            Some(vec![Some(AccessMode::Textual), Some(AccessMode::Visual)])
        );
        assert_eq!(
            metadata
                .values_of(AccessibilityProperty::Hazard)
                .next()
                .and_then(AccessibilityMetadataValue::hazard),
            Some(AccessibilityHazard::NoFlashingHazard)
        );
        assert_eq!(metadata.certifier_reports().count(), 2);
        assert_eq!(claims.len(), 1);
    }
}
