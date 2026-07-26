//! Parse, build, exchange, and resolve EPUB Annotations 1.0 data.
//!
//! Use [`AnnotationSet::parse_json`] to import annotation JSON, the builders to create annotations,
//! [`AnnotationBundle`] to exchange a set with audiovisual body resources, and
//! [`PublicationAnalysis::resolve_annotation_target`](crate::analysis::PublicationAnalysis::resolve_annotation_target)
//! to resolve targets against analyzed publication content.
//!
//! Import accepts partial annotation objects after requiring a top-level JSON object. Typed
//! accessors return recognized values, while `extra()` maps retain many malformed and unknown
//! members for normalized export. Export does not retain JSON member order, whitespace, malformed
//! array entries, or every malformed shape. Selector refinements beyond
//! [`MAX_SELECTOR_NESTING_DEPTH`] are discarded.

use crate::content::text::TextRange;
use crate::resource::EpubPath;
use crate::resource::MediaType;
use crate::resource::provider::{ProviderReadError, ResourceProviderIndexError};
use crate::resource::{AuthoredHref, ParsedHref, parse_href};

use oxilangtag::LanguageTag;
use serde_json::{Map, Value};
use std::collections::HashSet;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

mod bundle;
mod resolution;

pub use bundle::{AnnotationBundle, AnnotationBundleError, AnnotationResource};
pub(crate) use bundle::{
    MAX_ANNOTATIONS_JSON_BYTES, MAX_ARCHIVE_ENTRIES, MAX_ARCHIVE_RESOURCE_BYTES,
    MAX_ARCHIVE_UNCOMPRESSED_BYTES, normalize_annotation_resource_path,
};
pub use resolution::{AnnotationResolution, AnnotationSourceState, HostRequirement};

/// Stores JSON members that are not represented by typed annotation fields.
pub type JsonObject = Map<String, Value>;

/// Limits selector trees to this many levels, counting a top-level selector as level one.
///
/// Builders reject deeper trees. Import retains this many levels and discards deeper `refinedBy`
/// content.
pub const MAX_SELECTOR_NESTING_DEPTH: usize = 128;

const CONTEXT: &str = "@context";
const ID: &str = "id";
const TYPE: &str = "type";
const ANNOTATION_SET: &str = "AnnotationSet";
const ANNOTATION: &str = "Annotation";
const EPUB_ANNOTATIONS_CONTEXT: &str = "https://www.w3.org/ns/epub-anno.jsonld";
const TARGET: &str = "target";
const SOURCE: &str = "source";
const SELECTOR: &str = "selector";
const BODY: &str = "body";
const CREATED: &str = "created";
const MODIFIED: &str = "modified";
const CREATOR: &str = "creator";
const MOTIVATION: &str = "motivation";
const ITEMS: &str = "items";
const ABOUT: &str = "about";
const GENERATOR: &str = "generator";
const GENERATED: &str = "generated";
const VALUE: &str = "value";
const CONFORMS_TO: &str = "conformsTo";
const REFINED_BY: &str = "refinedBy";
const START: &str = "start";
const END: &str = "end";
const FORMAT: &str = "format";
const COLOR: &str = "color";
const HIGHLIGHT: &str = "highlight";
const TAGS: &str = "tags";
const NAME: &str = "name";
const HOMEPAGE: &str = "homepage";
const META: &str = "meta";

/// Reports a failure to decode or encode annotation JSON.
#[derive(Debug, thiserror::Error)]
pub enum AnnotationError {
    /// JSON syntax or serialization failed.
    #[error("JSON error: {source}")]
    Json {
        /// The underlying JSON error.
        #[from]
        source: serde_json::Error,
    },
    /// The top-level value was not the required JSON object.
    #[error("expected a JSON object for {kind}")]
    ExpectedObject {
        /// Description of the object that was expected.
        kind: &'static str,
    },
    /// Bundle annotation bytes were not UTF-8.
    #[error("annotation JSON is not valid UTF-8: {source}")]
    Utf8 {
        /// The UTF-8 decoding error.
        source: std::str::Utf8Error,
    },
}

/// Reports why typed annotation data could not be built or changed.
#[derive(Debug, thiserror::Error)]
pub enum AnnotationModelError {
    /// A required non-whitespace field was empty.
    #[error("annotation {field} must not be empty")]
    EmptyField {
        /// Model field name.
        field: &'static str,
    },
    /// A required field was absent or could not be recovered.
    #[error("annotation {field} is required")]
    MissingField {
        /// Model field name.
        field: &'static str,
    },
    /// A field was supplied for an incompatible model kind.
    #[error("annotation {field} is not applicable to {context}")]
    FieldNotApplicable {
        /// Model field name.
        field: &'static str,
        /// Kind for which the field is not applicable.
        context: &'static str,
    },
    /// A target source was not an unfragmented local publication path.
    #[error("annotation target source must be an unfragmented local publication path: {value}")]
    InvalidTargetSource {
        /// Rejected authored source.
        value: String,
    },
    /// A detached bundle resource path was unsafe or invalid.
    #[error("annotation resource path is invalid: {path}")]
    InvalidResourcePath {
        /// Rejected path.
        path: String,
    },
    /// A body's media type did not agree with its body kind.
    #[error("{body_type} annotation body format does not match {format}")]
    BodyFormatMismatch {
        /// Canonical annotation body type.
        body_type: &'static str,
        /// Rejected media type string.
        format: String,
    },
    /// A timestamp could not be formatted as RFC 3339.
    #[error("annotation {field} timestamp cannot be represented as RFC 3339")]
    TimestampNotRepresentable {
        /// Timestamp field name.
        field: &'static str,
    },
    /// Two annotations in a set had the same identifier.
    #[error("annotation id is duplicated within the annotation set: {id}")]
    DuplicateAnnotationId {
        /// Duplicated identifier.
        id: String,
    },
    /// No annotation had the requested identifier.
    #[error("annotation id was not found in the annotation set: {id}")]
    AnnotationNotFound {
        /// Requested identifier.
        id: String,
    },
    /// More than one recovered annotation had the requested identifier.
    #[error("annotation id is ambiguous within the annotation set: {id}")]
    AmbiguousAnnotationId {
        /// Ambiguous identifier.
        id: String,
    },
    /// A field value violated its typed invariant.
    #[error("annotation {field} has an invalid value: {value}")]
    InvalidField {
        /// Model field name.
        field: &'static str,
        /// Rejected value or a description of it.
        value: String,
    },
    /// `modified` preceded `created`.
    #[error("annotation modified timestamp must not be earlier than created")]
    ModifiedBeforeCreated,
    /// A selector tree exceeded [`MAX_SELECTOR_NESTING_DEPTH`].
    #[error("annotation selector nesting depth exceeds the maximum of {max}")]
    SelectorNestingDepthExceeded {
        /// Maximum accepted depth.
        max: usize,
    },
}

/// Reports why embedded annotations could not be loaded from a publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmbeddedAnnotationsError {
    /// The publication resource inventory could not be built.
    #[error("could not index resources for embedded annotations: {source}")]
    ProviderIndex {
        /// Provider indexing failure.
        #[source]
        source: ResourceProviderIndexError,
    },
    /// An embedded annotation resource could not be read.
    #[error("could not read embedded annotation resource {path}: {source}")]
    ResourceRead {
        /// Publication path that could not be read.
        path: EpubPath,
        /// Provider read failure.
        #[source]
        source: ProviderReadError,
    },
    /// Embedded annotation JSON could not be decoded.
    #[error(transparent)]
    Annotation {
        /// Annotation JSON failure.
        #[from]
        source: AnnotationError,
    },
    /// The embedded annotation archive was invalid.
    #[error(transparent)]
    Bundle {
        /// Bundle failure.
        #[from]
        source: AnnotationBundleError,
    },
}

/// Holds annotations for JSON interchange, bundle exchange, and target resolution.
///
/// Parsed sets may be partial. Optional accessors expose recovered typed values; use
/// [`AnnotationSet::extra`] to inspect other retained top-level members.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationSet {
    context: Option<Value>,
    type_valid: bool,
    id: Option<String>,
    generator: Option<AnnotationGenerator>,
    generated: Option<String>,
    about: Option<AnnotationAbout>,
    items: Vec<Annotation>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationSet {
    /// Builds a complete set, validating every annotation and requiring unique annotation IDs.
    #[builder]
    pub fn new(
        id: String,
        about: AnnotationAbout,
        #[builder(default)] items: Vec<Annotation>,
        generator: Option<AnnotationGenerator>,
        generated: Option<OffsetDateTime>,
    ) -> Result<Self, AnnotationModelError> {
        require_valid_id(&id, "set id")?;
        ensure_valid_about(&about)?;
        if let Some(generator) = &generator {
            ensure_valid_generator(generator)?;
        }
        for annotation in &items {
            ensure_valid_annotation(annotation)?;
        }
        ensure_unique_annotation_ids(&items)?;
        Ok(Self {
            context: Some(Value::String(EPUB_ANNOTATIONS_CONTEXT.to_string())),
            type_valid: true,
            id: Some(id),
            generator,
            generated: generated
                .map(|value| format_timestamp(value, "generated"))
                .transpose()?,
            about: Some(about),
            items,
            extra: JsonObject::new(),
        })
    }

    /// Parses UTF-8 JSON text for inspection, editing, or normalized export.
    pub fn parse_json(input: &str) -> Result<Self, AnnotationError> {
        let value: Value = serde_json::from_str(input)?;
        Self::from_json_value(value)
    }

    /// Imports a JSON object value for inspection, editing, or normalized export.
    pub fn from_json_value(value: Value) -> Result<Self, AnnotationError> {
        let Value::Object(object) = value else {
            return Err(AnnotationError::ExpectedObject {
                kind: "annotation set",
            });
        };
        Ok(Self::from_object(object))
    }

    /// Exports the set as compact, normalized JSON.
    pub fn to_json_string(&self) -> Result<String, AnnotationError> {
        Ok(serde_json::to_string(&self.to_value())?)
    }

    /// Returns the recovered absolute set identifier.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the recovered JSON-LD context.
    pub fn context(&self) -> Option<&Value> {
        self.context.as_ref()
    }

    /// Returns the recovered generator description.
    pub fn generator(&self) -> Option<&AnnotationGenerator> {
        self.generator.as_ref()
    }

    /// Returns the recovered RFC 3339 generation timestamp.
    pub fn generated(&self) -> Option<&str> {
        self.generated.as_deref()
    }

    /// Returns the recovered publication description.
    pub fn about(&self) -> Option<&AnnotationAbout> {
        self.about.as_ref()
    }

    /// Returns recovered annotation items in source order.
    pub fn items(&self) -> &[Annotation] {
        self.items.as_slice()
    }

    /// Appends a strictly valid annotation with a unique identifier.
    pub fn add_annotation(&mut self, annotation: Annotation) -> Result<(), AnnotationModelError> {
        ensure_valid_annotation(&annotation)?;
        let id = required_annotation_id(&annotation)?;
        if self.items.iter().any(|item| item.id() == Some(id)) {
            return Err(AnnotationModelError::DuplicateAnnotationId { id: id.to_string() });
        }
        self.items.push(annotation);
        Ok(())
    }

    /// Replaces the uniquely identified annotation and returns the previous value.
    pub fn replace_annotation(
        &mut self,
        annotation: Annotation,
    ) -> Result<Annotation, AnnotationModelError> {
        ensure_valid_annotation(&annotation)?;
        let id = required_annotation_id(&annotation)?.to_string();
        let matches = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (item.id() == Some(id.as_str())).then_some(index))
            .collect::<Vec<_>>();
        let index = match matches.as_slice() {
            [] => return Err(AnnotationModelError::AnnotationNotFound { id }),
            [index] => *index,
            _ => return Err(AnnotationModelError::AmbiguousAnnotationId { id }),
        };
        let replaced = std::mem::replace(&mut self.items[index], annotation);
        Ok(replaced)
    }

    /// Removes and returns the uniquely identified annotation.
    pub fn remove_annotation(&mut self, id: &str) -> Result<Annotation, AnnotationModelError> {
        let matches = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (item.id() == Some(id)).then_some(index))
            .collect::<Vec<_>>();
        let index = match matches.as_slice() {
            [] => {
                return Err(AnnotationModelError::AnnotationNotFound { id: id.to_string() });
            }
            [index] => *index,
            _ => {
                return Err(AnnotationModelError::AmbiguousAnnotationId { id: id.to_string() });
            }
        };
        let removed = self.items.remove(index);
        Ok(removed)
    }

    pub(crate) fn audiovisual_body_resource_ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(|annotation| {
            let body = annotation.body()?;
            if body.is_audiovisual() {
                body.id()
            } else {
                None
            }
        })
    }

    pub(crate) fn audiovisual_body_resource_paths(&self) -> impl Iterator<Item = String> + '_ {
        self.audiovisual_body_resource_ids()
            .filter_map(normalize_annotation_resource_path)
    }

    /// Returns top-level source members not represented by typed fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_object(mut object: JsonObject) -> Self {
        let context = take_context(&mut object);
        let id = take_string_if(&mut object, ID, |value| {
            require_valid_id(value, "id").is_ok()
        });
        let type_valid =
            take_string_if(&mut object, TYPE, |value| value == ANNOTATION_SET).is_some();
        let generator = take_parsed(&mut object, GENERATOR, AnnotationGenerator::from_value);
        let generated = take_string_if(&mut object, GENERATED, |value| {
            parse_timestamp(value).is_some()
        });
        let about = take_parsed(&mut object, ABOUT, AnnotationAbout::from_value);
        let items = match object.remove(ITEMS) {
            Some(Value::Array(values)) => values
                .into_iter()
                .filter_map(Annotation::from_value)
                .collect(),
            _ => Vec::new(),
        };
        Self {
            context,
            type_valid,
            id,
            generator,
            generated,
            about,
            items,
            extra: object,
        }
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        object.insert(
            CONTEXT.to_string(),
            canonical_set_context(self.context.as_ref()),
        );
        insert_string(&mut object, ID, self.id.as_deref());
        if self.type_valid {
            object.insert(TYPE.to_string(), Value::String(ANNOTATION_SET.to_string()));
        }
        if let Some(generator) = &self.generator {
            object.insert(GENERATOR.to_string(), generator.to_value());
        }
        insert_string(&mut object, GENERATED, self.generated.as_deref());
        if let Some(about) = &self.about {
            object.insert(ABOUT.to_string(), about.to_value());
        }
        object.insert(
            ITEMS.to_string(),
            Value::Array(self.items.iter().map(Annotation::to_value).collect()),
        );
        Value::Object(object)
    }
}

/// Describes one annotation that can be built, inspected, exchanged, and resolved.
///
/// Parsed annotations may be partial. A missing typed value can have source evidence in
/// [`Annotation::extra`].
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    context: Option<Value>,
    type_valid: bool,
    id: Option<String>,
    motivation: Option<AnnotationMotivation>,
    created: Option<String>,
    modified: Option<String>,
    creator: Option<AnnotationCreator>,
    target: Option<AnnotationTarget>,
    body: Option<AnnotationBody>,
    extra: JsonObject,
}

#[bon::bon]
impl Annotation {
    /// Builds a complete annotation, validating its timestamps, target, creator, and body.
    #[builder]
    pub fn new(
        id: String,
        created: OffsetDateTime,
        target: AnnotationTarget,
        motivation: Option<AnnotationMotivation>,
        modified: Option<OffsetDateTime>,
        creator: Option<AnnotationCreator>,
        body: Option<AnnotationBody>,
    ) -> Result<Self, AnnotationModelError> {
        require_valid_id(&id, "id")?;
        if modified.is_some_and(|modified| modified < created) {
            return Err(AnnotationModelError::ModifiedBeforeCreated);
        }
        ensure_valid_target(&target)?;
        if let Some(creator) = &creator {
            ensure_valid_creator(creator)?;
        }
        if let Some(body) = &body {
            ensure_valid_body(body)?;
        }
        Ok(Self {
            context: None,
            type_valid: true,
            id: Some(id),
            motivation,
            created: Some(format_timestamp(created, "created")?),
            modified: modified
                .map(|value| format_timestamp(value, "modified"))
                .transpose()?,
            creator,
            target: Some(target),
            body,
            extra: JsonObject::new(),
        })
    }

    /// Returns the recovered absolute annotation identifier.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the annotation-level JSON-LD context, if authored and recoverable.
    pub fn context(&self) -> Option<&Value> {
        self.context.as_ref()
    }

    /// Returns the recognized motivation or its unknown authored token.
    pub fn motivation(&self) -> Option<&AnnotationMotivation> {
        self.motivation.as_ref()
    }

    /// Returns the recovered RFC 3339 creation timestamp.
    pub fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }

    /// Returns the recovered RFC 3339 modification timestamp.
    pub fn modified(&self) -> Option<&str> {
        self.modified.as_deref()
    }

    /// Returns the recovered creator.
    pub fn creator(&self) -> Option<&AnnotationCreator> {
        self.creator.as_ref()
    }

    /// Returns the recovered target.
    pub fn target(&self) -> Option<&AnnotationTarget> {
        self.target.as_ref()
    }

    /// Returns the recovered body.
    pub fn body(&self) -> Option<&AnnotationBody> {
        self.body.as_ref()
    }

    /// Returns source members not represented by typed annotation fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let context = take_context(&mut object);
        let id = take_string_if(&mut object, ID, |value| {
            require_valid_id(value, "id").is_ok()
        });
        let type_valid = take_string_if(&mut object, TYPE, |value| value == ANNOTATION).is_some();
        let motivation_raw = take_string(&mut object, MOTIVATION);
        let motivation = motivation_raw
            .as_deref()
            .map(AnnotationMotivation::from_raw);
        let created = take_string_if(&mut object, CREATED, |value| {
            parse_timestamp(value).is_some()
        });
        let modified = take_string_if(&mut object, MODIFIED, |value| {
            parse_timestamp(value).is_some()
        });
        let creator = take_parsed(&mut object, CREATOR, AnnotationCreator::from_value);
        let target = take_parsed(&mut object, TARGET, AnnotationTarget::from_value);
        let body = take_parsed(&mut object, BODY, AnnotationBody::from_value);
        Some(Self {
            context,
            type_valid,
            id,
            motivation,
            created,
            modified,
            creator,
            target,
            body,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        if let Some(context) = &self.context {
            object.insert(CONTEXT.to_string(), context.clone());
        }
        insert_string(&mut object, ID, self.id.as_deref());
        if self.type_valid {
            object.insert(TYPE.to_string(), Value::String(ANNOTATION.to_string()));
        }
        if let Some(motivation) = &self.motivation {
            object.insert(
                MOTIVATION.to_string(),
                Value::String(motivation.as_str().to_string()),
            );
        }
        insert_string(&mut object, CREATED, self.created.as_deref());
        insert_string(&mut object, MODIFIED, self.modified.as_deref());
        if let Some(creator) = &self.creator {
            object.insert(CREATOR.to_string(), creator.to_value());
        }
        if let Some(target) = &self.target {
            object.insert(TARGET.to_string(), target.to_value());
        }
        if let Some(body) = &self.body {
            object.insert(BODY.to_string(), body.to_value());
        }
        Value::Object(object)
    }
}

/// Describes why an annotation was created.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AnnotationMotivation {
    /// Marks a location for later return.
    Bookmarking,
    /// Adds a comment.
    Commenting,
    /// Highlights selected content.
    Highlighting,
    /// An unrecognized authored motivation.
    Unknown(
        /// Original token.
        String,
    ),
}

impl AnnotationMotivation {
    fn from_raw(value: &str) -> Self {
        match value {
            "bookmarking" => Self::Bookmarking,
            "commenting" => Self::Commenting,
            "highlighting" => Self::Highlighting,
            _ => Self::Unknown(value.to_string()),
        }
    }

    /// Returns the canonical token or the preserved unknown token.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bookmarking => "bookmarking",
            Self::Commenting => "commenting",
            Self::Highlighting => "highlighting",
            Self::Unknown(value) => value,
        }
    }
}

/// Selects a publication resource and ordered alternatives within that resource.
///
/// Use [`AnnotationTarget::source_state`] for inventory-only source lookup, or
/// [`PublicationAnalysis::resolve_annotation_target`](crate::analysis::PublicationAnalysis::resolve_annotation_target)
/// to resolve selectors against analyzed content. Builders require an unfragmented local source;
/// parsed targets can be partial.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationTarget {
    source: Option<String>,
    selectors: Vec<AnnotationSelector>,
    meta: Option<JsonObject>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationTarget {
    /// Builds a target with a local, unfragmented source and a bounded valid selector tree.
    #[builder]
    pub fn new(
        source: String,
        #[builder(default)] selectors: Vec<AnnotationSelector>,
        meta: Option<JsonObject>,
    ) -> Result<Self, AnnotationModelError> {
        require_non_empty(&source, "target source")?;
        if !matches!(
            parse_href(AuthoredHref::new(source.clone())),
            ParsedHref::Local { fragment: None, .. }
        ) {
            return Err(AnnotationModelError::InvalidTargetSource { value: source });
        }
        ensure_valid_selectors(&selectors)?;
        Ok(Self {
            source: Some(source),
            selectors,
            meta,
            extra: JsonObject::new(),
        })
    }

    /// Returns the recovered source, or an empty string when import could not recover one.
    pub fn source(&self) -> &str {
        self.source.as_deref().unwrap_or_default()
    }

    /// Returns selector alternatives in authored order.
    pub fn selectors(&self) -> &[AnnotationSelector] {
        self.selectors.as_slice()
    }

    /// Returns application metadata attached to the target.
    pub fn meta(&self) -> Option<&JsonObject> {
        self.meta.as_ref()
    }

    /// Returns source members not represented by typed target fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let source = take_string_if(&mut object, SOURCE, |source| !source.is_empty());
        let selectors = take_selector_array(&mut object, SELECTOR, 1);
        let meta = take_object(&mut object, META);
        Some(Self {
            source,
            selectors,
            meta,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        insert_string(&mut object, SOURCE, self.source.as_deref());
        if !self.selectors.is_empty() {
            object.insert(
                SELECTOR.to_string(),
                Value::Array(
                    self.selectors
                        .iter()
                        .map(AnnotationSelector::to_value)
                        .collect(),
                ),
            );
        }
        if let Some(meta) = &self.meta {
            object.insert(META.to_string(), Value::Object(meta.clone()));
        }
        Value::Object(object)
    }
}

/// Selects content using a known selector or retains an unknown selector for interchange.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationSelector {
    /// A fragment selector.
    Fragment(
        /// Selector value.
        FragmentSelector,
    ),
    /// A CSS selector requiring host DOM resolution.
    Css(
        /// Selector value.
        CssSelector,
    ),
    /// A rendered-text position selector.
    TextPosition(
        /// Selector value.
        TextPositionSelector,
    ),
    /// An unrecognized or malformed selector object.
    Unknown(
        /// Preserved selector object.
        UnknownSelector,
    ),
}

impl AnnotationSelector {
    fn from_value(value: Value, depth: usize) -> Self {
        let Value::Object(mut object) = value else {
            unreachable!()
        };
        let selector_type = object.get(TYPE).cloned();
        let Some(type_string) = object.get(TYPE).and_then(Value::as_str).map(str::to_string) else {
            return Self::Unknown(UnknownSelector {
                selector_type,
                raw: object,
            });
        };
        object.remove(TYPE);
        match type_string.as_str() {
            "FragmentSelector" => Self::Fragment(FragmentSelector::from_object(object, depth)),
            "CssSelector" => Self::Css(CssSelector::from_object(object, depth)),
            "TextPositionSelector" => {
                Self::TextPosition(TextPositionSelector::from_object(object, depth))
            }
            _ => {
                object.insert(TYPE.to_string(), Value::String(type_string));
                Self::Unknown(UnknownSelector {
                    selector_type,
                    raw: object,
                })
            }
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Fragment(selector) => selector.to_value(),
            Self::Css(selector) => selector.to_value(),
            Self::TextPosition(selector) => selector.to_value(),
            Self::Unknown(selector) => selector.to_value(),
        }
    }
}

impl From<FragmentSelector> for AnnotationSelector {
    fn from(value: FragmentSelector) -> Self {
        Self::Fragment(value)
    }
}

impl From<CssSelector> for AnnotationSelector {
    fn from(value: CssSelector) -> Self {
        Self::Css(value)
    }
}

impl From<TextPositionSelector> for AnnotationSelector {
    fn from(value: TextPositionSelector) -> Self {
        Self::TextPosition(value)
    }
}

/// A fragment identifier with optional conformance and refinement alternatives.
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentSelector {
    value: Option<String>,
    conforms_to: Option<FragmentConformsTo>,
    refined_by: Vec<AnnotationSelector>,
    extra: JsonObject,
}

#[bon::bon]
impl FragmentSelector {
    /// Builds a non-empty fragment selector with a bounded valid refinement tree.
    #[builder]
    pub fn new(
        value: String,
        conforms_to: Option<FragmentConformsTo>,
        #[builder(default)] refined_by: Vec<AnnotationSelector>,
    ) -> Result<Self, AnnotationModelError> {
        require_non_empty(&value, "fragment selector value")?;
        ensure_valid_refinements(&refined_by)?;
        Ok(Self {
            value: Some(value),
            conforms_to,
            refined_by,
            extra: JsonObject::new(),
        })
    }

    /// Returns the fragment value, or an empty string when malformed during import.
    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or_default()
    }

    /// Returns the fragment syntax declaration, including preserved unknown declarations.
    pub fn conforms_to(&self) -> Option<&FragmentConformsTo> {
        self.conforms_to.as_ref()
    }

    /// Returns refinement alternatives in authored order.
    pub fn refined_by(&self) -> &[AnnotationSelector] {
        self.refined_by.as_slice()
    }

    /// Returns source members not represented by typed fragment fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_object(mut object: JsonObject, depth: usize) -> Self {
        let value = take_string(&mut object, VALUE);
        let conforms_to_raw = take_string(&mut object, CONFORMS_TO);
        let conforms_to = conforms_to_raw.as_deref().map(FragmentConformsTo::from_raw);
        let refined_by = take_refinements(&mut object, depth);
        Self {
            value,
            conforms_to,
            refined_by,
            extra: object,
        }
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        object.insert(
            TYPE.to_string(),
            Value::String("FragmentSelector".to_string()),
        );
        insert_string(&mut object, VALUE, self.value.as_deref());
        if let Some(conforms_to) = &self.conforms_to {
            object.insert(
                CONFORMS_TO.to_string(),
                Value::String(conforms_to.as_str().to_string()),
            );
        }
        insert_selectors(&mut object, REFINED_BY, &self.refined_by);
        Value::Object(object)
    }
}

/// The specification to which a fragment selector conforms.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum FragmentConformsTo {
    /// HTML fragment identifiers (RFC 3236 URI used by the annotation vocabulary).
    Html,
    /// Media Fragments URI syntax.
    Media,
    /// SVG fragment identifiers.
    Svg,
    /// Scroll-to-text fragment syntax.
    TextFragment,
    /// An unrecognized authored conformance URI.
    Unknown(
        /// Original URI string.
        String,
    ),
}

impl FragmentConformsTo {
    fn from_raw(value: &str) -> Self {
        match value {
            "http://tools.ietf.org/rfc/rfc3236" => Self::Html,
            "http://www.w3.org/TR/media-frags/" => Self::Media,
            "http://www.w3.org/TR/SVG/" => Self::Svg,
            "https://wicg.github.io/scroll-to-text-fragment/" => Self::TextFragment,
            _ => Self::Unknown(value.to_string()),
        }
    }

    /// Returns the canonical URI or the preserved unknown string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Html => "http://tools.ietf.org/rfc/rfc3236",
            Self::Media => "http://www.w3.org/TR/media-frags/",
            Self::Svg => "http://www.w3.org/TR/SVG/",
            Self::TextFragment => "https://wicg.github.io/scroll-to-text-fragment/",
            Self::Unknown(value) => value,
        }
    }
}

/// A CSS selector with optional refinement alternatives.
#[derive(Debug, Clone, PartialEq)]
pub struct CssSelector {
    value: Option<String>,
    refined_by: Vec<AnnotationSelector>,
    extra: JsonObject,
}

#[bon::bon]
impl CssSelector {
    /// Builds a non-empty CSS selector with a bounded valid refinement tree.
    #[builder]
    pub fn new(
        value: String,
        #[builder(default)] refined_by: Vec<AnnotationSelector>,
    ) -> Result<Self, AnnotationModelError> {
        require_non_empty(&value, "CSS selector value")?;
        ensure_valid_refinements(&refined_by)?;
        Ok(Self {
            value: Some(value),
            refined_by,
            extra: JsonObject::new(),
        })
    }

    /// Returns the CSS selector, or an empty string when malformed during import.
    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or_default()
    }

    /// Returns refinement alternatives in authored order.
    pub fn refined_by(&self) -> &[AnnotationSelector] {
        self.refined_by.as_slice()
    }

    /// Returns source members not represented by typed CSS selector fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_object(mut object: JsonObject, depth: usize) -> Self {
        let value = take_string(&mut object, VALUE);
        let refined_by = take_refinements(&mut object, depth);
        Self {
            value,
            refined_by,
            extra: object,
        }
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        object.insert(TYPE.to_string(), Value::String("CssSelector".to_string()));
        insert_string(&mut object, VALUE, self.value.as_deref());
        insert_selectors(&mut object, REFINED_BY, &self.refined_by);
        Value::Object(object)
    }
}

/// A half-open Unicode code-point range in the publication text representation.
///
/// Detached analysis resolves these offsets against its normalized extracted source-text stream,
/// not browser layout or UTF-8/UTF-16 units.
#[derive(Debug, Clone, PartialEq)]
pub struct TextPositionSelector {
    start: Option<u64>,
    end: Option<u64>,
    refined_by: Vec<AnnotationSelector>,
    extra: JsonObject,
}

#[bon::bon]
impl TextPositionSelector {
    /// Builds a non-empty half-open text range with bounded valid refinements.
    #[builder]
    pub fn new(
        range: TextRange,
        #[builder(default)] refined_by: Vec<AnnotationSelector>,
    ) -> Result<Self, AnnotationModelError> {
        ensure_valid_refinements(&refined_by)?;
        if range.start() >= range.end() {
            return Err(AnnotationModelError::InvalidField {
                field: "text position selector range",
                value: format!("{}..{}", range.start(), range.end()),
            });
        }
        Ok(Self {
            start: Some(range.start()),
            end: Some(range.end()),
            refined_by,
            extra: JsonObject::new(),
        })
    }

    /// Returns the inclusive Unicode code-point start offset, if recovered.
    pub fn start(&self) -> Option<u64> {
        self.start
    }

    /// Returns the exclusive Unicode code-point end offset, if recovered.
    pub fn end(&self) -> Option<u64> {
        self.end
    }

    /// Returns refinement alternatives in authored order.
    pub fn refined_by(&self) -> &[AnnotationSelector] {
        self.refined_by.as_slice()
    }

    /// Returns source members not represented by typed text-position fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_object(mut object: JsonObject, depth: usize) -> Self {
        let start = take_u64(&mut object, START);
        let end = take_u64(&mut object, END);
        let refined_by = take_refinements(&mut object, depth);
        Self {
            start,
            end,
            refined_by,
            extra: object,
        }
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        object.insert(
            TYPE.to_string(),
            Value::String("TextPositionSelector".to_string()),
        );
        if let Some(start) = self.start {
            object.insert(START.to_string(), Value::Number(start.into()));
        }
        if let Some(end) = self.end {
            object.insert(END.to_string(), Value::Number(end.into()));
        }
        insert_selectors(&mut object, REFINED_BY, &self.refined_by);
        Value::Object(object)
    }
}

/// Retains an unknown or malformed selector for inspection and export.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownSelector {
    selector_type: Option<Value>,
    raw: JsonObject,
}

impl UnknownSelector {
    /// Returns the original `type` member, including non-string malformed values.
    pub fn selector_type(&self) -> Option<&Value> {
        self.selector_type.as_ref()
    }

    /// Returns the complete selector object used for normalized re-serialization.
    pub fn raw(&self) -> &JsonObject {
        &self.raw
    }

    fn to_value(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

/// Carries inline text or references an audiovisual resource for an annotation.
///
/// Textual bodies use `value` and `text/plain`; audiovisual bodies use a safe bundle-relative
/// resource `id`. A parsed invalid media type remains available for export even when
/// [`AnnotationBody::format_media_type`] returns `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationBody {
    body_type: Option<AnnotationBodyType>,
    format: Option<MediaType>,
    format_source: Option<String>,
    value: Option<LocalizableText>,
    id: Option<String>,
    color: Option<AnnotationColor>,
    highlight: Option<AnnotationHighlight>,
    tags: Vec<String>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationBody {
    /// Builds a body and enforces fields and media types appropriate to its body kind.
    #[builder]
    pub fn new(
        body_type: AnnotationBodyType,
        format: Option<MediaType>,
        value: Option<LocalizableText>,
        id: Option<String>,
        color: Option<AnnotationColor>,
        highlight: Option<AnnotationHighlight>,
        #[builder(default)] tags: Vec<String>,
    ) -> Result<Self, AnnotationModelError> {
        let (format, value, id) = match body_type {
            AnnotationBodyType::TextualBody => {
                let value = value.ok_or(AnnotationModelError::MissingField { field: "value" })?;
                validate_typed_localizable_text(&value, "body value")?;
                if id.is_some() {
                    return Err(AnnotationModelError::FieldNotApplicable {
                        field: "id",
                        context: "a textual body",
                    });
                }
                if format.as_ref().is_some_and(|format| {
                    !format
                        .essence()
                        .is_some_and(|value| value.eq_ignore_ascii_case("text/plain"))
                }) {
                    return Err(AnnotationModelError::BodyFormatMismatch {
                        body_type: AnnotationBodyType::TextualBody.as_str(),
                        format: format
                            .as_ref()
                            .expect("checked as present")
                            .as_str()
                            .to_string(),
                    });
                }
                (
                    Some(MediaType::new("text/plain").expect("text/plain is a valid media type")),
                    Some(value),
                    None,
                )
            }
            AnnotationBodyType::Image | AnnotationBodyType::Audio | AnnotationBodyType::Video => {
                if value.is_some() {
                    return Err(AnnotationModelError::FieldNotApplicable {
                        field: "value",
                        context: "an audiovisual body",
                    });
                }
                let id = id.ok_or(AnnotationModelError::MissingField { field: "id" })?;
                let normalized = normalize_annotation_resource_path(&id).ok_or_else(|| {
                    AnnotationModelError::InvalidResourcePath { path: id.clone() }
                })?;
                if let Some(format) = &format {
                    let matches = match body_type {
                        AnnotationBodyType::Image => format.has_top_level_type(mime::IMAGE),
                        AnnotationBodyType::Audio => format.has_top_level_type(mime::AUDIO),
                        AnnotationBodyType::Video => format.has_top_level_type(mime::VIDEO),
                        AnnotationBodyType::TextualBody => unreachable!(),
                    };
                    if !matches {
                        return Err(AnnotationModelError::BodyFormatMismatch {
                            body_type: body_type.as_str(),
                            format: format.as_str().to_string(),
                        });
                    }
                }
                (format, None, Some(normalized))
            }
        };
        Ok(Self {
            body_type: Some(body_type),
            format_source: format.as_ref().map(|value| value.as_str().to_string()),
            format,
            value,
            id,
            color,
            highlight,
            tags,
            extra: JsonObject::new(),
        })
    }

    /// Builds a plain textual body with canonical `text/plain` format.
    pub fn text(value: impl Into<LocalizableText>) -> Result<Self, AnnotationModelError> {
        Self::builder()
            .body_type(AnnotationBodyType::TextualBody)
            .value(value.into())
            .build()
    }

    /// Returns the recovered body kind.
    pub fn body_type(&self) -> Option<&AnnotationBodyType> {
        self.body_type.as_ref()
    }

    /// Returns the parsed, valid media type.
    pub fn format_media_type(&self) -> Option<&MediaType> {
        self.format.as_ref()
    }

    /// Returns textual body content.
    pub fn value(&self) -> Option<&LocalizableText> {
        self.value.as_ref()
    }

    /// Returns the normalized detached resource path for an audiovisual body.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the requested color, including a preserved unknown token.
    pub fn color(&self) -> Option<&AnnotationColor> {
        self.color.as_ref()
    }

    /// Returns the requested highlight style, including a preserved unknown token.
    pub fn highlight(&self) -> Option<&AnnotationHighlight> {
        self.highlight.as_ref()
    }

    /// Returns recovered string tags in source order.
    pub fn tags(&self) -> &[String] {
        self.tags.as_slice()
    }

    /// Returns source members not represented by typed body fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    /// Returns whether the recovered kind is image, audio, or video.
    pub fn is_audiovisual(&self) -> bool {
        matches!(
            self.body_type,
            Some(AnnotationBodyType::Image | AnnotationBodyType::Audio | AnnotationBodyType::Video)
        )
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let body_type_raw = take_string_if(&mut object, TYPE, |value| {
            AnnotationBodyType::from_raw(value).is_some()
        });
        let body_type = body_type_raw
            .as_deref()
            .and_then(AnnotationBodyType::from_raw);
        let format_raw = take_string(&mut object, FORMAT);
        let format = format_raw
            .as_deref()
            .and_then(MediaType::new)
            .filter(MediaType::is_valid);
        let mut value = take_parsed(&mut object, VALUE, parse_localizable_text);
        let mut id = take_string(&mut object, ID);
        let color_raw = take_string(&mut object, COLOR);
        let color = color_raw.as_deref().map(AnnotationColor::from_raw);
        let highlight_raw = take_string(&mut object, HIGHLIGHT);
        let highlight = highlight_raw.as_deref().map(AnnotationHighlight::from_raw);
        let tags = take_string_array(&mut object, TAGS);
        match body_type {
            Some(AnnotationBodyType::TextualBody) => {
                id = None;
            }
            Some(
                AnnotationBodyType::Image | AnnotationBodyType::Audio | AnnotationBodyType::Video,
            ) => {
                value = None;
                if id
                    .as_deref()
                    .is_some_and(|value| normalize_annotation_resource_path(value).is_none())
                {
                    object.insert(ID.to_string(), Value::String(id.take().unwrap()));
                }
            }
            _ => {}
        }
        Some(Self {
            body_type,
            format,
            format_source: format_raw,
            value,
            id,
            color,
            highlight,
            tags,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        if let Some(body_type) = &self.body_type {
            object.insert(
                TYPE.to_string(),
                Value::String(body_type.as_str().to_string()),
            );
        }
        insert_string(&mut object, FORMAT, self.format_source.as_deref());
        if let Some(value) = &self.value {
            object.insert(VALUE.to_string(), value.to_value());
        }
        insert_string(&mut object, ID, self.id.as_deref());
        if let Some(color) = &self.color {
            object.insert(COLOR.to_string(), Value::String(color.as_str().to_string()));
        }
        if let Some(highlight) = &self.highlight {
            object.insert(
                HIGHLIGHT.to_string(),
                Value::String(highlight.as_str().to_string()),
            );
        }
        if !self.tags.is_empty() {
            object.insert(
                TAGS.to_string(),
                Value::Array(self.tags.iter().cloned().map(Value::String).collect()),
            );
        }
        Value::Object(object)
    }
}

/// Chooses the supported representation of an annotation body.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum AnnotationBodyType {
    /// Inline localizable plain text.
    TextualBody,
    /// Detached image resource.
    Image,
    /// Detached audio resource.
    Audio,
    /// Detached video resource.
    Video,
}

impl AnnotationBodyType {
    fn from_raw(value: &str) -> Option<Self> {
        match value {
            "TextualBody" => Some(Self::TextualBody),
            "Image" => Some(Self::Image),
            "Audio" => Some(Self::Audio),
            "Video" => Some(Self::Video),
            _ => None,
        }
    }

    /// Returns the canonical JSON-LD type string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextualBody => "TextualBody",
            Self::Image => "Image",
            Self::Audio => "Audio",
            Self::Video => "Video",
        }
    }
}

/// Suggests a color for presenting an annotation.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AnnotationColor {
    /// Pink.
    Pink,
    /// Orange.
    Orange,
    /// Yellow.
    Yellow,
    /// Green.
    Green,
    /// Blue.
    Blue,
    /// Purple.
    Purple,
    /// An unrecognized authored color.
    Unknown(
        /// Original token.
        String,
    ),
}

impl AnnotationColor {
    fn from_raw(value: &str) -> Self {
        match value {
            "pink" => Self::Pink,
            "orange" => Self::Orange,
            "yellow" => Self::Yellow,
            "green" => Self::Green,
            "blue" => Self::Blue,
            "purple" => Self::Purple,
            _ => Self::Unknown(value.to_string()),
        }
    }

    /// Returns the canonical token or preserved unknown token.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pink => "pink",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Unknown(value) => value,
        }
    }
}

/// Suggests how to present highlighted content.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AnnotationHighlight {
    /// Solid highlight.
    Solid,
    /// Underlined content.
    Underline,
    /// Struck-through content.
    Strikethrough,
    /// Outlined content.
    Outline,
    /// An unrecognized authored style.
    Unknown(
        /// Original token.
        String,
    ),
}

impl AnnotationHighlight {
    fn from_raw(value: &str) -> Self {
        match value {
            "solid" => Self::Solid,
            "underline" => Self::Underline,
            "strikethrough" => Self::Strikethrough,
            "outline" => Self::Outline,
            _ => Self::Unknown(value.to_string()),
        }
    }

    /// Returns the canonical token or preserved unknown token.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Solid => "solid",
            Self::Underline => "underline",
            Self::Strikethrough => "strikethrough",
            Self::Outline => "outline",
            Self::Unknown(value) => value,
        }
    }
}

/// Supplies plain text or text with language and direction metadata.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalizableText {
    /// A JSON string without language metadata.
    Plain(
        /// Text value.
        String,
    ),
    /// A JSON object carrying text and optional localization metadata.
    Localized {
        /// Text value.
        text: String,
        /// Authored BCP 47 language tag, when valid and present.
        language: Option<String>,
        /// Authored base direction.
        direction: Option<AnnotationTextDirection>,
        /// Source members not represented by localization fields.
        extra: JsonObject,
    },
}

impl LocalizableText {
    /// Constructs localized text; containing strict builders validate any language tag.
    pub fn localized(
        text: impl Into<String>,
        language: Option<String>,
        direction: Option<AnnotationTextDirection>,
    ) -> Self {
        Self::Localized {
            text: text.into(),
            language,
            direction,
            extra: JsonObject::new(),
        }
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::String(value) => Some(Self::Plain(value)),
            Value::Object(mut object) => {
                let text = take_string(&mut object, "text")
                    .or_else(|| take_string(&mut object, "stringValue"))?;
                let language = take_string_if(&mut object, "language", is_valid_language_tag);
                let direction = take_string_if(&mut object, "direction", |direction| {
                    matches!(direction, "ltr" | "rtl")
                })
                .as_deref()
                .and_then(AnnotationTextDirection::from_raw);
                Some(Self::Localized {
                    text,
                    language,
                    direction,
                    extra: object,
                })
            }
            _ => None,
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Plain(value) => Value::String(value.clone()),
            Self::Localized {
                text,
                language,
                direction,
                extra,
            } => {
                let mut object = extra.clone();
                object.insert("text".to_string(), Value::String(text.clone()));
                insert_string(&mut object, "language", language.as_deref());
                if let Some(direction) = direction {
                    object.insert(
                        "direction".to_string(),
                        Value::String(direction.as_str().to_string()),
                    );
                }
                Value::Object(object)
            }
        }
    }
}

impl From<String> for LocalizableText {
    fn from(value: String) -> Self {
        Self::Plain(value)
    }
}

impl From<&str> for LocalizableText {
    fn from(value: &str) -> Self {
        Self::Plain(value.to_string())
    }
}

/// Chooses the base direction of localized annotation text.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AnnotationTextDirection {
    /// Left-to-right text.
    Ltr,
    /// Right-to-left text.
    Rtl,
}

impl AnnotationTextDirection {
    fn from_raw(value: &str) -> Option<Self> {
        match value {
            "ltr" => Some(Self::Ltr),
            "rtl" => Some(Self::Rtl),
            _ => None,
        }
    }

    /// Returns the canonical lowercase direction token.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

/// Identifies the person, organization, or software credited with an annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationCreator {
    id: Option<String>,
    creator_type: Option<AnnotationCreatorType>,
    name: Option<LocalizableText>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationCreator {
    /// Builds a creator with an absolute identifier and optional validated localized name.
    #[builder]
    pub fn new(
        id: String,
        creator_type: AnnotationCreatorType,
        name: Option<LocalizableText>,
    ) -> Result<Self, AnnotationModelError> {
        require_valid_id(&id, "creator id")?;
        if let Some(name) = &name {
            validate_typed_localizable_text(name, "creator name")?;
        }
        Ok(Self {
            id: Some(id),
            creator_type: Some(creator_type),
            name,
            extra: JsonObject::new(),
        })
    }

    /// Returns the recovered absolute creator identifier.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the recovered creator kind.
    pub fn creator_type(&self) -> Option<&AnnotationCreatorType> {
        self.creator_type.as_ref()
    }

    /// Returns the creator's localizable display name.
    pub fn name(&self) -> Option<&LocalizableText> {
        self.name.as_ref()
    }

    /// Returns source members not represented by typed creator fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let id = take_string_if(&mut object, ID, |value| {
            require_valid_id(value, "id").is_ok()
        });
        let creator_type_raw = take_string_if(&mut object, TYPE, |value| {
            AnnotationCreatorType::from_raw(value).is_some()
        });
        let creator_type = creator_type_raw
            .as_deref()
            .and_then(AnnotationCreatorType::from_raw);
        let name = take_parsed(&mut object, NAME, parse_localizable_text);
        Some(Self {
            id,
            creator_type,
            name,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        insert_string(&mut object, ID, self.id.as_deref());
        if let Some(creator_type) = &self.creator_type {
            object.insert(
                TYPE.to_string(),
                Value::String(creator_type.as_str().to_string()),
            );
        }
        if let Some(name) = &self.name {
            object.insert(NAME.to_string(), name.to_value());
        }
        Value::Object(object)
    }
}

/// Chooses the supported kind of annotation creator.
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AnnotationCreatorType {
    /// A person.
    Person,
    /// An organization.
    Organization,
    /// Software acting as creator.
    Software,
}

impl AnnotationCreatorType {
    fn from_raw(value: &str) -> Option<Self> {
        match value {
            "Person" => Some(Self::Person),
            "Organization" => Some(Self::Organization),
            "Software" => Some(Self::Software),
            _ => None,
        }
    }

    /// Returns the canonical JSON-LD type string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Person => "Person",
            Self::Organization => "Organization",
            Self::Software => "Software",
        }
    }
}

/// Identifies software that generated an annotation set.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationGenerator {
    id: Option<String>,
    generator_type: Option<String>,
    name: Option<String>,
    homepage: Option<String>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationGenerator {
    /// Builds a software generator with an absolute ID and optional absolute homepage.
    #[builder]
    pub fn new(
        id: String,
        name: String,
        homepage: Option<String>,
    ) -> Result<Self, AnnotationModelError> {
        require_valid_id(&id, "generator id")?;
        require_non_empty(&name, "generator name")?;
        if let Some(homepage) = &homepage {
            require_absolute_url(homepage, "generator homepage")?;
        }
        Ok(Self {
            id: Some(id),
            generator_type: Some("Software".to_string()),
            name: Some(name),
            homepage,
            extra: JsonObject::new(),
        })
    }

    /// Returns the recovered absolute generator identifier.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the recovered type, which strict construction fixes to `Software`.
    pub fn generator_type(&self) -> Option<&str> {
        self.generator_type.as_deref()
    }

    /// Returns the recovered non-empty software name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the recovered absolute homepage URL.
    pub fn homepage(&self) -> Option<&str> {
        self.homepage.as_deref()
    }

    /// Returns source members not represented by typed generator fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let id = take_string_if(&mut object, ID, |value| {
            require_valid_id(value, "id").is_ok()
        });
        let generator_type = take_string_if(&mut object, TYPE, |value| value == "Software");
        let name = take_string(&mut object, NAME);
        let homepage = take_string_if(&mut object, HOMEPAGE, |homepage| {
            require_absolute_url(homepage, "generator homepage").is_ok()
        });
        Some(Self {
            id,
            generator_type,
            name,
            homepage,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        insert_string(&mut object, ID, self.id.as_deref());
        insert_string(&mut object, TYPE, self.generator_type.as_deref());
        insert_string(&mut object, NAME, self.name.as_deref());
        insert_string(&mut object, HOMEPAGE, self.homepage.as_deref());
        Value::Object(object)
    }
}

/// Identifies the publication described by an annotation set.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationAbout {
    identifiers: Vec<String>,
    format: Option<String>,
    title: Option<LocalizableText>,
    publisher: Option<LocalizableText>,
    creators: Vec<LocalizableText>,
    date: Option<String>,
    extra: JsonObject,
}

#[bon::bon]
impl AnnotationAbout {
    /// Builds publication metadata and validates media type, year, and localized text values.
    #[builder]
    pub fn new(
        #[builder(default)] identifiers: Vec<String>,
        format: Option<String>,
        title: Option<LocalizableText>,
        publisher: Option<LocalizableText>,
        #[builder(default)] creators: Vec<LocalizableText>,
        date: Option<String>,
    ) -> Result<Self, AnnotationModelError> {
        if let Some(format) = &format {
            require_media_type(format, "about format")?;
        }
        if let Some(date) = &date {
            require_year(date, "about date")?;
        }
        for value in title.iter().chain(publisher.iter()).chain(creators.iter()) {
            validate_typed_localizable_text(value, "about localized text")?;
        }
        Ok(Self {
            identifiers,
            format,
            title,
            publisher,
            creators,
            date,
            extra: JsonObject::new(),
        })
    }

    /// Returns recovered `dc:identifier` strings in source order.
    pub fn identifiers(&self) -> &[String] {
        self.identifiers.as_slice()
    }

    /// Returns the recovered valid `dc:format` media type string.
    pub fn format(&self) -> Option<&str> {
        self.format.as_deref()
    }

    /// Returns the publication title.
    pub fn title(&self) -> Option<&LocalizableText> {
        self.title.as_ref()
    }

    /// Returns the publication publisher.
    pub fn publisher(&self) -> Option<&LocalizableText> {
        self.publisher.as_ref()
    }

    /// Returns publication creators in source order.
    pub fn creators(&self) -> &[LocalizableText] {
        self.creators.as_slice()
    }

    /// Returns the recovered four-digit publication year.
    pub fn date(&self) -> Option<&str> {
        self.date.as_deref()
    }

    /// Returns source members not represented by typed publication fields.
    pub fn extra(&self) -> &JsonObject {
        &self.extra
    }

    fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut object) = value else {
            return None;
        };
        let identifiers = take_string_array(&mut object, "dc:identifier");
        let format = take_string_if(&mut object, "dc:format", |format| {
            MediaType::new(format).is_some_and(|format| format.is_valid())
        });
        let title = take_localizable_text(&mut object, "dc:title");
        let publisher = take_localizable_text(&mut object, "dc:publisher");
        let creators = take_localizable_text_array(&mut object, "dc:creator");
        let date = take_string_if(&mut object, "dc:date", |date| {
            date.len() == 4 && date.bytes().all(|byte| byte.is_ascii_digit())
        });
        Some(Self {
            identifiers,
            format,
            title,
            publisher,
            creators,
            date,
            extra: object,
        })
    }

    fn to_value(&self) -> Value {
        let mut object = self.extra.clone();
        if !self.identifiers.is_empty() {
            object.insert(
                "dc:identifier".to_string(),
                Value::Array(
                    self.identifiers
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        insert_string(&mut object, "dc:format", self.format.as_deref());
        if let Some(title) = &self.title {
            object.insert("dc:title".to_string(), title.to_value());
        }
        if let Some(publisher) = &self.publisher {
            object.insert("dc:publisher".to_string(), publisher.to_value());
        }
        if !self.creators.is_empty() {
            object.insert(
                "dc:creator".to_string(),
                Value::Array(
                    self.creators
                        .iter()
                        .map(LocalizableText::to_value)
                        .collect(),
                ),
            );
        }
        insert_string(&mut object, "dc:date", self.date.as_deref());
        Value::Object(object)
    }
}

fn take_string(object: &mut JsonObject, key: &str) -> Option<String> {
    object
        .get(key)
        .is_some_and(Value::is_string)
        .then(|| {
            object
                .remove(key)
                .and_then(|value| value.as_str().map(str::to_string))
        })
        .flatten()
}

fn take_string_if(
    object: &mut JsonObject,
    key: &str,
    predicate: impl FnOnce(&str) -> bool,
) -> Option<String> {
    let value = object.get(key)?.as_str()?;
    if !predicate(value) {
        return None;
    }
    take_string(object, key)
}

fn take_parsed<T>(
    object: &mut JsonObject,
    key: &str,
    parse: impl FnOnce(Value) -> Option<T>,
) -> Option<T> {
    let parsed = parse(object.get(key)?.clone())?;
    object.remove(key);
    Some(parsed)
}

fn take_object(object: &mut JsonObject, key: &str) -> Option<JsonObject> {
    if !object.get(key).is_some_and(Value::is_object) {
        return None;
    }
    object
        .remove(key)
        .and_then(|value| value.as_object().cloned())
}

fn canonical_set_context(context: Option<&Value>) -> Value {
    let canonical = Value::String(EPUB_ANNOTATIONS_CONTEXT.to_string());
    let Some(context) = context else {
        return canonical;
    };
    let extensions = match context {
        Value::String(value) if value == EPUB_ANNOTATIONS_CONTEXT => Vec::new(),
        Value::Array(values) => values
            .iter()
            .filter(|value| **value != canonical)
            .cloned()
            .collect(),
        value => vec![value.clone()],
    };
    if extensions.is_empty() {
        canonical
    } else {
        Value::Array(std::iter::once(canonical).chain(extensions).collect())
    }
}

fn take_u64(object: &mut JsonObject, key: &str) -> Option<u64> {
    let value = object.get(key)?.as_u64()?;
    object.remove(key);
    Some(value)
}

fn take_string_array(object: &mut JsonObject, key: &str) -> Vec<String> {
    if !object.get(key).is_some_and(Value::is_array) {
        return Vec::new();
    }
    match object.remove(key) {
        Some(Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| match value {
                Value::String(value) => Some(value),
                _ => None,
            })
            .collect(),
        Some(_) | None => Vec::new(),
    }
}

fn take_localizable_text(object: &mut JsonObject, key: &str) -> Option<LocalizableText> {
    take_parsed(object, key, parse_localizable_text)
}

fn take_localizable_text_array(object: &mut JsonObject, key: &str) -> Vec<LocalizableText> {
    if !object.get(key).is_some_and(Value::is_array) {
        return Vec::new();
    }
    match object.remove(key) {
        Some(Value::Array(values)) => values
            .into_iter()
            .filter_map(parse_localizable_text)
            .collect(),
        Some(_) | None => Vec::new(),
    }
}

fn parse_localizable_text(value: Value) -> Option<LocalizableText> {
    LocalizableText::from_value(value)
}

fn take_context(object: &mut JsonObject) -> Option<Value> {
    if !matches!(
        object.get(CONTEXT),
        Some(Value::String(_) | Value::Array(_))
    ) {
        return None;
    }
    object.remove(CONTEXT)
}

fn parse_selector(value: Value, depth: usize) -> Option<AnnotationSelector> {
    if !value.is_object() {
        return None;
    }
    Some(AnnotationSelector::from_value(value, depth))
}

fn take_selector_array(
    object: &mut JsonObject,
    key: &str,
    depth: usize,
) -> Vec<AnnotationSelector> {
    if !object.get(key).is_some_and(Value::is_array) {
        return Vec::new();
    }
    match object.remove(key) {
        Some(Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| parse_selector(value, depth))
            .collect(),
        _ => Vec::new(),
    }
}

fn take_refinements(object: &mut JsonObject, depth: usize) -> Vec<AnnotationSelector> {
    if depth >= MAX_SELECTOR_NESTING_DEPTH {
        // Known refinement content beyond the model bound is intentionally normalized away.
        object.remove(REFINED_BY);
        return Vec::new();
    }
    match object.get(REFINED_BY) {
        Some(Value::Array(_)) => take_selector_array(object, REFINED_BY, depth + 1),
        Some(Value::Object(_)) => object
            .remove(REFINED_BY)
            .and_then(|value| parse_selector(value, depth + 1))
            .into_iter()
            .collect(),
        Some(_) | None => Vec::new(),
    }
}

fn insert_string(object: &mut JsonObject, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn insert_selectors(object: &mut JsonObject, key: &str, values: &[AnnotationSelector]) {
    if values.is_empty() {
        return;
    }
    let value = if values.len() == 1 {
        values[0].to_value()
    } else {
        Value::Array(values.iter().map(AnnotationSelector::to_value).collect())
    };
    object.insert(key.to_string(), value);
}

fn require_non_empty(value: &str, field: &'static str) -> Result<(), AnnotationModelError> {
    if value.trim().is_empty() {
        Err(AnnotationModelError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn require_valid_id(value: &str, field: &'static str) -> Result<(), AnnotationModelError> {
    require_non_empty(value, field)?;
    if Url::parse(value).is_err() {
        return Err(AnnotationModelError::InvalidField {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn require_absolute_url(value: &str, field: &'static str) -> Result<(), AnnotationModelError> {
    if Url::parse(value).is_err() {
        return Err(AnnotationModelError::InvalidField {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn require_media_type(value: &str, field: &'static str) -> Result<(), AnnotationModelError> {
    if !MediaType::new(value).is_some_and(|media_type| media_type.is_valid()) {
        return Err(AnnotationModelError::InvalidField {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn require_year(value: &str, field: &'static str) -> Result<(), AnnotationModelError> {
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AnnotationModelError::InvalidField {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn is_valid_language_tag(value: &str) -> bool {
    LanguageTag::parse(value).is_ok()
}

fn validate_typed_localizable_text(
    value: &LocalizableText,
    field: &'static str,
) -> Result<(), AnnotationModelError> {
    if let LocalizableText::Localized {
        language: Some(language),
        ..
    } = value
        && !is_valid_language_tag(language)
    {
        return Err(AnnotationModelError::InvalidField {
            field,
            value: language.clone(),
        });
    }
    Ok(())
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn format_timestamp(
    value: OffsetDateTime,
    field: &'static str,
) -> Result<String, AnnotationModelError> {
    value
        .format(&Rfc3339)
        .map_err(|_| AnnotationModelError::TimestampNotRepresentable { field })
}

fn required_annotation_id(annotation: &Annotation) -> Result<&str, AnnotationModelError> {
    annotation
        .id()
        .ok_or(AnnotationModelError::MissingField { field: "id" })
}

fn ensure_valid_annotation(annotation: &Annotation) -> Result<(), AnnotationModelError> {
    let id = required_annotation_id(annotation)?;
    require_valid_id(id, "id")?;
    if !annotation.type_valid {
        return Err(AnnotationModelError::InvalidField {
            field: "type",
            value: "expected Annotation".to_string(),
        });
    }
    if annotation.created.is_none() {
        return Err(AnnotationModelError::MissingField { field: "created" });
    }
    if let (Some(created), Some(modified)) = (
        annotation.created.as_deref().and_then(parse_timestamp),
        annotation.modified.as_deref().and_then(parse_timestamp),
    ) && modified < created
    {
        return Err(AnnotationModelError::ModifiedBeforeCreated);
    }
    let target = annotation
        .target
        .as_ref()
        .ok_or(AnnotationModelError::MissingField { field: "target" })?;
    ensure_valid_target(target)?;
    if let Some(creator) = &annotation.creator {
        ensure_valid_creator(creator)?;
    }
    if let Some(body) = &annotation.body {
        ensure_valid_body(body)?;
    }
    Ok(())
}

fn ensure_valid_target(target: &AnnotationTarget) -> Result<(), AnnotationModelError> {
    let source = target
        .source
        .as_deref()
        .ok_or(AnnotationModelError::MissingField {
            field: "target source",
        })?;
    require_non_empty(source, "target source")?;
    if !matches!(
        parse_href(AuthoredHref::new(source.to_string())),
        ParsedHref::Local { fragment: None, .. }
    ) {
        return Err(AnnotationModelError::InvalidTargetSource {
            value: source.to_string(),
        });
    }
    ensure_valid_selectors(&target.selectors)?;
    Ok(())
}

fn ensure_valid_creator(creator: &AnnotationCreator) -> Result<(), AnnotationModelError> {
    let id = creator
        .id
        .as_deref()
        .ok_or(AnnotationModelError::MissingField {
            field: "creator id",
        })?;
    require_valid_id(id, "creator id")?;
    if creator.creator_type.is_none() {
        return Err(AnnotationModelError::MissingField {
            field: "creator type",
        });
    }
    if let Some(name) = &creator.name {
        validate_typed_localizable_text(name, "creator name")?;
    }
    Ok(())
}

fn ensure_valid_body(body: &AnnotationBody) -> Result<(), AnnotationModelError> {
    let body_type = body
        .body_type
        .as_ref()
        .ok_or(AnnotationModelError::MissingField { field: "body type" })?;
    if body.format_source.is_some() && body.format.is_none() {
        return Err(AnnotationModelError::InvalidField {
            field: "body format",
            value: body.format_source.clone().unwrap_or_default(),
        });
    }
    match body_type {
        AnnotationBodyType::TextualBody => {
            let value = body
                .value
                .as_ref()
                .ok_or(AnnotationModelError::MissingField { field: "value" })?;
            validate_typed_localizable_text(value, "body value")?;
            if body.id.is_some() {
                return Err(AnnotationModelError::FieldNotApplicable {
                    field: "id",
                    context: "a textual body",
                });
            }
            if body.format.as_ref().is_some_and(|format| {
                !format
                    .essence()
                    .is_some_and(|value| value.eq_ignore_ascii_case("text/plain"))
            }) {
                return Err(AnnotationModelError::BodyFormatMismatch {
                    body_type: body_type.as_str(),
                    format: body.format_source.clone().unwrap_or_default(),
                });
            }
        }
        AnnotationBodyType::Image | AnnotationBodyType::Audio | AnnotationBodyType::Video => {
            if body.value.is_some() {
                return Err(AnnotationModelError::FieldNotApplicable {
                    field: "value",
                    context: "an audiovisual body",
                });
            }
            let id = body
                .id
                .as_deref()
                .ok_or(AnnotationModelError::MissingField { field: "id" })?;
            if normalize_annotation_resource_path(id).is_none() {
                return Err(AnnotationModelError::InvalidResourcePath {
                    path: id.to_string(),
                });
            }
            if let Some(format) = &body.format {
                let matches = match body_type {
                    AnnotationBodyType::Image => format.has_top_level_type(mime::IMAGE),
                    AnnotationBodyType::Audio => format.has_top_level_type(mime::AUDIO),
                    AnnotationBodyType::Video => format.has_top_level_type(mime::VIDEO),
                    AnnotationBodyType::TextualBody => unreachable!(),
                };
                if !matches {
                    return Err(AnnotationModelError::BodyFormatMismatch {
                        body_type: body_type.as_str(),
                        format: format.as_str().to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn ensure_valid_generator(generator: &AnnotationGenerator) -> Result<(), AnnotationModelError> {
    let id = generator
        .id
        .as_deref()
        .ok_or(AnnotationModelError::MissingField {
            field: "generator id",
        })?;
    require_valid_id(id, "generator id")?;
    if generator.generator_type.as_deref() != Some("Software") {
        return Err(AnnotationModelError::InvalidField {
            field: "generator type",
            value: generator.generator_type.clone().unwrap_or_default(),
        });
    }
    let name = generator
        .name
        .as_deref()
        .ok_or(AnnotationModelError::MissingField {
            field: "generator name",
        })?;
    require_non_empty(name, "generator name")?;
    if let Some(homepage) = &generator.homepage {
        require_absolute_url(homepage, "generator homepage")?;
    }
    Ok(())
}

fn ensure_valid_about(about: &AnnotationAbout) -> Result<(), AnnotationModelError> {
    if let Some(format) = &about.format {
        require_media_type(format, "about format")?;
    }
    if let Some(date) = &about.date {
        require_year(date, "about date")?;
    }
    for value in about
        .title
        .iter()
        .chain(about.publisher.iter())
        .chain(about.creators.iter())
    {
        validate_typed_localizable_text(value, "about localized text")?;
    }
    Ok(())
}

fn ensure_unique_annotation_ids(items: &[Annotation]) -> Result<(), AnnotationModelError> {
    let mut seen = HashSet::new();
    for annotation in items {
        let id = required_annotation_id(annotation)?;
        if !seen.insert(id) {
            return Err(AnnotationModelError::DuplicateAnnotationId { id: id.to_string() });
        }
    }
    Ok(())
}

fn ensure_valid_selectors(selectors: &[AnnotationSelector]) -> Result<(), AnnotationModelError> {
    ensure_valid_selector_tree(selectors, 1, false)
}

fn ensure_valid_refinements(selectors: &[AnnotationSelector]) -> Result<(), AnnotationModelError> {
    ensure_valid_selector_tree(selectors, 2, true)
}

fn ensure_valid_selector_tree(
    selectors: &[AnnotationSelector],
    starting_depth: usize,
    refinements_only: bool,
) -> Result<(), AnnotationModelError> {
    let mut pending = selectors
        .iter()
        .map(|selector| (selector, starting_depth, refinements_only))
        .collect::<Vec<_>>();
    while let Some((selector, depth, is_refinement)) = pending.pop() {
        if depth > MAX_SELECTOR_NESTING_DEPTH {
            return Err(AnnotationModelError::SelectorNestingDepthExceeded {
                max: MAX_SELECTOR_NESTING_DEPTH,
            });
        }
        match selector {
            AnnotationSelector::Fragment(selector) => {
                require_non_empty(selector.value(), "fragment selector value")?;
                pending.extend(
                    selector
                        .refined_by()
                        .iter()
                        .map(|selector| (selector, depth + 1, true)),
                );
            }
            AnnotationSelector::Css(selector) => {
                require_non_empty(selector.value(), "CSS selector value")?;
                pending.extend(
                    selector
                        .refined_by()
                        .iter()
                        .map(|selector| (selector, depth + 1, true)),
                );
            }
            AnnotationSelector::TextPosition(selector) => {
                let (Some(start), Some(end)) = (selector.start(), selector.end()) else {
                    return Err(AnnotationModelError::MissingField {
                        field: "text position selector range",
                    });
                };
                if start >= end {
                    return Err(AnnotationModelError::InvalidField {
                        field: "text position selector range",
                        value: format!("{start}..{end}"),
                    });
                }
                pending.extend(
                    selector
                        .refined_by()
                        .iter()
                        .map(|selector| (selector, depth + 1, true)),
                );
            }
            AnnotationSelector::Unknown(selector) => {
                if is_refinement {
                    return Err(AnnotationModelError::InvalidField {
                        field: "refinedBy selector type",
                        value: "unknown selector".to_string(),
                    });
                }
                if selector.selector_type().and_then(Value::as_str).is_none() {
                    return Err(AnnotationModelError::MissingField {
                        field: "selector type",
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict_set() -> Value {
        serde_json::json!({
            "id": "urn:test:set",
            "type": "AnnotationSet",
            "about": {},
            "items": [{
                "id": "urn:test:annotation",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "chapter.xhtml"}
            }]
        })
    }

    #[test]
    fn malformed_annotation_fields_are_recovered_into_partial_models() {
        let mut cases = Vec::new();
        for (json_pointer, bad_value) in [
            ("/id", Value::Null),
            ("/type", serde_json::json!("WrongSet")),
            ("/about", Value::Bool(false)),
            ("/items", serde_json::json!({})),
            ("/items/0/id", serde_json::json!("relative")),
            ("/items/0/type", serde_json::json!("WrongAnnotation")),
            ("/items/0/created", serde_json::json!("today")),
            ("/items/0/target", Value::Null),
            ("/items/0/target/source", serde_json::json!("")),
        ] {
            let mut value = strict_set();
            *value.pointer_mut(json_pointer).unwrap() = bad_value;
            cases.push((value, json_pointer));
        }

        let mut missing = strict_set();
        missing.as_object_mut().unwrap().remove(ABOUT);
        cases.push((missing, "/about"));
        let mut missing = strict_set();
        missing[ITEMS][0].as_object_mut().unwrap().remove(TARGET);
        cases.push((missing, "/items/0/target"));

        for (value, field) in cases {
            let set = AnnotationSet::from_json_value(value).unwrap();
            assert!(set.to_json_string().is_ok(), "{field}");
        }

        assert!(matches!(
            AnnotationSet::from_json_value(Value::Null),
            Err(AnnotationError::ExpectedObject { .. })
        ));
        assert!(matches!(
            AnnotationSet::parse_json("{"),
            Err(AnnotationError::Json { .. })
        ));
    }

    #[test]
    fn non_object_annotation_set_is_a_structural_error() {
        assert!(matches!(
            AnnotationSet::from_json_value(serde_json::json!([])),
            Err(AnnotationError::ExpectedObject {
                kind: "annotation set"
            })
        ));
    }

    #[test]
    fn malformed_selectors_remain_structurally_recoverable() {
        let target = AnnotationTarget::from_value(
            serde_json::json!({"source":"chapter.xhtml","selector":false}),
        )
        .unwrap();
        assert!(target.selectors().is_empty());
        assert_eq!(target.to_value()[SELECTOR], false);

        let target = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[{"type":"FragmentSelector","value":4}]
        }))
        .unwrap();
        let selector = &target.selectors()[0];
        let AnnotationSelector::Fragment(fragment) = selector else {
            panic!("expected fragment selector");
        };
        assert!(fragment.value.is_none());
        assert_eq!(fragment.value(), "");
        assert_eq!(fragment.extra()[VALUE], 4);
        assert_eq!(fragment.to_value()[VALUE], 4);

        let target = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[{"type":"TextPositionSelector","start":-1,"end":4}]
        }))
        .unwrap();
        let selector = &target.selectors()[0];
        let AnnotationSelector::TextPosition(position) = selector else {
            panic!("expected text position selector");
        };
        assert_eq!(position.start(), None);
        assert_eq!(position.end(), Some(4));
        assert_eq!(position.extra()[START], -1);
        assert_eq!(position.to_value()[START], -1);

        let target = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[{"type":"CssSelector","value":"p","refinedBy":false}]
        }))
        .unwrap();
        let selector = &target.selectors()[0];
        let AnnotationSelector::Css(css) = selector else {
            panic!("expected CSS selector");
        };
        assert!(css.refined_by().is_empty());
        assert_eq!(css.extra()[REFINED_BY], false);
        assert_eq!(css.to_value()[REFINED_BY], false);
    }

    #[test]
    fn parsed_unknown_selectors_require_a_string_type_when_cloned_into_a_builder() {
        for selector in [serde_json::json!({"x":true}), serde_json::json!({"type":4})] {
            let parsed = AnnotationTarget::from_value(
                serde_json::json!({"source":"chapter.xhtml","selector":[selector]}),
            )
            .unwrap();
            let error = AnnotationTarget::builder()
                .source(parsed.source().to_string())
                .selectors(parsed.selectors().to_vec())
                .build()
                .unwrap_err();
            assert!(matches!(
                error,
                AnnotationModelError::MissingField {
                    field: "selector type"
                }
            ));
        }

        let parsed = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[{"type":"FutureSelector","x":true}]
        }))
        .unwrap();
        assert!(
            AnnotationTarget::builder()
                .source(parsed.source().to_string())
                .selectors(parsed.selectors().to_vec())
                .build()
                .is_ok()
        );
    }

    #[test]
    fn parsed_annotations_with_reversed_timestamps_are_rejected_by_set_builder() {
        let annotation = Annotation::from_value(serde_json::json!({
            "id":"urn:test:a",
            "type":"Annotation",
            "created":"2026-07-16T12:00:00Z",
            "modified":"2026-07-16T11:00:00Z",
            "target":{"source":"chapter.xhtml"}
        }))
        .unwrap();
        let error = AnnotationSet::builder()
            .id("urn:test:set".to_string())
            .about(AnnotationAbout::builder().build().unwrap())
            .items(vec![annotation])
            .build()
            .unwrap_err();
        assert!(matches!(error, AnnotationModelError::ModifiedBeforeCreated));
    }

    #[test]
    fn representable_closed_values_contradictions_and_empty_selectors_survive() {
        let input = serde_json::json!({
            "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "x-set":true,
            "items":[{
                "id":"urn:test:a", "type":"Annotation",
                "motivation":"future-motivation",
                "created":"2026-07-15T02:00:00Z",
                "modified":"2026-07-15T01:00:00Z",
                "target":{"source":"chapter.xhtml","selector":[
                    {"type":"FragmentSelector","value":"","conformsTo":"future-conformance"},
                    {"type":"TextPositionSelector","start":9,"end":2},
                    {"type":"FutureSelector","x":true}
                ]},
                "body":{"type":"Image","id":"image.bin","format":"audio/mpeg",
                        "color":"sepia","highlight":"glow","x-body":true}
            }, {
                "id":"urn:test:b", "type":"Annotation",
                "created":"2026-07-15T00:00:00Z",
                "target":{"source":"chapter.xhtml"},
                "body":{"type":"TextualBody","value":"note","format":"not a media type"}
            }]
        });
        let set = AnnotationSet::from_json_value(input).unwrap();
        let output = set.to_value();

        assert_eq!(output["x-set"], true);
        assert_eq!(output[ITEMS][0][MOTIVATION], "future-motivation");
        assert_eq!(output[ITEMS][0][TARGET][SELECTOR][0][VALUE], "");
        assert_eq!(
            output[ITEMS][0][TARGET][SELECTOR][0][CONFORMS_TO],
            "future-conformance"
        );
        assert_eq!(output[ITEMS][0][TARGET][SELECTOR][1][START], 9);
        assert_eq!(output[ITEMS][0][TARGET][SELECTOR][2]["x"], true);
        assert_eq!(output[ITEMS][0][BODY][FORMAT], "audio/mpeg");
        assert_eq!(output[ITEMS][0][BODY][COLOR], "sepia");
        assert_eq!(output[ITEMS][0][BODY][HIGHLIGHT], "glow");
        assert_eq!(output[ITEMS][0][BODY]["x-body"], true);
        assert_eq!(output[ITEMS][1][BODY][FORMAT], "not a media type");
    }

    #[test]
    fn parses_annotation_set_and_preserves_unknown_fields() {
        let input = r##"{
            "@context": "https://www.w3.org/ns/epub-anno.jsonld",
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "x-extra": {"kept": true},
            "about": {"dc:identifier": ["urn:isbn:123"]},
            "items": [{
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {
                    "source": "titlepage.xhtml",
                    "selector": [{"type": "FragmentSelector", "value": "frag", "unknown": 1}]
                },
                "body": {"type": "TextualBody", "value": "note", "color": "blue"}
            }]
        }"##;

        let set = AnnotationSet::parse_json(input).unwrap();
        assert_eq!(set.id(), Some("urn:uuid:set"));
        assert_eq!(set.items().len(), 1);

        let output: Value = serde_json::from_str(&set.to_json_string().unwrap()).unwrap();
        assert_eq!(output["x-extra"]["kept"], Value::Bool(true));
        assert_eq!(output["items"][0]["target"]["selector"][0]["unknown"], 1);
    }

    #[test]
    fn annotation_accessors_expose_parsed_fields() {
        let input = r##"{
            "@context": "https://www.w3.org/ns/epub-anno.jsonld",
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "generator": {
                "id": "https://example.org/app",
                "type": "Software",
                "name": "Reader Lab",
                "homepage": "https://example.org",
                "version": "1.0"
            },
            "generated": "2026-07-15T00:00:00Z",
            "about": {
                "dc:identifier": ["urn:isbn:123"],
                "dc:format": "application/epub+zip",
                "dc:title": {"text": "Livre", "language": "fr"},
                "dc:publisher": "Publisher",
                "dc:creator": ["Author"],
                "dc:date": "2026",
                "unknown": true
            },
            "items": [{
                "@context": "https://www.w3.org/ns/epub-anno.jsonld",
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "motivation": "highlighting",
                "created": "2026-07-15T00:00:00Z",
                "modified": "2026-07-15T01:00:00Z",
                "creator": {
                    "id": "urn:uuid:user",
                    "type": "Person",
                    "name": "Reader",
                    "role": "tester"
                },
                "target": {"source": "titlepage.xhtml"},
                "body": {
                    "type": "TextualBody",
                    "format": "text/plain",
                    "value": "note",
                    "color": "yellow",
                    "highlight": "underline",
                    "tags": ["important"],
                    "unknown": true
                }
            }]
        }"##;

        let set = AnnotationSet::parse_json(input).unwrap();
        assert!(set.context().is_some());
        assert_eq!(set.generated(), Some("2026-07-15T00:00:00Z"));

        let generator = set.generator().unwrap();
        assert_eq!(generator.id(), Some("https://example.org/app"));
        assert_eq!(generator.generator_type(), Some("Software"));
        assert_eq!(generator.name(), Some("Reader Lab"));
        assert_eq!(generator.homepage(), Some("https://example.org"));
        assert_eq!(generator.extra()["version"], "1.0");

        let about = set.about().unwrap();
        assert_eq!(about.identifiers(), &["urn:isbn:123".to_string()]);
        assert_eq!(about.format(), Some("application/epub+zip"));
        assert!(matches!(
            about.title(),
            Some(LocalizableText::Localized { .. })
        ));
        assert!(
            matches!(about.publisher(), Some(LocalizableText::Plain(value)) if value == "Publisher")
        );
        assert_eq!(about.creators().len(), 1);
        assert_eq!(about.date(), Some("2026"));
        assert_eq!(about.extra()["unknown"], true);

        let annotation = &set.items()[0];
        assert!(annotation.context().is_some());
        assert!(matches!(
            annotation.motivation(),
            Some(AnnotationMotivation::Highlighting)
        ));
        assert_eq!(annotation.created(), Some("2026-07-15T00:00:00Z"));
        assert_eq!(annotation.modified(), Some("2026-07-15T01:00:00Z"));

        let creator = annotation.creator().unwrap();
        assert_eq!(creator.id(), Some("urn:uuid:user"));
        assert!(matches!(
            creator.creator_type(),
            Some(AnnotationCreatorType::Person)
        ));
        assert!(matches!(creator.name(), Some(LocalizableText::Plain(value)) if value == "Reader"));
        assert_eq!(creator.extra()["role"], "tester");

        let body = annotation.body().unwrap();
        assert_eq!(
            body.format_media_type().map(MediaType::as_str),
            Some("text/plain")
        );
        assert!(matches!(body.color(), Some(AnnotationColor::Yellow)));
        assert!(matches!(
            body.highlight(),
            Some(AnnotationHighlight::Underline)
        ));
        assert_eq!(body.tags(), &["important".to_string()]);
        assert_eq!(body.extra()["unknown"], true);
    }

    #[test]
    fn refined_by_accepts_a_single_selector() {
        let set = AnnotationSet::parse_json(
            r##"{
                "id": "urn:test:set",
                "type": "AnnotationSet",
                "about": {},
                "items": [{
                    "id": "urn:test:a",
                    "type": "Annotation",
                    "created": "2026-07-15T00:00:00Z",
                    "target": {
                        "source": "chapter.xhtml",
                        "selector": [{
                            "type": "FragmentSelector",
                            "value": "target",
                            "refinedBy": {"type": "CssSelector", "value": "#target"}
                        }]
                    }
                }]
            }"##,
        )
        .unwrap();

        let AnnotationSelector::Fragment(selector) =
            &set.items()[0].target().unwrap().selectors()[0]
        else {
            panic!("expected fragment selector");
        };
        assert_eq!(selector.refined_by().len(), 1);
    }

    fn nested_css_json(depth: usize) -> Value {
        let mut selector = serde_json::json!({"type":"CssSelector","value":"leaf"});
        for index in 1..depth {
            selector = serde_json::json!({
                "type":"CssSelector",
                "value":format!("node-{index}"),
                "refinedBy":selector
            });
        }
        selector
    }

    fn typed_css_chain(depth: usize) -> AnnotationSelector {
        let mut selector: AnnotationSelector = CssSelector::builder()
            .value("leaf".to_string())
            .build()
            .unwrap()
            .into();
        for index in 1..depth {
            selector = CssSelector::builder()
                .value(format!("node-{index}"))
                .refined_by(vec![selector])
                .build()
                .unwrap()
                .into();
        }
        selector
    }

    #[test]
    fn tolerant_import_keeps_the_exact_selector_depth_limit() {
        let target = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[nested_css_json(MAX_SELECTOR_NESTING_DEPTH)]
        }))
        .unwrap();
        let mut selector = &target.selectors()[0];
        let mut depth = 1;
        loop {
            let AnnotationSelector::Css(css) = selector else {
                panic!("expected CSS selector");
            };
            let Some(next) = css.refined_by().first() else {
                break;
            };
            selector = next;
            depth += 1;
        }
        assert_eq!(depth, MAX_SELECTOR_NESTING_DEPTH);
    }

    #[test]
    fn tolerant_import_omits_only_refinements_beyond_the_depth_limit() {
        let target = AnnotationTarget::from_value(serde_json::json!({
            "source":"chapter.xhtml",
            "selector":[nested_css_json(MAX_SELECTOR_NESTING_DEPTH + 1)]
        }))
        .unwrap();
        let mut selector = &target.selectors()[0];
        for _ in 1..MAX_SELECTOR_NESTING_DEPTH {
            let AnnotationSelector::Css(css) = selector else {
                panic!("expected CSS selector");
            };
            selector = &css.refined_by()[0];
        }
        let AnnotationSelector::Css(css) = selector else {
            panic!("expected CSS selector");
        };
        assert!(css.refined_by().is_empty());
        assert!(css.to_value().get(REFINED_BY).is_none());
    }

    #[test]
    fn typed_selector_construction_accepts_the_limit_and_rejects_plus_one() {
        let at_limit = typed_css_chain(MAX_SELECTOR_NESTING_DEPTH);
        for error in [
            FragmentSelector::builder()
                .value("fragment".to_string())
                .refined_by(vec![at_limit.clone()])
                .build()
                .unwrap_err(),
            CssSelector::builder()
                .value("body".to_string())
                .refined_by(vec![at_limit.clone()])
                .build()
                .unwrap_err(),
            TextPositionSelector::builder()
                .range(TextRange::new(0, 1).unwrap())
                .refined_by(vec![at_limit])
                .build()
                .unwrap_err(),
        ] {
            assert!(matches!(
                error,
                AnnotationModelError::SelectorNestingDepthExceeded {
                    max: MAX_SELECTOR_NESTING_DEPTH
                }
            ));
        }
    }

    #[test]
    fn unknown_selectors_are_preserved_but_forbidden_as_refinements() {
        let set = AnnotationSet::parse_json(
            r##"{
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[{
                    "id":"urn:test:a", "type":"Annotation",
                    "created":"2026-07-15T00:00:00Z",
                    "target":{"source":"chapter.xhtml","selector":[
                        {"type":"FutureSelector","value":"top","x-top":true},
                        {"type":"CssSelector","value":"body","refinedBy":
                            {"type":"FutureSelector","value":"single","x-single":true}},
                        {"type":"FragmentSelector","value":"chapter","refinedBy":[
                            {"type":"FutureSelector","value":"array","x-array":true},
                            {"type":"CssSelector","value":"main","refinedBy":
                                {"type":"FutureSelector","value":"nested","x-nested":true}}
                        ]}
                    ]}
                }]
            }"##,
        )
        .unwrap();

        let parsed = set.items()[0].target().unwrap().selectors();
        assert!(matches!(parsed[0], AnnotationSelector::Unknown(_)));
        let AnnotationSelector::Css(css) = &parsed[1] else {
            panic!("expected CSS selector");
        };
        assert!(matches!(
            css.refined_by()[0],
            AnnotationSelector::Unknown(_)
        ));
        let AnnotationSelector::Fragment(fragment) = &parsed[2] else {
            panic!("expected fragment selector");
        };
        assert!(matches!(
            fragment.refined_by()[0],
            AnnotationSelector::Unknown(_)
        ));

        let normalized = set.to_value();
        let selectors = normalized[ITEMS][0][TARGET][SELECTOR].as_array().unwrap();
        assert_eq!(
            selectors[0],
            serde_json::json!({
                "type":"FutureSelector", "value":"top", "x-top":true
            })
        );
        assert_eq!(
            selectors[1][REFINED_BY],
            serde_json::json!({
                "type":"FutureSelector", "value":"single", "x-single":true
            })
        );
        assert_eq!(selectors[2][REFINED_BY][0]["x-array"], true);
        assert_eq!(selectors[2][REFINED_BY][1][REFINED_BY]["x-nested"], true);

        let unknown = set.items()[0].target().unwrap().selectors()[0].clone();
        assert!(
            CssSelector::builder()
                .value("body".to_string())
                .refined_by(vec![unknown.clone()])
                .build()
                .is_err()
        );
        assert!(
            FragmentSelector::builder()
                .value("chapter".to_string())
                .refined_by(vec![unknown.clone()])
                .build()
                .is_err()
        );
        assert!(
            TextPositionSelector::builder()
                .range(TextRange::new(0, 1).unwrap())
                .refined_by(vec![unknown])
                .build()
                .is_err()
        );
    }

    #[test]
    fn accepts_rfc3339_timestamps_and_parameterized_media_types() {
        let input = r#"{
            "@context": "https://www.w3.org/ns/epub-anno.jsonld",
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "generated": "2026-07-15T00:00:00+02:00",
            "about": {},
            "items": [{
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "modified": "2026-07-15T03:00:00+02:00",
                "target": {"source": "chapter.xhtml"},
                "body": {"type": "TextualBody", "value": "note", "format": "text/plain; charset=utf-8"}
            }]
        }"#;

        let set = AnnotationSet::parse_json(input).unwrap();
        assert_eq!(set.generated(), Some("2026-07-15T00:00:00+02:00"));
    }

    #[test]
    fn textual_body_extension_format_is_preserved() {
        let input = r#"{
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "about": {},
            "items": [{
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "chapter.xhtml"},
                "body": {"type": "TextualBody", "value": "note", "format": "text/html"}
            }]
        }"#;

        let set = AnnotationSet::parse_json(input).unwrap();
        assert_eq!(set.to_value()[ITEMS][0][BODY][FORMAT], "text/html");
        assert_eq!(
            set.items()[0]
                .body()
                .unwrap()
                .format_media_type()
                .unwrap()
                .as_str(),
            "text/html"
        );
    }

    #[test]
    fn audiovisual_body_format_should_match_body_type() {
        let input = r#"{
            "id": "urn:uuid:set",
            "type": "AnnotationSet",
            "about": {},
            "items": [{
                "id": "urn:uuid:image",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "chapter.xhtml"},
                "body": {"type": "Image", "id": "image.bin", "format": "audio/mpeg"}
            }, {
                "id": "urn:uuid:audio",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "chapter.xhtml"},
                "body": {"type": "Audio", "id": "audio.bin", "format": "video/webm"}
            }, {
                "id": "urn:uuid:video",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "chapter.xhtml"},
                "body": {"type": "Video", "id": "video.bin", "format": "image/png"}
            }]
        }"#;

        let set = AnnotationSet::parse_json(input).unwrap();
        let expected = ["audio/mpeg", "video/webm", "image/png"];
        for (annotation, expected) in set.items().iter().zip(expected) {
            assert_eq!(
                annotation
                    .body()
                    .unwrap()
                    .format_media_type()
                    .map(MediaType::as_str),
                Some(expected)
            );
        }
        let normalized = set.to_value();
        for (annotation, expected) in normalized[ITEMS].as_array().unwrap().iter().zip(expected) {
            assert_eq!(annotation[BODY][FORMAT], expected);
        }
    }

    #[test]
    fn duplicate_annotation_ids_round_trip_and_are_ambiguous_for_mutation() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id": "urn:uuid:set",
                "type": "AnnotationSet",
                "about": {},
                "items": [{
                    "id": "urn:uuid:a1", "type": "Annotation",
                    "created": "2026-07-15T00:00:00Z",
                    "target": {"source": "one.xhtml"}
                }, {
                    "id": "urn:uuid:a1", "type": "Annotation",
                    "created": "2026-07-15T00:00:00Z",
                    "target": {"source": "two.xhtml"}
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(set.items().len(), 2);
        assert_eq!(set.to_value()[ITEMS].as_array().unwrap().len(), 2);
    }

    #[test]
    fn malformed_agents_and_body_are_recovered() {
        let mut value = strict_set();
        value[GENERATOR] = serde_json::json!({"type":"Software","name":"App"});
        assert!(
            AnnotationSet::from_json_value(value)
                .unwrap()
                .generator()
                .is_some()
        );

        let mut value = strict_set();
        value[ITEMS][0][CREATOR] = serde_json::json!({"id":"urn:test:reader"});
        assert!(
            AnnotationSet::from_json_value(value).unwrap().items()[0]
                .creator()
                .is_some()
        );

        let mut value = strict_set();
        value[ITEMS][0][BODY] = serde_json::json!({"type":"UnknownBody"});
        assert!(
            AnnotationSet::from_json_value(value).unwrap().items()[0]
                .body()
                .is_some()
        );
    }

    #[test]
    fn invalid_selector_value_is_omitted_from_normalized_json() {
        let input = r#"{
            "id": "urn:test:set",
            "type": "AnnotationSet",
            "about": {},
            "items": [{
                "id": "urn:uuid:a1",
                "type": "Annotation",
                "created": "2026-07-15T00:00:00Z",
                "target": {"source": "titlepage.xhtml", "selector": ["bad selector"]}
            }]
        }"#;

        let set = AnnotationSet::parse_json(input).unwrap();
        assert!(set.to_value()[ITEMS][0][TARGET].get(SELECTOR).is_none());
    }

    #[test]
    fn malformed_known_scalar_values_are_preserved_in_extra() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet",
                "generator":{"id":"urn:test:app","type":"Person","name":"App","x":true},
                "about":{},
                "items":[{
                    "id":"a", "type":"Annotation", "created":"2026-07-15T00:00:00Z",
                    "motivation":"editing",
                    "creator":{"id":"user","type":"Robot","x":true},
                    "target":{"source":"chapter.xhtml","selector":[
                        3,
                        {"type":"FragmentSelector","value":"x","conformsTo":"unknown","x":true}
                    ]},
                    "body":{"type":"TextualBody","value":"note","format":"text/html",
                            "color":"brown","highlight":"glow","x":true}
                }]
            }"#,
        )
        .unwrap();
        let output = set.to_value();
        assert_eq!(output[GENERATOR][TYPE], "Person");
        assert_eq!(output[ITEMS][0][ID], "a");
        assert_eq!(output[ITEMS][0][CREATOR][ID], "user");
        assert_eq!(output[ITEMS][0][CREATOR][TYPE], "Robot");
        assert_eq!(output[ITEMS][0][BODY]["x"], true);
        assert_eq!(
            output[ITEMS][0][TARGET][SELECTOR].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn malformed_known_values_round_trip_when_canonical_fields_do_not_replace_them() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet", "generator":false, "about":{},
                "items":[{
                    "id":"a", "type":"Annotation", "created":"2026-07-15T00:00:00Z",
                    "creator":false,
                    "target":{"source":"chapter.xhtml","selector":["bad"]},
                    "body":false
                }]
            }"#,
        )
        .unwrap();
        let output = set.to_value();
        assert_eq!(output[GENERATOR], false);
        assert_eq!(output[ITEMS][0][CREATOR], false);
        assert_eq!(output[ITEMS][0][BODY], false);
        assert!(output[ITEMS][0][TARGET].get(SELECTOR).is_none());
    }

    #[test]
    fn remote_and_fragment_targets_are_recovered_and_serialized() {
        let set = AnnotationSet::parse_json(
            r##"{
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[
                    {"id":"urn:test:a", "type":"Annotation", "created":"2026-07-15T00:00:00Z",
                     "target":{"source":"https://example.com/chapter.xhtml"}},
                    {"id":"urn:test:b", "type":"Annotation", "created":"2026-07-15T00:00:00Z",
                     "target":{"source":"#fragment"}}
                ]
            }"##,
        )
        .unwrap();
        assert_eq!(
            set.items()[0].target().unwrap().source(),
            "https://example.com/chapter.xhtml"
        );
        assert_eq!(set.items()[1].target().unwrap().source(), "#fragment");
    }

    fn created_at() -> OffsetDateTime {
        OffsetDateTime::parse("2026-07-16T12:00:00Z", &Rfc3339).unwrap()
    }

    fn target_with(selectors: Vec<AnnotationSelector>) -> AnnotationTarget {
        AnnotationTarget::builder()
            .source("text/chapter.xhtml".to_string())
            .selectors(selectors)
            .build()
            .unwrap()
    }

    fn annotation_with_body(id: &str, body: AnnotationBody) -> Annotation {
        Annotation::builder()
            .id(format!("urn:test:{id}"))
            .created(created_at())
            .target(target_with(Vec::new()))
            .body(body)
            .build()
            .unwrap()
    }

    #[test]
    fn typed_builders_create_canonical_annotation_json() {
        let range = TextRange::new(4, 19).unwrap();
        let refinement = TextPositionSelector::builder()
            .range(range)
            .build()
            .unwrap();
        let selector = CssSelector::builder()
            .value("#intro > p:nth-child(2)".to_string())
            .refined_by(vec![refinement.into()])
            .build()
            .unwrap();
        let target = target_with(vec![selector.into()]);
        let creator = AnnotationCreator::builder()
            .id("urn:uuid:reader".to_string())
            .creator_type(AnnotationCreatorType::Person)
            .name(LocalizableText::from("Reader"))
            .build()
            .unwrap();
        let body = AnnotationBody::builder()
            .body_type(AnnotationBodyType::TextualBody)
            .value(LocalizableText::localized(
                "j'adore !",
                Some("fr".to_string()),
                None,
            ))
            .color(AnnotationColor::Blue)
            .highlight(AnnotationHighlight::Underline)
            .tags(vec!["teacher".to_string()])
            .build()
            .unwrap();
        let annotation = Annotation::builder()
            .id("urn:uuid:annotation".to_string())
            .created(created_at())
            .target(target)
            .motivation(AnnotationMotivation::Commenting)
            .creator(creator)
            .body(body)
            .build()
            .unwrap();
        let about = AnnotationAbout::builder()
            .identifiers(vec!["urn:isbn:1234567890".to_string()])
            .format("application/epub+zip".to_string())
            .title(LocalizableText::from("Alice in Wonderland"))
            .creators(vec![LocalizableText::from("Anne O'Tater")])
            .date("1865".to_string())
            .build()
            .unwrap();
        let generator = AnnotationGenerator::builder()
            .id("https://example.com/reader".to_string())
            .name("Reader".to_string())
            .homepage("https://example.com".to_string())
            .build()
            .unwrap();
        let set = AnnotationSet::builder()
            .id("urn:uuid:set".to_string())
            .about(about)
            .items(vec![annotation])
            .generator(generator)
            .generated(created_at())
            .build()
            .unwrap();

        let value: Value = serde_json::from_str(&set.to_json_string().unwrap()).unwrap();
        assert_eq!(value[CONTEXT], EPUB_ANNOTATIONS_CONTEXT);
        assert_eq!(value[TYPE], ANNOTATION_SET);
        assert_eq!(value[ITEMS][0][TYPE], ANNOTATION);
        assert_eq!(value[ITEMS][0][BODY][FORMAT], "text/plain");
        assert_eq!(value[ITEMS][0][TARGET][SELECTOR][0][REFINED_BY][START], 4);
        let reparsed = AnnotationSet::parse_json(&set.to_json_string().unwrap()).unwrap();
        assert_eq!(reparsed.items().len(), 1);
    }

    #[test]
    fn typed_builders_reject_invalid_semantic_values() {
        assert!(matches!(
            AnnotationTarget::builder()
                .source("chapter.xhtml#target".to_string())
                .build(),
            Err(AnnotationModelError::InvalidTargetSource { .. })
        ));
        assert!(matches!(
            CssSelector::builder().value(String::new()).build(),
            Err(AnnotationModelError::EmptyField { .. })
        ));
        for (body_type, format) in [
            (AnnotationBodyType::Image, "audio/mpeg"),
            (AnnotationBodyType::Audio, "video/webm"),
            (AnnotationBodyType::Video, "image/png"),
        ] {
            assert!(matches!(
                AnnotationBody::builder()
                    .body_type(body_type)
                    .id("resource.bin".to_string())
                    .format(MediaType::new(format).unwrap())
                    .build(),
                Err(AnnotationModelError::BodyFormatMismatch { .. })
            ));
        }
    }

    #[test]
    fn loaded_sets_can_add_replace_and_remove_without_losing_extensions() {
        let mut set = AnnotationSet::parse_json(
            r#"{
                "@context":"https://www.w3.org/ns/epub-anno.jsonld",
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "x-set":true,
                "items":[{
                    "id":"urn:test:original", "type":"Annotation", "created":"2026-07-16T12:00:00Z",
                    "target":{"source":"text/chapter.xhtml"}, "x-item":{"kept":true}
                }]
            }"#,
        )
        .unwrap();
        let added = annotation_with_body("added", AnnotationBody::text("note").unwrap());
        set.add_annotation(added.clone()).unwrap();
        let replacement =
            annotation_with_body("added", AnnotationBody::text("replacement").unwrap());
        let old = set.replace_annotation(replacement).unwrap();
        assert_eq!(old.id(), Some("urn:test:added"));
        set.remove_annotation("urn:test:added").unwrap();

        let value: Value = serde_json::from_str(&set.to_json_string().unwrap()).unwrap();
        assert_eq!(value["x-set"], true);
        assert_eq!(value[ITEMS][0]["x-item"]["kept"], true);
        assert_eq!(set.items().len(), 1);
    }

    #[test]
    fn editor_draft_refined_selector_example_round_trips() {
        // EPUB Annotations 1.0 editor's draft, refinement-of-selection example:
        // https://w3c.github.io/epub-specs/epub34/annotations/
        let set = AnnotationSet::parse_json(
            r##"{
                "@context":"https://www.w3.org/ns/epub-anno.jsonld",
                "id":"urn:uuid:set", "type":"AnnotationSet", "about":{},
                "items":[{
                    "id":"urn:uuid:annotation", "type":"Annotation",
                    "created":"2026-07-16T12:00:00Z",
                    "target":{"source":"text/chapter.xhtml","selector":[{
                        "type":"CssSelector", "value":"#intro > p:nth-child(2)",
                        "refinedBy":{"type":"TextPositionSelector","start":4,"end":19}
                    }]}
                }]
            }"##,
        )
        .unwrap();

        let reparsed = AnnotationSet::parse_json(&set.to_json_string().unwrap()).unwrap();
        assert_eq!(reparsed, set);
    }

    #[test]
    fn body_fields_that_do_not_apply_are_omitted_from_recovered_model() {
        for (body, field) in [
            (
                serde_json::json!({"type":"TextualBody","value":"note","id":"wrong.mp3"}),
                "/items/0/body/id",
            ),
            (
                serde_json::json!({"type":"Audio","id":"voice.mp3","value":"wrong"}),
                "/items/0/body/value",
            ),
        ] {
            let mut value = strict_set();
            value[ITEMS][0][BODY] = body;
            let set = AnnotationSet::from_json_value(value).unwrap();
            let pointer = field.strip_prefix("/items/0").unwrap();
            assert!(
                set.to_value()[ITEMS][0].pointer(pointer).is_none(),
                "{field}"
            );
        }
    }

    #[test]
    fn duplicate_id_mutation_is_ambiguous_and_transactional() {
        let mut set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[
                    {"id":"urn:test:same","type":"Annotation","created":"2026-07-16T12:00:00Z","target":{"source":"one.xhtml"}},
                    {"id":"urn:test:same","type":"Annotation","created":"2026-07-16T12:00:00Z","target":{"source":"two.xhtml"}}
                ]
            }"#,
        )
        .unwrap();
        let before = set.clone();

        assert!(matches!(
            set.remove_annotation("urn:test:same"),
            Err(AnnotationModelError::AmbiguousAnnotationId { .. })
        ));
        assert_eq!(set, before);
    }

    #[test]
    fn typed_creation_rejects_invalid_metadata_and_timestamp_order() {
        assert!(
            AnnotationAbout::builder()
                .format("not a media type".to_string())
                .build()
                .is_err()
        );
        assert!(
            AnnotationAbout::builder()
                .date("26".to_string())
                .build()
                .is_err()
        );
        assert!(
            AnnotationGenerator::builder()
                .id("https://example.com/app".to_string())
                .name("App".to_string())
                .homepage("relative/path".to_string())
                .build()
                .is_err()
        );

        let modified = OffsetDateTime::parse("2026-07-16T11:00:00Z", &Rfc3339).unwrap();
        assert!(matches!(
            Annotation::builder()
                .id("urn:uuid:annotation".to_string())
                .created(created_at())
                .modified(modified)
                .target(target_with(Vec::new()))
                .build(),
            Err(AnnotationModelError::ModifiedBeforeCreated)
        ));
    }

    #[test]
    fn missing_set_id_is_recovered_as_absent_model_state() {
        let set = AnnotationSet::parse_json(
            r#"{
                "type":"AnnotationSet", "about":{}, "items":[{
                    "id":"urn:test:missing", "type":"Annotation",
                    "created":"2026-07-16T12:00:00Z",
                    "target":{"source":"missing.xhtml"}
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(set.id(), None);
        assert!(set.to_json_string().is_ok());
    }

    #[test]
    fn identities_and_generator_homepage_use_absolute_urls() {
        for json_pointer in [
            "/id",
            "/items/0/id",
            "/generator/id",
            "/generator/homepage",
            "/items/0/creator/id",
        ] {
            let mut value = strict_set();
            value[GENERATOR] = serde_json::json!({
                "id":"urn:test:app", "type":"Software", "name":"App",
                "homepage":"https://example.com/app"
            });
            value[ITEMS][0][CREATOR] = serde_json::json!({
                "id":"urn:test:person",
                "type":"Person"
            });
            *value.pointer_mut(json_pointer).unwrap() = Value::String("relative".to_string());
            let set = AnnotationSet::from_json_value(value).unwrap();
            assert!(set.to_json_string().is_ok(), "{json_pointer}");
        }
        assert!(
            AnnotationSet::builder()
                .id("relative".to_string())
                .about(AnnotationAbout::builder().build().unwrap())
                .build()
                .is_err()
        );
    }

    #[test]
    fn publication_identifiers_remain_arbitrary_strings() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet",
                "about":{"dc:identifier":["ISBN 978 1 4028 9462 6",""]}, "items":[]
            }"#,
        )
        .unwrap();

        assert_eq!(
            set.about().unwrap().identifiers(),
            &["ISBN 978 1 4028 9462 6".to_string(), "".to_string()]
        );
        assert!(
            AnnotationAbout::builder()
                .identifiers(vec!["not a URL".to_string()])
                .build()
                .is_ok()
        );
    }

    #[test]
    fn localizable_text_uses_a_bcp47_parser_and_text_body_validation() {
        let invalid = LocalizableText::localized("note", Some("en-a".to_string()), None);
        assert!(AnnotationBody::text(invalid).is_err());

        let valid = LocalizableText::localized("nuqneH", Some("i-klingon".to_string()), None);
        assert!(AnnotationBody::text(valid).is_ok());
    }

    #[test]
    fn imported_empty_selectors_and_empty_text_positions_are_invalid() {
        let set = AnnotationSet::parse_json(
            r#"{
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[{
                    "id":"urn:test:a", "type":"Annotation", "created":"2026-07-16T12:00:00Z",
                    "target":{"source":"chapter.xhtml","selector":[
                        {"type":"FragmentSelector","value":"","x":1},
                        {"type":"CssSelector","value":""},
                        {"type":"TextPositionSelector","start":4,"end":4}
                    ]}
                }]
            }"#,
        )
        .unwrap();

        let selectors = set.items()[0].target().unwrap().selectors();
        let selector = &selectors[0];
        let AnnotationSelector::Fragment(fragment) = selector else {
            panic!("expected fragment selector");
        };
        assert_eq!(fragment.value(), "");
        assert_eq!(fragment.extra()["x"], 1);
        let AnnotationSelector::Css(css) = &selectors[1] else {
            panic!("expected CSS selector");
        };
        assert_eq!(css.value(), "");
        let AnnotationSelector::TextPosition(position) = &selectors[2] else {
            panic!("expected text position selector");
        };
        assert_eq!((position.start(), position.end()), (Some(4), Some(4)));

        let empty = TextRange::new(4, 4).unwrap();
        assert!(
            TextPositionSelector::builder()
                .range(empty)
                .build()
                .is_err()
        );
    }

    #[test]
    fn normalized_set_context_keeps_extensions_and_adds_epub_context() {
        let set = AnnotationSet::parse_json(
            r#"{
                "@context":["https://example.org/context",{"ex":"https://example.org/"}],
                "id":"urn:test:set", "type":"AnnotationSet", "about":{}, "items":[]
            }"#,
        )
        .unwrap();
        let context = set.to_value()[CONTEXT].as_array().unwrap().clone();

        assert_eq!(context[0], EPUB_ANNOTATIONS_CONTEXT);
        assert_eq!(context[1], "https://example.org/context");
        assert_eq!(context[2]["ex"], "https://example.org/");
    }

    #[test]
    fn invalid_model_construction_returns_direct_field_error() {
        let annotation = Annotation::from_value(serde_json::json!({"type":"Annotation"})).unwrap();
        let error = AnnotationSet::builder()
            .id("urn:test:set".to_string())
            .about(AnnotationAbout::builder().build().unwrap())
            .items(vec![annotation])
            .build()
            .unwrap_err();
        assert!(matches!(
            error,
            AnnotationModelError::MissingField { field: "id" }
        ));

        let annotation = Annotation::from_value(serde_json::json!({
            "id":"urn:test:a", "type":"Annotation",
            "created":"2026-07-16T12:00:00Z",
            "target":{"source":"https://example.com/chapter.xhtml"}
        }))
        .unwrap();
        let error = AnnotationSet::builder()
            .id("urn:test:set".to_string())
            .about(AnnotationAbout::builder().build().unwrap())
            .items(vec![annotation])
            .build()
            .unwrap_err();
        assert!(matches!(
            error,
            AnnotationModelError::InvalidTargetSource { .. }
        ));

        let annotation = Annotation::from_value(serde_json::json!({
            "id":"urn:test:a", "type":"Annotation",
            "created":"2026-07-16T12:00:00Z",
            "target":{"source":"chapter.xhtml"},
            "body":{"type":"FutureBody"}
        }))
        .unwrap();
        let error = AnnotationSet::builder()
            .id("urn:test:set".to_string())
            .about(AnnotationAbout::builder().build().unwrap())
            .items(vec![annotation])
            .build()
            .unwrap_err();
        assert!(matches!(
            error,
            AnnotationModelError::MissingField { field: "body type" }
        ));
    }
}
