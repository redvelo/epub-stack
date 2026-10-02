//! Authored links, manifest relationships, resolved targets, and XHTML link views.
//!
//! References retain the authored href or ID-reference text and show whether it resolves to a
//! local resource, fragment, remote declaration, missing target, or ambiguous declaration.

use super::PublicationAnalysis;
use crate::content::{FormFact, MediaFact, ScriptFact};
use crate::resource::{AuthoredHref, AuthoredIdRef, EpubPath, ManifestOrdinal, ResourceOrdinal};

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

/// What wrote a link: a resource, or a manifest declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceSource {
    /// A resource containing an authored href.
    Resource(ResourceOrdinal),
    /// A manifest declaration containing an authored ID reference.
    Declaration(ManifestOrdinal),
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
    /// An NCX navigation point target.
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

/// Where a link turned out to point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HrefTarget {
    /// A local resource, with any authored query retained.
    Resource {
        /// The resource it points at.
        resource: ResourceOrdinal,
        /// The query component without the leading `?`.
        query: Option<String>,
    },
    /// A local resource fragment, with fragment existence when it was inspectable.
    Fragment {
        /// The resource it points at.
        resource: ResourceOrdinal,
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
        /// The resource a matching declaration points at, if one does.
        declared_resource: Option<ResourceOrdinal>,
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

/// What a `fallback` or `media-overlay` IDREF turned out to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestTarget {
    /// One matching declaration and its resource, when that declaration resolves to one.
    Declaration {
        /// The declaration it names.
        declaration: ManifestOrdinal,
        /// The resource that declaration points at.
        resource: Option<ResourceOrdinal>,
    },
    /// The authored IDREF is not a valid normalized manifest ID.
    InvalidManifestIdref,
    /// No declaration has the authored ID.
    Missing,
    /// Multiple declarations have the authored ID.
    Ambiguous {
        /// Every declaration it names.
        candidates: Vec<ManifestOrdinal>,
    },
}

/// Source element or CSS construct that carried a reference.
///
/// The source document is identified by the reference's source resource; its use by
/// [`HrefRole`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceContext {
    /// A markup element and attribute.
    Element(ElementAttribute),
    /// A CSS rule or declaration context.
    Css(CssContext),
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

/// One link: where it was written, what it was written as, and where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HrefReference {
    source: ResourceOrdinal,
    declared: AuthoredHref,
    role: HrefRole,
    target: HrefTarget,
    context: ReferenceContext,
}

impl HrefReference {
    /// The resource this link was written in.
    pub fn source(&self) -> ResourceOrdinal {
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

    /// Where it points.
    pub fn target(&self) -> &HrefTarget {
        &self.target
    }

    /// Returns the source construct that carried the href.
    pub fn context(&self) -> &ReferenceContext {
        &self.context
    }
}

/// One `fallback` or `media-overlay` IDREF, and what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestReference {
    source: ManifestOrdinal,
    declared: AuthoredIdRef,
    role: ManifestRole,
    target: ManifestTarget,
}

impl ManifestReference {
    /// The declaration this IDREF was written on.
    pub fn source(&self) -> ManifestOrdinal {
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

    /// Where it points.
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
    /// What wrote this link.
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
    resource: ResourceOrdinal,
    slots: &'a [ReferenceSlot],
    references: &'a [AuthoredReference],
}

impl<'a> XhtmlMediaOccurrence<'a> {
    pub(crate) fn new(
        fact: &'a MediaFact,
        resource: ResourceOrdinal,
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
    /// Iterates every authored link and manifest relationship found by this analysis.
    pub fn references(&self) -> impl Iterator<Item = &AuthoredReference> {
        self.references.iter()
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
    source: impl Into<ResourceOrdinal>,
    declared: AuthoredHref,
    role: HrefRole,
    target: HrefTarget,
    context: ReferenceContext,
) -> ReferenceSlot {
    let slot = ReferenceSlot::new(references.len());
    references.push(AuthoredReference::Href(HrefReference {
        source: source.into(),
        declared,
        role,
        target,
        context,
    }));
    slot
}

pub(crate) fn manifest_reference(
    references: &mut Vec<AuthoredReference>,
    source: impl Into<ManifestOrdinal>,
    declared: AuthoredIdRef,
    role: ManifestRole,
    target: ManifestTarget,
) -> ReferenceSlot {
    let slot = ReferenceSlot::new(references.len());
    references.push(AuthoredReference::Manifest(ManifestReference {
        source: source.into(),
        declared,
        role,
        target,
    }));
    slot
}
