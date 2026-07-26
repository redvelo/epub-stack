//! Authored links, manifest relationships, resolved targets, and XHTML link views.
//!
//! References retain the authored href or ID-reference text and show whether it resolves to a
//! local resource, fragment, remote declaration, missing target, or ambiguous declaration.
//! Resource and declaration keys belong to the analysis snapshot and must not be reused with a
//! later analysis.

use super::PublicationAnalysis;
use crate::content::{ContentFacts, FormFact, MediaFact, ScriptFact};
use crate::resource::{
    AuthoredHref, AuthoredIdRef, EpubPath, IndexKeyError, ManifestKey, ResourceKey,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ReferenceSlot(usize);

impl ReferenceSlot {
    pub(crate) fn new(slot: usize) -> Self {
        Self(slot)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// The snapshot-local owner of an authored reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceSource {
    /// A resource containing an authored href.
    Resource(ResourceKey),
    /// A manifest declaration containing an authored ID reference.
    Declaration(ManifestKey),
}

/// The semantic use of an authored href.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HrefRole {
    /// A table-of-contents navigation target.
    Toc,
    /// A page-list navigation target.
    PageList,
    /// A landmarks navigation target.
    Landmark,
    /// A package reference to an NCX document.
    Ncx,
    /// A legacy OPF guide target.
    Guide,
    /// A metadata link target.
    Metadata,
    /// A package collection link target.
    Collection,
    /// A content hyperlink.
    Hyperlink,
    /// A linked stylesheet.
    Stylesheet,
    /// A form submission target.
    FormAction,
    /// Image content.
    Image,
    /// External script content.
    Script,
    /// Audio content.
    Audio,
    /// Video content.
    Video,
    /// A media source candidate.
    Source,
    /// A timed-text track.
    Track,
    /// A video poster image.
    Poster,
    /// Object data.
    Object,
    /// Embedded content.
    Embed,
    /// An inline frame document.
    Iframe,
    /// An SVG resource reference.
    Svg,
    /// The text target of a SMIL node.
    SmilText,
    /// The audio target of a SMIL node.
    SmilAudio,
    /// A CSS import.
    CssImport,
    /// A CSS `url()` value.
    CssUrl,
    /// Font content referenced by CSS.
    Font,
}

/// The semantic use of an authored manifest ID reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestRole {
    /// A manifest fallback chain edge.
    Fallback,
    /// A manifest media-overlay association.
    MediaOverlay,
}

/// Resolution of an authored href against one analysis snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HrefTarget {
    /// A local resource, with any authored query retained.
    Resource {
        /// The resolved snapshot-local resource key.
        resource: ResourceKey,
        /// The query component without the leading `?`.
        query: Option<String>,
    },
    /// A local resource fragment, with fragment existence when it was inspectable.
    Fragment {
        /// The resolved snapshot-local resource key.
        resource: ResourceKey,
        /// The query component without the leading `?`.
        query: Option<String>,
        /// The fragment component without the leading `#`.
        fragment: String,
        /// Whether the fragment was found, or `None` when fragment discovery was incomplete.
        exists: Option<bool>,
    },
    /// An absolute remote URL, optionally matched to a remote manifest declaration.
    Remote {
        /// The authored absolute href.
        href: String,
        /// The snapshot-local resource represented by a matching declaration.
        declared_resource: Option<ResourceKey>,
    },
    /// A `data:` URL retained as authored.
    Data(String),
    /// An absolute non-remote URI retained as authored.
    External(String),
    /// A valid local path that is absent from the resource index.
    MissingLocal(EpubPath),
    /// An href whose syntax could not be resolved.
    Invalid(AuthoredHref),
}

/// Resolution of an authored manifest ID reference in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestTarget {
    /// One matching declaration and its resource, when that declaration resolves to one.
    Declaration {
        /// The matching snapshot-local declaration key.
        declaration: ManifestKey,
        /// The declaration's snapshot-local resource target.
        resource: Option<ResourceKey>,
    },
    /// No declaration has the authored ID.
    Missing,
    /// Multiple declarations have the authored ID.
    Ambiguous {
        /// All matching snapshot-local declaration keys.
        candidates: Vec<ManifestKey>,
    },
}

/// Source element or CSS construct that carried a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceContext {
    /// An OPF element and attribute.
    Package(ElementAttribute),
    /// An EPUB NAV or NCX element and attribute.
    Navigation(ElementAttribute),
    /// An XHTML element and attribute.
    Xhtml(ElementAttribute),
    /// A SMIL element and attribute.
    Smil(ElementAttribute),
    /// A CSS rule or declaration context.
    Css(CssContext),
    /// An SVG element and attribute.
    Svg(ElementAttribute),
}

/// The source element and attribute names for an authored href.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementAttribute {
    element: String,
    attribute: String,
}

impl ElementAttribute {
    /// Returns the source element's local name.
    pub fn element(&self) -> &str {
        &self.element
    }

    /// Returns the authored attribute name.
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    pub(crate) fn new(element: impl Into<String>, attribute: impl Into<String>) -> Self {
        Self {
            element: element.into(),
            attribute: attribute.into(),
        }
    }
}

/// The CSS location that carried an authored reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssContext {
    at_rule: Option<String>,
    property: Option<String>,
}

impl CssContext {
    /// Returns the at-rule name when the reference came from one.
    pub fn at_rule(&self) -> Option<&str> {
        self.at_rule.as_deref()
    }

    /// Returns the property name when the reference came from a declaration.
    pub fn property(&self) -> Option<&str> {
        self.property.as_deref()
    }

    pub(crate) fn new(at_rule: Option<String>, property: Option<String>) -> Self {
        Self { at_rule, property }
    }
}

/// An authored href together with its role, source, and snapshot resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HrefReference {
    source: ResourceKey,
    declared: AuthoredHref,
    role: HrefRole,
    target: HrefTarget,
    context: ReferenceContext,
}

impl HrefReference {
    /// Returns the snapshot-local resource containing the href.
    pub fn source(&self) -> ResourceKey {
        self.source
    }

    /// Returns the exact authored href.
    pub fn declared(&self) -> &AuthoredHref {
        &self.declared
    }

    /// Returns how the source used the href.
    pub fn role(&self) -> HrefRole {
        self.role
    }

    /// Returns resolution against this analysis snapshot.
    pub fn target(&self) -> &HrefTarget {
        &self.target
    }

    /// Returns the source construct that carried the href.
    pub fn context(&self) -> &ReferenceContext {
        &self.context
    }
}

/// An authored manifest ID reference and its snapshot resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestReference {
    source: ManifestKey,
    declared: AuthoredIdRef,
    role: ManifestRole,
    target: ManifestTarget,
}

impl ManifestReference {
    /// Returns the snapshot-local declaration containing the ID reference.
    pub fn source(&self) -> ManifestKey {
        self.source
    }

    /// Returns the exact authored ID reference.
    pub fn declared(&self) -> &AuthoredIdRef {
        &self.declared
    }

    /// Returns how the declaration used the ID reference.
    pub fn role(&self) -> ManifestRole {
        self.role
    }

    /// Returns resolution against this analysis snapshot.
    pub fn target(&self) -> &ManifestTarget {
        &self.target
    }
}

/// An authored href or manifest relationship and its analyzed target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoredReference {
    /// A URI-valued reference from a resource.
    Href(HrefReference),
    /// An ID-valued relationship from a manifest declaration.
    Manifest(ManifestReference),
}

impl AuthoredReference {
    /// Returns the snapshot-local owner of the authored reference.
    pub fn source(&self) -> ReferenceSource {
        match self {
            Self::Href(reference) => ReferenceSource::Resource(reference.source()),
            Self::Manifest(reference) => ReferenceSource::Declaration(reference.source()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct XhtmlReferenceIndex {
    pub(crate) media: Vec<Vec<ReferenceSlot>>,
    pub(crate) forms: Vec<Option<ReferenceSlot>>,
    pub(crate) scripts: Vec<Option<ReferenceSlot>>,
}

/// One XHTML media element together with its authored source links.
#[derive(Debug, Clone, Copy)]
pub struct XhtmlMediaOccurrence<'a> {
    fact: &'a MediaFact,
    resource: ResourceKey,
    slots: &'a [ReferenceSlot],
    references: &'a [AuthoredReference],
}

impl<'a> XhtmlMediaOccurrence<'a> {
    pub(crate) fn new(
        fact: &'a MediaFact,
        resource: ResourceKey,
        slots: &'a [ReferenceSlot],
        references: &'a [AuthoredReference],
    ) -> Self {
        Self {
            fact,
            resource,
            slots,
            references,
        }
    }

    /// Returns the extracted media fact.
    pub fn fact(self) -> &'a MediaFact {
        self.fact
    }

    /// Iterates its authored `src` then `srcset` references in source order.
    pub fn references(self) -> impl Iterator<Item = &'a HrefReference> {
        self.slots
            .iter()
            .filter_map(move |slot| match self.references.get(slot.index()) {
                Some(AuthoredReference::Href(reference)) if reference.source() == self.resource => {
                    Some(reference)
                }
                _ => None,
            })
    }
}

/// One XHTML form or control together with its optional submission link.
#[derive(Debug, Clone, Copy)]
pub struct XhtmlFormOccurrence<'a> {
    fact: &'a FormFact,
    reference: Option<&'a HrefReference>,
}

impl<'a> XhtmlFormOccurrence<'a> {
    pub(crate) fn new(fact: &'a FormFact, reference: Option<&'a HrefReference>) -> Self {
        Self { fact, reference }
    }

    /// Returns the extracted form fact.
    pub fn fact(self) -> &'a FormFact {
        self.fact
    }

    /// Returns the form or control submission reference, when authored.
    pub fn reference(self) -> Option<&'a HrefReference> {
        self.reference
    }
}

/// One XHTML script together with its optional external source link.
#[derive(Debug, Clone, Copy)]
pub struct XhtmlScriptOccurrence<'a> {
    fact: &'a ScriptFact,
    reference: Option<&'a HrefReference>,
}

impl PublicationAnalysis {
    /// Returns XHTML media elements with their authored `src` and `srcset` links.
    ///
    /// Returns `Ok(None)` when the resource has no available XHTML facts.
    pub fn xhtml_media(
        &self,
        resource: ResourceKey,
    ) -> Result<Option<impl Iterator<Item = XhtmlMediaOccurrence<'_>>>, IndexKeyError> {
        let facts = self
            .content_for(resource)?
            .value()
            .and_then(ContentFacts::as_xhtml);
        Ok(facts.map(move |facts| {
            facts.media().iter().enumerate().map(move |(index, fact)| {
                let slots = self
                    .xhtml_references
                    .get(&resource)
                    .and_then(|references| references.media.get(index))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                XhtmlMediaOccurrence::new(fact, resource, slots, &self.references)
            })
        }))
    }

    /// Returns XHTML forms and controls with their optional submission links.
    ///
    /// Returns `Ok(None)` when the resource has no available XHTML facts.
    pub fn xhtml_forms(
        &self,
        resource: ResourceKey,
    ) -> Result<Option<impl Iterator<Item = XhtmlFormOccurrence<'_>>>, IndexKeyError> {
        let facts = self
            .content_for(resource)?
            .value()
            .and_then(ContentFacts::as_xhtml);
        Ok(facts.map(move |facts| {
            facts.forms().iter().enumerate().map(move |(index, fact)| {
                let slot = self
                    .xhtml_references
                    .get(&resource)
                    .and_then(|references| references.forms.get(index))
                    .copied()
                    .flatten();
                XhtmlFormOccurrence::new(
                    fact,
                    slot.and_then(|slot| self.xhtml_reference(resource, slot)),
                )
            })
        }))
    }

    /// Returns XHTML scripts with their optional external source links.
    ///
    /// Returns `Ok(None)` when the resource has no available XHTML facts.
    pub fn xhtml_scripts(
        &self,
        resource: ResourceKey,
    ) -> Result<Option<impl Iterator<Item = XhtmlScriptOccurrence<'_>>>, IndexKeyError> {
        let facts = self
            .content_for(resource)?
            .value()
            .and_then(ContentFacts::as_xhtml);
        Ok(facts.map(move |facts| {
            facts
                .scripts()
                .iter()
                .enumerate()
                .map(move |(index, fact)| {
                    let slot = self
                        .xhtml_references
                        .get(&resource)
                        .and_then(|references| references.scripts.get(index))
                        .copied()
                        .flatten();
                    XhtmlScriptOccurrence::new(
                        fact,
                        slot.and_then(|slot| self.xhtml_reference(resource, slot)),
                    )
                })
        }))
    }

    /// Iterates every authored link and manifest relationship found by this analysis.
    pub fn references(&self) -> impl Iterator<Item = &AuthoredReference> {
        self.references.iter()
    }

    fn xhtml_reference(
        &self,
        resource: ResourceKey,
        slot: ReferenceSlot,
    ) -> Option<&HrefReference> {
        match self.references.get(slot.index()) {
            Some(AuthoredReference::Href(reference)) if reference.source() == resource => {
                Some(reference)
            }
            _ => None,
        }
    }
}

impl<'a> XhtmlScriptOccurrence<'a> {
    pub(crate) fn new(fact: &'a ScriptFact, reference: Option<&'a HrefReference>) -> Self {
        Self { fact, reference }
    }

    /// Returns the extracted script fact.
    pub fn fact(self) -> &'a ScriptFact {
        self.fact
    }

    /// Returns the external script source reference, when authored.
    pub fn reference(self) -> Option<&'a HrefReference> {
        self.reference
    }
}

pub(crate) fn href_reference(
    references: &mut Vec<AuthoredReference>,
    source: ResourceKey,
    declared: AuthoredHref,
    role: HrefRole,
    target: HrefTarget,
    context: ReferenceContext,
) -> ReferenceSlot {
    let slot = ReferenceSlot::new(references.len());
    references.push(AuthoredReference::Href(HrefReference {
        source,
        declared,
        role,
        target,
        context,
    }));
    slot
}

pub(crate) fn manifest_reference(
    references: &mut Vec<AuthoredReference>,
    source: ManifestKey,
    declared: AuthoredIdRef,
    role: ManifestRole,
    target: ManifestTarget,
) -> ReferenceSlot {
    let slot = ReferenceSlot::new(references.len());
    references.push(AuthoredReference::Manifest(ManifestReference {
        source,
        declared,
        role,
        target,
    }));
    slot
}
