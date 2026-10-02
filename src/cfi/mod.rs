//! Parse, build, format, and resolve EPUB Canonical Fragment Identifiers.
//!
//! Use [`Cfi::parse`] for a complete `epubcfi(...)` fragment, or construct one from [`CfiPath`],
//! [`Step`], [`Offset`], and [`Assertion`]. [`CfiRange`] represents a common parent with relative
//! start and end paths. [`Display`] produces CFI syntax; [`crate::Epub::resolve_cfi`] resolves it.
//!
//! Parsing and construction report invalid CFI syntax as [`CfiError`]. Resolving a CFI against a
//! publication reports resource or document traversal failures as [`CfiResolveError`].
use core::fmt;
use nom::{
    Finish, IResult, Parser,
    branch::alt,
    bytes::complete::tag,
    character::complete::digit1,
    combinator::{all_consuming, map, opt},
    error::{Error, ErrorKind},
    multi::{many0, many1},
    sequence::delimited,
};
use std::fmt::{Display, Formatter};

mod resolution;

/// Live CFI resolution errors and successful point/range results.
pub use resolution::{
    AssertionMismatch, CfiResolveError, ContentDocumentFailure, ContentPathFailure, OffsetFailure,
    PackagePathFailure, ResolvedCfi, ResolvedCfiLocation, ResolvedCfiPoint, ResolvedCfiRange,
};

type Result<T> = std::result::Result<T, CfiError>;

/// A CFI syntax or construction failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CfiError {
    /// A full CFI path had no steps.
    #[error("CFI path must include at least one step")]
    EmptyPath,
    /// A normal step was zero.
    #[error("CFI step must be greater than zero: {step}")]
    InvalidStep {
        /// Rejected step number.
        step: usize,
    },
    /// Spatial coordinates were non-finite, non-canonical, or outside 0 through 100.
    #[error("Invalid spatial offset: x={x}, y={y}")]
    InvalidSpatialOffset {
        /// Rejected horizontal percentage.
        x: f64,
        /// Rejected vertical percentage.
        y: f64,
    },
    /// A temporal value was non-finite or not representable in canonical CFI number syntax.
    #[error("Invalid temporal offset: {value}")]
    InvalidTemporalOffset {
        /// Rejected temporal value.
        value: f64,
    },
    /// An assertion was empty.
    #[error("Invalid CFI assertion")]
    InvalidAssertion,
    /// An extension parameter had an empty/invalid name or value list.
    #[error("Invalid CFI assertion parameter")]
    InvalidParameter,
    /// An otherwise valid assertion appeared on an incompatible step or offset.
    #[error("CFI assertion is not supported at this location")]
    UnsupportedAssertionPlacement,
    /// A range parent or endpoint contained a side-bias parameter.
    #[error("CFI range endpoints cannot include side-bias parameters")]
    RangeSideBiasUnsupported,
    /// Text did not parse as complete CFI syntax.
    #[error("CFI parse error: {source}")]
    Nom {
        /// Parser position and error kind, owning the remaining input.
        #[from]
        source: nom::error::Error<String>,
    },
}

fn fragment(input: &str) -> IResult<&str, Cfi> {
    let (input, path) = path(input)?;
    let (input, range) = opt(range_suffix).parse(input)?;
    let cfi = match range {
        Some((start, end)) => Cfi::Range(CfiRange {
            parent: path,
            start,
            end,
        }),
        None => Cfi::Point(path),
    };
    Ok((input, cfi))
}

fn range_suffix(input: &str) -> IResult<&str, (LocalPath, LocalPath)> {
    let (input, _) = tag(",").parse(input)?;
    let (input, start) = local_path(input)?;
    let (input, _) = tag(",").parse(input)?;
    let (input, end) = local_path(input)?;
    Ok((input, (start, end)))
}

fn epubcfi(input: &str) -> IResult<&str, Cfi> {
    delimited(tag("epubcfi("), fragment, tag(")")).parse(input)
}

fn range(input: &str) -> IResult<&str, CfiRange> {
    let (input, parent) = path(input)?;
    let (input, (start, end)) = range_suffix(input)?;
    Ok((input, CfiRange { parent, start, end }))
}

fn path(input: &str) -> IResult<&str, CfiPath> {
    let (input, first_step) = step(input)?;
    let (input, local) = local_path(input)?;
    let mut steps = vec![first_step];
    steps.extend(local.steps);
    Ok((
        input,
        CfiPath {
            path: LocalPath {
                steps,
                tail: local.tail,
            },
        },
    ))
}

fn local_path(input: &str) -> IResult<&str, LocalPath> {
    let (input, steps) = many0(step).parse(input)?;
    if let Ok((input, redirected)) = redirected_path(input) {
        return Ok((
            input,
            LocalPath {
                steps,
                tail: LocalPathTail::Redirect(redirected),
            },
        ));
    }
    let (input, offset) = opt(offset).parse(input)?;
    Ok((
        input,
        LocalPath {
            steps,
            tail: match offset {
                Some(offset) => LocalPathTail::Offset(offset),
                None => LocalPathTail::None,
            },
        },
    ))
}

fn redirected_path(input: &str) -> IResult<&str, RedirectedPath> {
    let (input, _) = tag("!").parse(input)?;
    alt((
        map(offset, RedirectedPath::Offset),
        map(path, |path| RedirectedPath::Path(Box::new(path))),
    ))
    .parse(input)
}

fn integer(input: &str) -> IResult<&str, usize> {
    let (input, digits) = digit1(input)?;
    if digits.len() > 1 && digits.starts_with('0') {
        return Err(nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        }));
    }
    let value = digits.parse().map_err(|_| {
        nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        })
    })?;
    Ok((input, value))
}

fn number(input: &str) -> IResult<&str, f64> {
    let (input, integer_digits) = digit1(input)?;
    if integer_digits.len() > 1 && integer_digits.starts_with('0') {
        return Err(nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        }));
    }
    let mut number_string = integer_digits.to_string();
    if let Some(rest) = input.strip_prefix('.') {
        let (remaining, frac_digits) = digit1(rest)?;
        if frac_digits.ends_with('0') {
            return Err(nom::Err::Error(Error {
                input,
                code: ErrorKind::Fail,
            }));
        }
        number_string.push('.');
        number_string.push_str(frac_digits);
        let value = number_string.parse().map_err(|_| {
            nom::Err::Error(Error {
                input,
                code: ErrorKind::Fail,
            })
        })?;
        return Ok((remaining, value));
    }
    let value = number_string.parse().map_err(|_| {
        nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        })
    })?;
    Ok((input, value))
}

fn step(input: &str) -> IResult<&str, Step> {
    let (input, _) = tag("/").parse(input)?;
    let (input, step_value) = integer(input)?;
    let (input, assertion) = opt(step_assertion).parse(input)?;
    Ok((
        input,
        Step {
            step: step_value,
            assertion,
        },
    ))
}

fn offset(input: &str) -> IResult<&str, Offset> {
    alt((temporal_spatial, temporal, spatial, character_offset)).parse(input)
}

fn character_offset(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag(":").parse(input)?;
    let (input, value) = integer(input)?;
    let (input, assertion) = opt(text_assertion).parse(input)?;
    Ok((
        input,
        Offset {
            value: OffsetValue::Character { value, assertion },
        },
    ))
}

fn spatial(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag("@").parse(input)?;
    let (input, x) = number(input)?;
    let (input, _) = tag(":").parse(input)?;
    let (input, y) = number(input)?;
    if x > 100.0 || y > 100.0 {
        return Err(nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        }));
    }
    Ok((
        input,
        Offset {
            value: OffsetValue::Spatial { x, y },
        },
    ))
}

fn temporal(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag("~").parse(input)?;
    let (input, value) = number(input)?;
    Ok((
        input,
        Offset {
            value: OffsetValue::Temporal { value },
        },
    ))
}

fn temporal_spatial(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag("~").parse(input)?;
    let (input, temporal_value) = number(input)?;
    let (input, _) = tag("@").parse(input)?;
    let (input, x) = number(input)?;
    let (input, _) = tag(":").parse(input)?;
    let (input, y) = number(input)?;
    if x > 100.0 || y > 100.0 {
        return Err(nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        }));
    }
    Ok((
        input,
        Offset {
            value: OffsetValue::TemporalSpatial {
                temporal: temporal_value,
                x,
                y,
            },
        },
    ))
}

fn step_assertion(input: &str) -> IResult<&str, Assertion> {
    delimited(tag("["), step_assertion_body, tag("]")).parse(input)
}

fn step_assertion_body(input: &str) -> IResult<&str, Assertion> {
    if input.starts_with(';') {
        let (input, parameters) = many1(parameter).parse(input)?;
        return Ok((input, Assertion::Parameters(parameters)));
    }
    let (input, value) = value(input)?;
    let (input, parameters) = many0(parameter).parse(input)?;
    Ok((input, Assertion::Id { value, parameters }))
}

fn text_assertion(input: &str) -> IResult<&str, Assertion> {
    delimited(tag("["), text_assertion_body, tag("]")).parse(input)
}

fn text_assertion_body(input: &str) -> IResult<&str, Assertion> {
    if let Some(rest) = input.strip_prefix(',') {
        let (rest, after) = value(rest)?;
        let (rest, parameters) = many0(parameter).parse(rest)?;
        return Ok((
            rest,
            Assertion::Text {
                before: None,
                after: Some(after),
                parameters,
            },
        ));
    }
    if input.starts_with(';') {
        let (input, parameters) = many1(parameter).parse(input)?;
        return Ok((input, Assertion::Parameters(parameters)));
    }
    let (input, before) = value(input)?;
    let (input, after) = opt(|input| {
        let (input, _) = tag(",").parse(input)?;
        value(input)
    })
    .parse(input)?;
    let (input, parameters) = many0(parameter).parse(input)?;
    Ok((
        input,
        Assertion::Text {
            before: Some(before),
            after,
            parameters,
        },
    ))
}

fn parameter(input: &str) -> IResult<&str, Parameter> {
    let (input, _) = tag(";").parse(input)?;
    let (input, name) = value_no_space(input)?;
    let (input, _) = tag("=").parse(input)?;
    let (input, values) = csv(input)?;
    let param = match (name.as_str(), values.as_slice()) {
        ("s", [value]) if value == "b" => Parameter::side_bias(SideBias::Before),
        ("s", [value]) if value == "a" => Parameter::side_bias(SideBias::After),
        _ => Parameter {
            value: ParameterValue::Unknown { name, csv: values },
        },
    };
    Ok((input, param))
}

fn csv(input: &str) -> IResult<&str, Vec<String>> {
    let (input, first) = value(input)?;
    let (input, mut rest) = many0(|input| {
        let (input, _) = tag(",").parse(input)?;
        value(input)
    })
    .parse(input)?;
    let mut values = vec![first];
    values.append(&mut rest);
    Ok((input, values))
}

fn value(input: &str) -> IResult<&str, String> {
    parse_value(input, true, false)
}

fn value_no_space(input: &str) -> IResult<&str, String> {
    parse_value(input, false, true)
}

fn parse_value(input: &str, allow_space: bool, stop_on_equal: bool) -> IResult<&str, String> {
    let mut output = String::new();
    let mut end_index = 0;
    let mut chars = input.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if ch == ',' || ch == ';' || ch == ']' || (stop_on_equal && ch == '=') {
            end_index = index;
            break;
        }
        if ch == '^' {
            let Some((next_index, next_char)) = chars.next() else {
                return Err(nom::Err::Error(Error {
                    input,
                    code: ErrorKind::Escaped,
                }));
            };
            if !is_special_char(next_char) {
                return Err(nom::Err::Error(Error {
                    input,
                    code: ErrorKind::Escaped,
                }));
            }
            output.push(next_char);
            end_index = next_index + next_char.len_utf8();
            continue;
        }
        if ch == '(' || ch == ')' || ch == '[' {
            return Err(nom::Err::Error(Error {
                input,
                code: ErrorKind::Fail,
            }));
        }
        if !allow_space && ch == ' ' {
            return Err(nom::Err::Error(Error {
                input,
                code: ErrorKind::Fail,
            }));
        }
        if is_special_char(ch) {
            return Err(nom::Err::Error(Error {
                input,
                code: ErrorKind::Fail,
            }));
        }
        output.push(ch);
        end_index = index + ch.len_utf8();
    }

    if output.is_empty() {
        return Err(nom::Err::Error(Error {
            input,
            code: ErrorKind::Fail,
        }));
    }

    let remaining = &input[end_index..];
    Ok((remaining, output))
}

fn is_special_char(ch: char) -> bool {
    matches!(ch, '^' | '[' | ']' | '(' | ')' | ',' | ';' | '=')
}

/// A parsed EPUB CFI: either a single point in a book, or a span between two.
#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Cfi {
    /// A single location.
    Point(
        /// Absolute path to the location.
        CfiPath,
    ),
    /// A range between two locations sharing a common parent.
    Range(
        /// Common parent and relative endpoints.
        CfiRange,
    ),
}

impl Cfi {
    /// Parses a complete `epubcfi(...)` fragment.
    pub fn parse(input: &str) -> Result<Self> {
        input.parse()
    }

    /// The point path, or the common parent path of a range.
    pub fn path(&self) -> &CfiPath {
        match self {
            Self::Point(path) => path,
            Self::Range(range) => range.parent(),
        }
    }
}

impl From<CfiPath> for Cfi {
    fn from(path: CfiPath) -> Self {
        Self::Point(path)
    }
}

impl From<CfiRange> for Cfi {
    fn from(range: CfiRange) -> Self {
        Self::Range(range)
    }
}

impl std::str::FromStr for Cfi {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(epubcfi).parse(input).finish() {
            Ok((_, cfi)) => {
                match &cfi {
                    Cfi::Point(path) => validate_cfi_path(path)?,
                    Cfi::Range(range) => range.validate()?,
                }
                Ok(cfi)
            }
            Err(Error { input, code }) => Err(CfiError::Nom {
                source: Error {
                    input: input.to_string(),
                    code,
                },
            }),
        }
    }
}

impl Display for Cfi {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Point(path) => write!(f, "epubcfi({path})"),
            Self::Range(range) => write!(f, "epubcfi({range})"),
        }
    }
}

/// A non-empty absolute CFI path, including its optional offset or redirect tail.
#[derive(Debug, PartialEq, Clone)]
pub struct CfiPath {
    path: LocalPath,
}

impl CfiPath {
    /// Builds a non-empty path and validates step and tail assertion placement recursively.
    pub fn new(steps: Vec<Step>, tail: LocalPathTail) -> Result<Self> {
        if steps.is_empty() {
            return Err(CfiError::EmptyPath);
        }
        let path = Self {
            path: LocalPath { steps, tail },
        };
        validate_cfi_path(&path)?;
        Ok(path)
    }

    /// Path steps in traversal order.
    pub fn steps(&self) -> &[Step] {
        self.path.steps()
    }

    /// The terminal offset or redirect representation.
    pub fn tail(&self) -> &LocalPathTail {
        self.path.tail()
    }

    /// Replaces the tail and revalidates recursive assertion placement.
    pub fn with_tail(mut self, tail: LocalPathTail) -> Result<Self> {
        self.path.tail = tail;
        validate_cfi_path(&self)?;
        Ok(self)
    }

    /// The direct terminal offset; redirected offsets are not included.
    pub fn offset(&self) -> Option<&Offset> {
        self.path.offset()
    }

    /// The path following `!`, if present.
    pub fn redirected(&self) -> Option<&RedirectedPath> {
        self.path.redirected()
    }

    pub(crate) fn as_local_path(&self) -> &LocalPath {
        &self.path
    }

    /// Whether this un-offset path ends on an even element step.
    pub fn refers_to_element(&self) -> bool {
        self.offset().is_none()
            && self
                .steps()
                .last()
                .map(|step| step.step % 2 == 0)
                .unwrap_or(false)
    }
}

impl Display for CfiPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.path.fmt(f)
    }
}

impl std::str::FromStr for CfiPath {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(path).parse(input).finish() {
            Ok((_, path)) => {
                validate_cfi_path(&path)?;
                Ok(path)
            }
            Err(Error { input, code }) => Err(CfiError::Nom {
                source: Error {
                    input: input.to_string(),
                    code,
                },
            }),
        }
    }
}

/// A relative CFI path used after a redirect or as a range endpoint.
///
/// The virtual `/0` step is accepted only as a terminal, unasserted, un-offset step.
#[derive(Debug, PartialEq, Clone)]
pub struct LocalPath {
    steps: Vec<Step>,
    tail: LocalPathTail,
}

impl LocalPath {
    /// Builds a local path and validates step and tail assertion placement.
    pub fn new(steps: Vec<Step>, tail: LocalPathTail) -> Result<Self> {
        let path = Self { steps, tail };
        validate_local_path(&path)?;
        Ok(path)
    }

    /// Relative steps in traversal order.
    pub fn steps(&self) -> &[Step] {
        self.steps.as_slice()
    }

    /// The terminal offset or redirect representation.
    pub fn tail(&self) -> &LocalPathTail {
        &self.tail
    }
    /// The direct terminal offset; redirected offsets are not included.
    pub fn offset(&self) -> Option<&Offset> {
        self.tail.offset()
    }
    /// The path following `!`, if present.
    pub fn redirected(&self) -> Option<&RedirectedPath> {
        self.tail.redirected()
    }
}

impl Display for LocalPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for step in self.steps() {
            write!(f, "{step}")?;
        }
        write!(f, "{}", self.tail)
    }
}

impl std::str::FromStr for LocalPath {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(local_path).parse(input).finish() {
            Ok((_, local)) => {
                validate_local_path(&local)?;
                Ok(local)
            }
            Err(Error { input, code }) => Err(CfiError::Nom {
                source: Error {
                    input: input.to_string(),
                    code,
                },
            }),
        }
    }
}

/// A CFI range represented by a common parent and relative start/end paths.
///
/// Construction checks path shape and rejects side bias. Resolution checks endpoint order.
#[derive(Debug, PartialEq, Clone)]
pub struct CfiRange {
    parent: CfiPath,
    start: LocalPath,
    end: LocalPath,
}

impl CfiRange {
    /// Constructs a range from validated parts, rejecting side-bias parameters.
    pub fn new(parent: CfiPath, start: LocalPath, end: LocalPath) -> Result<Self> {
        let range = Self { parent, start, end };
        if path_has_side_bias(&range.parent)
            || local_path_has_side_bias(&range.start)
            || local_path_has_side_bias(&range.end)
        {
            return Err(CfiError::RangeSideBiasUnsupported);
        }
        Ok(range)
    }

    fn validate(&self) -> Result<()> {
        validate_cfi_path(&self.parent)?;
        validate_local_path(&self.start)?;
        validate_local_path(&self.end)?;
        if path_has_side_bias(&self.parent)
            || local_path_has_side_bias(&self.start)
            || local_path_has_side_bias(&self.end)
        {
            return Err(CfiError::RangeSideBiasUnsupported);
        }
        Ok(())
    }

    /// The common absolute parent path.
    pub fn parent(&self) -> &CfiPath {
        &self.parent
    }
    /// The relative inclusive start endpoint.
    pub fn start(&self) -> &LocalPath {
        &self.start
    }
    /// The relative exclusive end endpoint.
    pub fn end(&self) -> &LocalPath {
        &self.end
    }
}

impl std::str::FromStr for CfiRange {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(range).parse(input).finish() {
            Ok((_, range)) => {
                range.validate()?;
                Ok(range)
            }
            Err(Error { input, code }) => Err(CfiError::Nom {
                source: Error {
                    input: input.to_string(),
                    code,
                },
            }),
        }
    }
}

impl Display for CfiRange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{}", self.parent(), self.start(), self.end())
    }
}

/// Terminal state of a full or local path.
#[derive(Debug, PartialEq, Clone, Default)]
pub enum LocalPathTail {
    /// No offset and no indirection.
    #[default]
    None,
    /// One terminal offset.
    Offset(
        /// Terminal offset.
        Offset,
    ),
    /// Indirection through `!`.
    Redirect(
        /// Redirect target.
        RedirectedPath,
    ),
}

impl LocalPathTail {
    /// The direct offset, if this tail contains one.
    pub fn offset(&self) -> Option<&Offset> {
        match self {
            LocalPathTail::Offset(offset) => Some(offset),
            LocalPathTail::None | LocalPathTail::Redirect(_) => None,
        }
    }

    /// The redirect target, if this is a redirect tail.
    pub fn redirected(&self) -> Option<&RedirectedPath> {
        match self {
            LocalPathTail::Redirect(path) => Some(path),
            LocalPathTail::None | LocalPathTail::Offset(_) => None,
        }
    }
}

impl Display for LocalPathTail {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LocalPathTail::None => Ok(()),
            LocalPathTail::Offset(offset) => write!(f, "{offset}"),
            LocalPathTail::Redirect(path) => write!(f, "!{path}"),
        }
    }
}

/// Target following a CFI `!` indirection.
#[derive(Debug, PartialEq, Clone)]
pub enum RedirectedPath {
    /// Redirect directly to an offset.
    Offset(
        /// Redirected offset.
        Offset,
    ),
    /// Redirect to another non-empty path.
    Path(
        /// Redirected path.
        Box<CfiPath>,
    ),
}

impl Display for RedirectedPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RedirectedPath::Offset(offset) => write!(f, "{offset}"),
            RedirectedPath::Path(path) => write!(f, "{path}"),
        }
    }
}

/// One numeric CFI traversal step with an optional assertion.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Step {
    /// Child Elements are even indexed, starting with 2
    step: usize,
    assertion: Option<Assertion>,
}

impl Step {
    /// Creates a positive ordinary step and validates assertion placement.
    pub fn new(step: usize, assertion: Option<Assertion>) -> Result<Self> {
        if step == 0 {
            return Err(CfiError::InvalidStep { step });
        }
        let step = Self { step, assertion };
        validate_steps(std::slice::from_ref(&step))?;
        Ok(step)
    }

    /// Creates the virtual `/0` step accepted only as a terminal range boundary.
    ///
    /// Passing this value to an ordinary path constructor is an error.
    pub fn virtual_boundary() -> Self {
        Self {
            step: 0,
            assertion: None,
        }
    }
    /// The numeric step, including zero for a virtual boundary.
    pub fn step(&self) -> usize {
        self.step
    }
    /// The step assertion.
    pub fn assertion(&self) -> Option<&Assertion> {
        self.assertion.as_ref()
    }
    /// Adds or replaces the assertion and validates its placement on this step.
    pub fn with_assertion(mut self, assertion: Assertion) -> Result<Self> {
        self.assertion = Some(assertion);
        validate_steps(std::slice::from_ref(&self))?;
        Ok(self)
    }
}

impl Display for Step {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.step)?;
        match &self.assertion {
            Some(assertion) => assertion.fmt(f),
            None => Ok(()),
        }
    }
}

/// A character, temporal, spatial, or combined temporal-spatial CFI offset.
#[derive(Debug, PartialEq, Clone)]
pub struct Offset {
    value: OffsetValue,
}

#[derive(Debug, PartialEq, Clone)]
enum OffsetValue {
    Character {
        value: usize,
        assertion: Option<Assertion>,
    },
    Temporal {
        value: f64,
    },
    Spatial {
        x: f64,
        y: f64,
    },
    TemporalSpatial {
        temporal: f64,
        x: f64,
        y: f64,
    },
}

impl Offset {
    /// Constructs a character offset with an optional text or parameter assertion.
    pub fn character(value: usize, assertion: Option<Assertion>) -> Result<Self> {
        let offset = Self {
            value: OffsetValue::Character { value, assertion },
        };
        validate_offset_assertion(&offset)?;
        Ok(offset)
    }

    /// Constructs a non-negative canonical temporal offset.
    pub fn temporal(value: f64) -> Result<Self> {
        validate_temporal(value)?;
        Ok(Self {
            value: OffsetValue::Temporal { value },
        })
    }

    /// Constructs spatial percentage coordinates in the inclusive range 0 through 100.
    pub fn spatial(x: f64, y: f64) -> Result<Self> {
        validate_spatial(x, y)?;
        Ok(Self {
            value: OffsetValue::Spatial { x, y },
        })
    }

    /// Constructs a canonical temporal offset with bounded spatial percentages.
    pub fn temporal_spatial(temporal: f64, x: f64, y: f64) -> Result<Self> {
        validate_temporal(temporal)?;
        validate_spatial(x, y)?;
        Ok(Self {
            value: OffsetValue::TemporalSpatial { temporal, x, y },
        })
    }

    /// The character value and optional assertion, if this is a character offset.
    pub fn as_character(&self) -> Option<(usize, Option<&Assertion>)> {
        match &self.value {
            OffsetValue::Character { value, assertion } => Some((*value, assertion.as_ref())),
            _ => None,
        }
    }

    /// The temporal component of temporal and combined offsets.
    pub fn temporal_value(&self) -> Option<f64> {
        match &self.value {
            OffsetValue::Temporal { value }
            | OffsetValue::TemporalSpatial {
                temporal: value, ..
            } => Some(*value),
            _ => None,
        }
    }

    /// The `(x, y)` coordinates for spatial and combined offsets.
    pub fn spatial_value(&self) -> Option<(f64, f64)> {
        match &self.value {
            OffsetValue::Spatial { x, y } | OffsetValue::TemporalSpatial { x, y, .. } => {
                Some((*x, *y))
            }
            _ => None,
        }
    }
}

impl Display for Offset {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.value {
            OffsetValue::Character { value, assertion } => {
                write!(f, ":{value}")?;
                match assertion {
                    Some(assertion) => assertion.fmt(f),
                    None => Ok(()),
                }
            }
            OffsetValue::Temporal { value } => write!(f, "~{value}"),
            OffsetValue::Spatial { x, y } => write!(f, "@{x}:{y}"),
            OffsetValue::TemporalSpatial { temporal, x, y } => {
                write!(f, "~{temporal}@{x}:{y}")
            }
        }
    }
}

/// An assertion attached to a step or character offset.
///
/// The variant states where the assertion is valid: [`Self::Id`] asserts the element ID of an even
/// element step, and [`Self::Text`] asserts the source text around a character offset.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Assertion {
    /// An element ID assertion on an even element step.
    Id {
        /// Asserted element ID.
        value: String,
        /// Extension parameters in source order.
        parameters: Vec<Parameter>,
    },
    /// Preceding and following source text around a character offset.
    Text {
        /// Text expected to precede the offset.
        before: Option<String>,
        /// Text expected to follow the offset.
        after: Option<String>,
        /// Extension parameters in source order.
        parameters: Vec<Parameter>,
    },
    /// Parameters with no assertion value.
    Parameters(
        /// Extension parameters in source order.
        Vec<Parameter>,
    ),
}

impl Assertion {
    /// Constructs an element ID assertion.
    pub fn id(value: impl Into<String>) -> Result<Self> {
        let assertion = Self::Id {
            value: value.into(),
            parameters: Vec::new(),
        };
        validate_assertion(&assertion)?;
        Ok(assertion)
    }

    /// Constructs a preceding/following text assertion; at least one side must be present.
    pub fn text(
        before: Option<impl Into<String>>,
        after: Option<impl Into<String>>,
    ) -> Result<Self> {
        let assertion = Self::Text {
            before: before.map(Into::into),
            after: after.map(Into::into),
            parameters: Vec::new(),
        };
        validate_assertion(&assertion)?;
        Ok(assertion)
    }

    /// Constructs an assertion containing parameters but no assertion value.
    pub fn parameters(parameters: Vec<Parameter>) -> Result<Self> {
        let assertion = Self::Parameters(parameters);
        validate_assertion(&assertion)?;
        Ok(assertion)
    }

    /// Adds parameters to an ID or text assertion.
    pub fn with_parameters(mut self, added: Vec<Parameter>) -> Result<Self> {
        match &mut self {
            Self::Id { parameters, .. }
            | Self::Text { parameters, .. }
            | Self::Parameters(parameters) => parameters.extend(added),
        }
        validate_assertion(&self)?;
        Ok(self)
    }

    /// Assertion parameters in source order.
    pub fn parameter_values(&self) -> &[Parameter] {
        match self {
            Self::Id { parameters, .. }
            | Self::Text { parameters, .. }
            | Self::Parameters(parameters) => parameters.as_slice(),
        }
    }

    /// The asserted element ID, if this is an ID assertion.
    pub fn id_value(&self) -> Option<&str> {
        match self {
            Self::Id { value, .. } => Some(value.as_str()),
            Self::Text { .. } | Self::Parameters(_) => None,
        }
    }
}

impl Display for Assertion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[")?;
        match self {
            Self::Id { value, .. } => write!(f, "{}", escape_value(value))?,
            Self::Text { before, after, .. } => {
                if let Some(before) = before {
                    write!(f, "{}", escape_value(before))?;
                }
                if let Some(after) = after {
                    write!(f, ",{}", escape_value(after))?;
                }
            }
            Self::Parameters(_) => {}
        }
        for parameter in self.parameter_values() {
            parameter.fmt(f)?;
        }
        write!(f, "]")
    }
}

/// A CFI assertion parameter.
///
/// Side bias is the standardized parameter; unknown name/CSV pairs preserve implementation-defined
/// extensions and are escaped during formatting.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Parameter {
    value: ParameterValue,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum ParameterValue {
    SideBias(SideBias),
    Unknown { name: String, csv: Vec<String> },
}

impl Parameter {
    /// Constructs the standardized side-bias parameter.
    pub fn side_bias(side_bias: SideBias) -> Self {
        Self {
            value: ParameterValue::SideBias(side_bias),
        }
    }

    /// Constructs an extension parameter with a non-empty, space-free name and non-empty values.
    pub fn unknown(name: impl Into<String>, csv: Vec<String>) -> Result<Self> {
        let parameter = Self {
            value: ParameterValue::Unknown {
                name: name.into(),
                csv,
            },
        };
        validate_parameter(&parameter)?;
        Ok(parameter)
    }

    /// The standardized side-bias value, if this is that parameter.
    pub fn side_bias_value(&self) -> Option<SideBias> {
        match self.value {
            ParameterValue::SideBias(value) => Some(value),
            ParameterValue::Unknown { .. } => None,
        }
    }

    /// The unescaped name and values of an extension parameter.
    pub fn unknown_value(&self) -> Option<(&str, &[String])> {
        match &self.value {
            ParameterValue::Unknown { name, csv } => Some((name.as_str(), csv.as_slice())),
            ParameterValue::SideBias(_) => None,
        }
    }
}

impl Display for Parameter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.value {
            ParameterValue::SideBias(x) => x.fmt(f),
            ParameterValue::Unknown { name, csv } => write!(
                f,
                ";{}={}",
                escape_value(name),
                csv.iter()
                    .map(|value| escape_value(value))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

fn validate_cfi_path(path: &CfiPath) -> Result<()> {
    if path.steps().is_empty() {
        return Err(CfiError::EmptyPath);
    }
    if let Some(step) = path.steps().iter().find(|step| step.step() == 0) {
        return Err(CfiError::InvalidStep { step: step.step() });
    }
    validate_local_path(path.as_local_path())
}

fn validate_local_path(path: &LocalPath) -> Result<()> {
    validate_steps(path.steps())?;
    validate_tail_assertions(path.tail())?;
    if path.steps().last().is_some_and(|step| step.step() == 0) && path.offset().is_some() {
        return Err(CfiError::InvalidStep { step: 0 });
    }
    Ok(())
}

fn validate_steps(steps: &[Step]) -> Result<()> {
    for (index, step) in steps.iter().enumerate() {
        if step.step() == 0 {
            if index != steps.len().saturating_sub(1) {
                return Err(CfiError::InvalidStep { step: 0 });
            }
            if step.assertion().is_some() {
                return Err(CfiError::UnsupportedAssertionPlacement);
            }
            continue;
        }
        let Some(assertion) = step.assertion() else {
            continue;
        };
        validate_assertion(assertion)?;
        match assertion {
            Assertion::Id { .. } if step.step() % 2 != 0 => {
                return Err(CfiError::UnsupportedAssertionPlacement);
            }
            Assertion::Text { .. } => return Err(CfiError::UnsupportedAssertionPlacement),
            _ => {}
        }
    }
    Ok(())
}

fn assertion_has_side_bias(assertion: &Assertion) -> bool {
    assertion
        .parameter_values()
        .iter()
        .any(|parameter| parameter.side_bias_value().is_some())
}

fn validate_tail_assertions(tail: &LocalPathTail) -> Result<()> {
    match tail {
        LocalPathTail::Offset(offset) | LocalPathTail::Redirect(RedirectedPath::Offset(offset)) => {
            validate_offset_assertion(offset)?;
        }
        LocalPathTail::Redirect(RedirectedPath::Path(path)) => validate_cfi_path(path)?,
        LocalPathTail::None => {}
    }
    Ok(())
}

fn validate_offset_assertion(offset: &Offset) -> Result<()> {
    let OffsetValue::Character {
        assertion: Some(assertion),
        ..
    } = &offset.value
    else {
        return Ok(());
    };
    validate_assertion(assertion)?;
    if matches!(assertion, Assertion::Id { .. }) {
        return Err(CfiError::UnsupportedAssertionPlacement);
    }
    Ok(())
}

fn validate_assertion(assertion: &Assertion) -> Result<()> {
    match assertion {
        Assertion::Id { value, .. } => {
            if value.is_empty() {
                return Err(CfiError::UnsupportedAssertionPlacement);
            }
        }
        Assertion::Text { before, after, .. } => {
            if before.is_none() && after.is_none() {
                return Err(CfiError::InvalidAssertion);
            }
            if before.iter().chain(after).any(|value| value.is_empty()) {
                return Err(CfiError::UnsupportedAssertionPlacement);
            }
        }
        Assertion::Parameters(parameters) => {
            if parameters.is_empty() {
                return Err(CfiError::InvalidAssertion);
            }
        }
    }
    for parameter in assertion.parameter_values() {
        validate_parameter(parameter)?;
    }
    Ok(())
}

fn validate_parameter(parameter: &Parameter) -> Result<()> {
    let ParameterValue::Unknown { name, csv } = &parameter.value else {
        return Ok(());
    };
    if name.is_empty() || name.contains(' ') || csv.is_empty() || csv.iter().any(String::is_empty) {
        return Err(CfiError::InvalidParameter);
    }
    Ok(())
}

fn validate_temporal(value: f64) -> Result<()> {
    if !is_formattable_number(value) {
        return Err(CfiError::InvalidTemporalOffset { value });
    }
    Ok(())
}

fn validate_spatial(x: f64, y: f64) -> Result<()> {
    if !is_formattable_number(x) || !is_formattable_number(y) || x > 100.0 || y > 100.0 {
        return Err(CfiError::InvalidSpatialOffset { x, y });
    }
    Ok(())
}

fn is_formattable_number(value: f64) -> bool {
    let formatted = value.to_string();
    all_consuming(number).parse(formatted.as_str()).is_ok()
}

fn path_has_side_bias(path: &CfiPath) -> bool {
    local_path_has_side_bias(path.as_local_path())
}

fn local_path_has_side_bias(path: &LocalPath) -> bool {
    steps_have_side_bias(path.steps()) || tail_has_side_bias(path.tail())
}

fn steps_have_side_bias(steps: &[Step]) -> bool {
    steps
        .iter()
        .any(|step| step.assertion().is_some_and(assertion_has_side_bias))
}

fn tail_has_side_bias(tail: &LocalPathTail) -> bool {
    match tail {
        LocalPathTail::Offset(offset) | LocalPathTail::Redirect(RedirectedPath::Offset(offset)) => {
            offset_has_side_bias(offset)
        }
        LocalPathTail::Redirect(RedirectedPath::Path(path)) => path_has_side_bias(path),
        LocalPathTail::None => false,
    }
}

fn offset_has_side_bias(offset: &Offset) -> bool {
    matches!(
        &offset.value,
        OffsetValue::Character {
            assertion: Some(assertion),
            ..
        } if assertion_has_side_bias(assertion)
    )
}

fn escape_value(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if is_special_char(ch) {
                [Some('^'), Some(ch)]
            } else {
                [Some(ch), None]
            }
        })
        .flatten()
        .collect()
}

/// Which side of a location should be preferred after content changes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SideBias {
    /// Prefer content before the location (`;s=b`).
    Before,
    /// Prefer content after the location (`;s=a`).
    After,
}

impl Display for SideBias {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SideBias::Before => write!(f, ";s=b"),
            SideBias::After => write!(f, ";s=a"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_number() {
        assert_eq!(number("105").unwrap(), ("", 105.0));
        assert_eq!(number("1.05").unwrap(), ("", 1.05));
        assert_eq!(number("0.05").unwrap(), ("", 0.05));
        assert!(number("01").is_err());
        assert!(number("1.0").is_err());
        assert!(number(".05").is_err());
    }

    #[test]
    fn public_parsers_round_trip_display_forms() {
        for input in [
            "epubcfi(/6/4[chap01ref]!/4[body01]/10[para05]/3:10)",
            "epubcfi(/6/4[chap01ref]!/4[body01]/10[para05]/3:10,/2/1:1,/3:4)",
        ] {
            assert_eq!(input.parse::<Cfi>().unwrap().to_string(), input);
        }
        for input in ["/8/4[test]/2:0", "/8[test;s=b]"] {
            assert_eq!(input.parse::<CfiPath>().unwrap().to_string(), input);
        }
        for input in ["/8", "/2[test;Ф=Ф,def]", "/1:0[;s=a]"] {
            assert_eq!(input.parse::<LocalPath>().unwrap().to_string(), input);
        }
        let range = "/6/4[chap01ref]!/4[body01]/10[para05]/3:10,/2/1:1,/3:4";
        assert_eq!(range.parse::<CfiRange>().unwrap().to_string(), range);
    }

    #[test]
    fn from_str_rejects_trailing_input() {
        assert!("epubcfi(/6)garbage".parse::<Cfi>().is_err());
        assert!("/6garbage".parse::<CfiPath>().is_err());
        assert!("/2:0garbage".parse::<LocalPath>().is_err());
        assert!("/6,/1:0,/1:1garbage".parse::<CfiRange>().is_err());
    }

    #[test]
    fn step_assertions_are_element_ids_only() {
        assert!("/2[section]".parse::<LocalPath>().is_ok());
        assert!(matches!(
            "/1[text]".parse::<LocalPath>(),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
        assert!(matches!(
            "/2[before,after]".parse::<LocalPath>(),
            Err(CfiError::Nom { .. })
        ));
    }

    #[test]
    fn character_offset_preserves_escaped_text_assertions() {
        let path = "/1:3[aa^[bb^]^^,cc^,dd]".parse::<LocalPath>().unwrap();
        let (_, assertion) = path.offset().unwrap().as_character().unwrap();
        assert!(matches!(
            assertion,
            Some(Assertion::Text { before: Some(before), after: Some(after), .. })
                if before == "aa[bb]^" && after == "cc,dd"
        ));
        assert_eq!(path.to_string(), "/1:3[aa^[bb^]^^,cc^,dd]");
    }

    #[test]
    fn range_endpoints_preserve_text_assertions() {
        let range = "/6,/1:1[a,b],/1:2[b,c]".parse::<CfiRange>().unwrap();
        for (endpoint, expected) in [(range.start(), ("a", "b")), (range.end(), ("b", "c"))] {
            let (_, assertion) = endpoint.offset().unwrap().as_character().unwrap();
            assert!(matches!(
                assertion,
                Some(Assertion::Text { before: Some(before), after: Some(after), .. })
                    if before == expected.0 && after == expected.1
            ));
        }
    }

    #[test]
    fn range_endpoints_reject_side_bias() {
        assert!(matches!(
            "/6,/1:0[;s=b],/1:1".parse::<CfiRange>(),
            Err(CfiError::RangeSideBiasUnsupported)
        ));
        assert!(matches!(
            "epubcfi(/6,/1:0,/1:1[;s=a])".parse::<Cfi>(),
            Err(CfiError::RangeSideBiasUnsupported)
        ));
        assert!(matches!(
            "/6,/2[;s=b],/4".parse::<CfiRange>(),
            Err(CfiError::RangeSideBiasUnsupported)
        ));
        assert!(matches!(
            "/6[;s=b]/4!/4,/0,/2".parse::<CfiRange>(),
            Err(CfiError::RangeSideBiasUnsupported)
        ));
        assert!(matches!(
            "/6/4!/4[;s=a],/0,/2".parse::<CfiRange>(),
            Err(CfiError::RangeSideBiasUnsupported)
        ));
    }

    #[test]
    fn virtual_zero_step_is_only_valid_as_a_terminal_local_step() {
        assert!("/6,/0,/2".parse::<CfiRange>().is_ok());
        assert!(matches!(
            "/0".parse::<CfiPath>(),
            Err(CfiError::InvalidStep { step: 0 })
        ));
        assert!(matches!(
            "/0/2".parse::<LocalPath>(),
            Err(CfiError::InvalidStep { step: 0 })
        ));
        assert!(matches!(
            "/6,/0:0,/2".parse::<CfiRange>(),
            Err(CfiError::InvalidStep { step: 0 })
        ));
    }

    #[test]
    fn programmatic_assertions_follow_parser_value_grammar() {
        assert!(matches!(
            Assertion::parameters(Vec::new()),
            Err(CfiError::InvalidAssertion)
        ));
        assert!(matches!(
            Assertion::text(None::<String>, None::<String>),
            Err(CfiError::InvalidAssertion)
        ));
        assert!(matches!(
            Assertion::id(String::new()),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));

        let text = Assertion::text(Some("a"), Some("b")).unwrap();
        assert!(matches!(
            Step::new(2, Some(text)),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
        assert!(matches!(
            Step::new(1, Some(Assertion::id("section").unwrap())),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
        assert!(matches!(
            Offset::character(0, Some(Assertion::id("section").unwrap())),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
    }

    #[test]
    fn programmatic_points_and_ranges_round_trip() {
        let point = Cfi::Point(
            CfiPath::new(vec![Step::new(6, None).unwrap()], LocalPathTail::None).unwrap(),
        );
        assert_eq!(point.to_string().parse::<Cfi>().unwrap(), point);

        let id = Assertion::id("chapter").unwrap();
        let parent = CfiPath::new(
            vec![Step::new(6, None).unwrap(), Step::new(4, Some(id)).unwrap()],
            LocalPathTail::Redirect(RedirectedPath::Path(Box::new(
                CfiPath::new(vec![Step::new(2, None).unwrap()], LocalPathTail::None).unwrap(),
            ))),
        )
        .unwrap();
        let start_assertion = Assertion::text(Some("before"), Some("after")).unwrap();
        let start = LocalPath::new(
            vec![Step::new(1, None).unwrap()],
            LocalPathTail::Offset(Offset::character(3, Some(start_assertion)).unwrap()),
        )
        .unwrap();
        let end = LocalPath::new(vec![Step::virtual_boundary()], LocalPathTail::None).unwrap();
        let cfi = Cfi::Range(CfiRange::new(parent, start, end).unwrap());
        let formatted = cfi.to_string();

        assert_eq!(formatted.parse::<Cfi>().unwrap(), cfi);
    }

    #[test]
    fn programmatic_construction_rejects_parser_invalid_states() {
        let invalid_step = Step::virtual_boundary();
        assert!(matches!(
            CfiPath::new(vec![invalid_step], LocalPathTail::None),
            Err(CfiError::InvalidStep { step: 0 })
        ));
        assert!(matches!(
            Offset::spatial(100.1, 0.0),
            Err(CfiError::InvalidSpatialOffset { x, y }) if x == 100.1 && y == 0.0
        ));
        assert!(matches!(
            Offset::temporal(f64::NAN),
            Err(CfiError::InvalidTemporalOffset { value }) if value.is_nan()
        ));
        assert!(matches!(
            Parameter::unknown("has space", vec!["value".into()]),
            Err(CfiError::InvalidParameter)
        ));
    }

    #[test]
    fn temporal_offsets_keep_double_precision() {
        let offset = Offset::temporal(3600.0001).unwrap();
        assert_eq!(offset.temporal_value(), Some(3600.0001));
        assert_eq!(offset.to_string(), "~3600.0001");
    }

    #[test]
    fn every_offset_constructor_round_trips() {
        let assertion = Assertion::parameters(vec![Parameter::side_bias(SideBias::After)]).unwrap();
        let offsets = [
            Offset::character(0, Some(assertion)).unwrap(),
            Offset::temporal(1.25).unwrap(),
            Offset::spatial(0.5, 100.0).unwrap(),
            Offset::temporal_spatial(2.5, 25.0, 75.0).unwrap(),
        ];

        for offset in offsets {
            let path = LocalPath::new(
                vec![Step::new(1, None).unwrap()],
                LocalPathTail::Offset(offset),
            )
            .unwrap();
            assert_eq!(path.to_string().parse::<LocalPath>().unwrap(), path);
        }
    }

    #[test]
    fn unknown_parameters_expose_name_and_values() {
        let path = "/2[test;Ф=Ф,def]".parse::<LocalPath>().unwrap();
        let assertion = path.steps()[0].assertion().unwrap();
        let parameter = &assertion.parameter_values()[0];
        assert_eq!(
            parameter.unknown_value(),
            Some(("Ф", ["Ф".to_string(), "def".to_string()].as_slice()))
        );
    }
}
