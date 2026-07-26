//! Parse, build, format, and resolve EPUB Canonical Fragment Identifiers.
//!
//! Use [`Cfi::parse`] for a complete `epubcfi(...)` fragment, or construct one from [`CfiPath`],
//! [`Step`], [`Offset`], and [`Assertion`]. [`CfiRange`] represents a common parent with relative
//! start and end paths. Formatting these values with [`Display`] produces CFI
//! syntax. Resolve a valid [`Cfi`] against an [`Epub`](crate::Epub) to obtain [`ResolvedCfi`].
//!
//! Parsing and construction report invalid CFI syntax as [`CfiError`]. Resolving a CFI against a
//! publication reports resource or document traversal failures as [`CfiResolveError`]. Individual
//! constructors document the rules they enforce.
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

/// XML decoding error exposed by live CFI resolution.
pub use crate::xml::XmlDecodeError as CfiXmlDecodeError;
/// Live CFI resolution errors and successful point/range results.
pub use resolution::{
    CfiResolveError, ResolvedCfi, ResolvedCfiLocation, ResolvedCfiPoint, ResolvedCfiRange,
};

type Result<T> = std::result::Result<T, CfiError>;

/// A CFI syntax or construction failure.
#[derive(Debug, thiserror::Error)]
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
        x: f32,
        /// Rejected vertical percentage.
        y: f32,
    },
    /// A temporal value was non-finite or not representable in canonical CFI number syntax.
    #[error("Invalid temporal offset: {value}")]
    InvalidTemporalOffset {
        /// Rejected temporal value.
        value: f32,
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
    let range = range.map(|(start, end)| CfiRange {
        parent: path.clone(),
        start,
        end,
    });
    Ok((input, Cfi { path, range }))
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
            steps,
            tail: local.tail,
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
            tail: LocalPathTail::Offset(offset),
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

fn number(input: &str) -> IResult<&str, f32> {
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
    let (input, assertion) = opt(assertion).parse(input)?;
    Ok((
        input,
        Step {
            step: step_value,
            assertion,
        },
    ))
}

fn offset(input: &str) -> IResult<&str, Offset> {
    let (input, offset) =
        alt((temporal_spatial, temporal, spatial, character_offset)).parse(input)?;
    let (input, assertion) = opt(assertion).parse(input)?;
    let offset = match offset.kind {
        OffsetKind::Character { value, .. } => Offset {
            kind: OffsetKind::Character { value, assertion },
        },
        OffsetKind::Temporal { value } => {
            if assertion.is_some() {
                return Err(nom::Err::Error(Error {
                    input,
                    code: ErrorKind::Fail,
                }));
            }
            Offset {
                kind: OffsetKind::Temporal { value },
            }
        }
        OffsetKind::Spatial { x, y } => {
            if assertion.is_some() {
                return Err(nom::Err::Error(Error {
                    input,
                    code: ErrorKind::Fail,
                }));
            }
            Offset {
                kind: OffsetKind::Spatial { x, y },
            }
        }
        OffsetKind::TemporalSpatial { temporal, x, y } => {
            if assertion.is_some() {
                return Err(nom::Err::Error(Error {
                    input,
                    code: ErrorKind::Fail,
                }));
            }
            Offset {
                kind: OffsetKind::TemporalSpatial { temporal, x, y },
            }
        }
    };
    Ok((input, offset))
}

fn character_offset(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag(":").parse(input)?;
    let (input, value) = integer(input)?;
    Ok((
        input,
        Offset {
            kind: OffsetKind::Character {
                value,
                assertion: None,
            },
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
            kind: OffsetKind::Spatial { x, y },
        },
    ))
}

fn temporal(input: &str) -> IResult<&str, Offset> {
    let (input, _) = tag("~").parse(input)?;
    let (input, value) = number(input)?;
    Ok((
        input,
        Offset {
            kind: OffsetKind::Temporal { value },
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
            kind: OffsetKind::TemporalSpatial {
                temporal: temporal_value,
                x,
                y,
            },
        },
    ))
}

fn assertion(input: &str) -> IResult<&str, Assertion> {
    delimited(tag("["), assertion_body, tag("]")).parse(input)
}

fn assertion_body(input: &str) -> IResult<&str, Assertion> {
    let (input, leading_comma) = opt(tag(",")).parse(input)?;
    let preceding_comma = leading_comma.is_some();

    if !preceding_comma && input.starts_with(';') {
        let (input, parameters) = many1(parameter).parse(input)?;
        return Ok((
            input,
            Assertion {
                values: vec![],
                parameters,
                preceding_comma: false,
            },
        ));
    }

    let (input, first_value) = value(input)?;
    let mut values = vec![first_value];
    let mut input = input;
    if !preceding_comma
        && let Ok((next_input, _)) = tag::<&str, &str, Error<&str>>(",").parse(input)
    {
        let (next_input, second_value) = value(next_input)?;
        values.push(second_value);
        input = next_input;
    }
    let (input, parameters) = many0(parameter).parse(input)?;
    Ok((
        input,
        Assertion {
            values,
            parameters,
            preceding_comma,
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
            kind: ParameterKind::Unknown { name, csv: values },
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

/// Parsed EPUB CFI fragment (supports single paths and ranges).
///
/// Use `Cfi::parse` to parse the full `epubcfi(...)` fragment and
/// `Cfi::range()` to access the optional range.
#[derive(Debug, PartialEq, Clone)]
pub struct Cfi {
    path: CfiPath,
    range: Option<CfiRange>,
}

impl Cfi {
    /// Parses a complete `epubcfi(...)` fragment.
    pub fn parse(input: &str) -> Result<Self> {
        input.parse()
    }

    /// Constructs a point CFI from a validated full path.
    pub fn new(path: CfiPath) -> Result<Self> {
        validate_path_assertions(&path)?;
        Ok(Self { path, range: None })
    }

    /// Constructs a range CFI from its common parent and two relative endpoints.
    pub fn new_range(parent: CfiPath, start: LocalPath, end: LocalPath) -> Result<Self> {
        Self::from_range(CfiRange::new(parent, start, end)?)
    }

    /// Wraps a validated range as a CFI fragment.
    pub fn from_range(range: CfiRange) -> Result<Self> {
        range.validate()?;
        Ok(Self {
            path: range.parent.clone(),
            range: Some(range),
        })
    }

    /// The point path or range common parent path.
    pub fn path(&self) -> &CfiPath {
        &self.path
    }
    /// The range components, if this is a range CFI.
    pub fn range(&self) -> Option<&CfiRange> {
        self.range.as_ref()
    }
}

impl std::str::FromStr for Cfi {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(epubcfi).parse(input).finish() {
            Ok((_, cfi)) => {
                validate_path_assertions(&cfi.path)?;
                if let Some(range) = &cfi.range {
                    range.validate()?;
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
        write!(f, "epubcfi({}", self.path())?;
        if let Some(range) = &self.range {
            write!(f, ",{},{}", range.start(), range.end())?;
        }
        write!(f, ")")
    }
}

/// A non-empty absolute CFI path, including its optional offset or redirect tail.
#[derive(Debug, PartialEq, Clone)]
pub struct CfiPath {
    steps: Vec<Step>,
    tail: LocalPathTail,
}

impl CfiPath {
    /// Builds a non-empty path and validates step and tail assertion placement recursively.
    pub fn new(steps: Vec<Step>, tail: LocalPathTail) -> Result<Self> {
        if steps.is_empty() {
            return Err(CfiError::EmptyPath);
        }
        let path = Self { steps, tail };
        validate_path_assertions(&path)?;
        Ok(path)
    }

    /// Path steps in traversal order.
    pub fn steps(&self) -> &[Step] {
        self.steps.as_slice()
    }

    /// The terminal offset or redirect representation.
    pub fn tail(&self) -> &LocalPathTail {
        &self.tail
    }

    /// Replaces the tail and revalidates recursive assertion placement.
    pub fn with_tail(mut self, tail: LocalPathTail) -> Result<Self> {
        self.tail = tail;
        validate_path_assertions(&self)?;
        Ok(self)
    }

    /// The direct terminal offset; redirected offsets are not included.
    pub fn offset(&self) -> Option<&Offset> {
        self.tail.offset()
    }

    /// The path following `!`, if present.
    pub fn redirected(&self) -> Option<&RedirectedPath> {
        self.tail.redirected()
    }

    pub(crate) fn as_local_path(&self) -> LocalPath {
        LocalPath {
            steps: self.steps.clone(),
            tail: self.tail.clone(),
        }
    }

    /// Whether this un-offset path ends on an even element step.
    pub fn refers_to_element(&self) -> bool {
        self.offset().is_none()
            && self
                .steps
                .last()
                .map(|step| step.step % 2 == 0)
                .unwrap_or(false)
    }
}

impl Display for CfiPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.steps()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .join(""),
            self.tail
        )
    }
}

impl std::str::FromStr for CfiPath {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(path).parse(input).finish() {
            Ok((_, path)) => {
                validate_path_assertions(&path)?;
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
#[derive(Debug, PartialEq, Clone)]
pub struct LocalPath {
    steps: Vec<Step>,
    tail: LocalPathTail,
}

impl LocalPath {
    /// Builds a local path; all ordinary steps must be positive.
    pub fn new(steps: Vec<Step>, tail: LocalPathTail) -> Result<Self> {
        let path = Self { steps, tail };
        validate_local_path_assertions(&path)?;
        Ok(path)
    }

    /// Builds a range endpoint, permitting `/0` only as its terminal, unasserted, un-offset step.
    pub fn range_boundary(steps: Vec<Step>, tail: LocalPathTail) -> Result<Self> {
        let path = Self { steps, tail };
        validate_range_endpoint_assertions(&path)?;
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
        write!(
            f,
            "{}{}",
            self.steps()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .join(""),
            self.tail
        )
    }
}

impl std::str::FromStr for LocalPath {
    type Err = CfiError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match all_consuming(local_path).parse(input).finish() {
            Ok((_, local)) => {
                validate_local_path_assertions(&local)?;
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
/// Build a range when the start and end share a parent. Construction checks path shape and rejects
/// side bias anywhere in the range. Endpoint order is established during live resolution, which
/// rejects reversed ranges.
#[derive(Debug, PartialEq, Clone)]
pub struct CfiRange {
    parent: CfiPath,
    start: LocalPath,
    end: LocalPath,
}

impl CfiRange {
    /// Constructs and validates a range.
    pub fn new(parent: CfiPath, start: LocalPath, end: LocalPath) -> Result<Self> {
        let range = Self { parent, start, end };
        range.validate()?;
        Ok(range)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_path_assertions(&self.parent)?;
        validate_range_endpoint_assertions(&self.start)?;
        validate_range_endpoint_assertions(&self.end)?;
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
#[derive(Debug, PartialEq, Clone)]
pub enum LocalPathTail {
    /// No offset or one terminal offset.
    Offset(
        /// Optional terminal offset.
        Option<Offset>,
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
            LocalPathTail::Offset(offset) => offset.as_ref(),
            LocalPathTail::Redirect(_) => None,
        }
    }

    /// The redirect target, if this is a redirect tail.
    pub fn redirected(&self) -> Option<&RedirectedPath> {
        match self {
            LocalPathTail::Redirect(path) => Some(path),
            LocalPathTail::Offset(_) => None,
        }
    }
}

impl Default for LocalPathTail {
    fn default() -> Self {
        LocalPathTail::Offset(None)
    }
}

impl Display for LocalPathTail {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LocalPathTail::Offset(Some(offset)) => write!(f, "{}", offset),
            LocalPathTail::Offset(None) => Ok(()),
            LocalPathTail::Redirect(path) => write!(f, "!{}", path),
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
            RedirectedPath::Offset(offset) => write!(f, "{}", offset),
            RedirectedPath::Path(path) => write!(f, "{}", path),
        }
    }
}

/// One numeric CFI traversal step with an optional assertion.
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
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
        validate_steps(std::slice::from_ref(&step), false)?;
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
        validate_steps(std::slice::from_ref(&self), false)?;
        Ok(self)
    }
}

impl Display for Step {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "/{}{}",
            self.step,
            match &self.assertion {
                Some(a) => a.to_string(),
                None => "".to_string(),
            }
        )
    }
}

/// A character, temporal, spatial, or combined temporal-spatial CFI offset.
#[derive(Debug, PartialEq, Clone, PartialOrd)]
pub struct Offset {
    kind: OffsetKind,
}

#[derive(Debug, PartialEq, Clone, PartialOrd)]
enum OffsetKind {
    Character {
        value: usize,
        assertion: Option<Assertion>,
    },
    Temporal {
        value: f32,
    },
    Spatial {
        x: f32,
        y: f32,
    },
    TemporalSpatial {
        temporal: f32,
        x: f32,
        y: f32,
    },
}

impl Offset {
    /// Constructs a character offset with an optional text or parameter assertion.
    pub fn character(value: usize, assertion: Option<Assertion>) -> Result<Self> {
        let offset = Self {
            kind: OffsetKind::Character { value, assertion },
        };
        validate_offset_assertion(&offset)?;
        Ok(offset)
    }

    /// Constructs a non-negative canonical temporal offset.
    pub fn temporal(value: f32) -> Result<Self> {
        validate_temporal(value)?;
        Ok(Self {
            kind: OffsetKind::Temporal { value },
        })
    }

    /// Constructs spatial percentage coordinates in the inclusive range 0 through 100.
    pub fn spatial(x: f32, y: f32) -> Result<Self> {
        validate_spatial(x, y)?;
        Ok(Self {
            kind: OffsetKind::Spatial { x, y },
        })
    }

    /// Constructs a canonical temporal offset with bounded spatial percentages.
    pub fn temporal_spatial(temporal: f32, x: f32, y: f32) -> Result<Self> {
        validate_temporal(temporal)?;
        validate_spatial(x, y)?;
        Ok(Self {
            kind: OffsetKind::TemporalSpatial { temporal, x, y },
        })
    }

    /// The character value, if this is a character offset.
    pub fn character_value(&self) -> Option<usize> {
        self.as_character().map(|(value, _)| value)
    }

    /// The character value and optional assertion, if applicable.
    pub fn as_character(&self) -> Option<(usize, Option<&Assertion>)> {
        match &self.kind {
            OffsetKind::Character { value, assertion } => Some((*value, assertion.as_ref())),
            _ => None,
        }
    }

    /// The temporal component of temporal and combined offsets.
    pub fn temporal_value(&self) -> Option<f32> {
        match &self.kind {
            OffsetKind::Temporal { value }
            | OffsetKind::TemporalSpatial {
                temporal: value, ..
            } => Some(*value),
            _ => None,
        }
    }

    /// The `(x, y)` coordinates for spatial and combined offsets.
    pub fn spatial_value(&self) -> Option<(f32, f32)> {
        match &self.kind {
            OffsetKind::Spatial { x, y } | OffsetKind::TemporalSpatial { x, y, .. } => {
                Some((*x, *y))
            }
            _ => None,
        }
    }

    /// The assertion attached to a character offset.
    pub fn assertion(&self) -> Option<&Assertion> {
        match &self.kind {
            OffsetKind::Character { assertion, .. } => assertion.as_ref(),
            _ => None,
        }
    }
}

impl Display for Offset {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            OffsetKind::Character { value, assertion } => write!(
                f,
                ":{}{}",
                value,
                match assertion {
                    Some(l) => l.to_string(),
                    None => "".to_string(),
                }
            ),
            OffsetKind::Temporal { value } => write!(f, "~{}", value),
            OffsetKind::Spatial { x, y } => write!(f, "@{}:{}", x, y),
            OffsetKind::TemporalSpatial { temporal, x, y } => {
                write!(f, "~{}@{}:{}", temporal, x, y)
            }
        }
    }
}

/// Escaped assertion values and extension parameters attached to a step or character offset.
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct Assertion {
    values: Vec<String>,
    parameters: Vec<Parameter>,
    preceding_comma: bool,
}

impl Assertion {
    /// Constructs a non-empty assertion and validates value and parameter syntax.
    pub fn new(
        values: Vec<String>,
        parameters: Vec<Parameter>,
        preceding_comma: bool,
    ) -> Result<Self> {
        let assertion = Self {
            values,
            parameters,
            preceding_comma,
        };
        validate_assertion(&assertion)?;
        Ok(assertion)
    }

    /// Constructs a one-value assertion, typically an element ID assertion.
    pub fn value(value: impl Into<String>) -> Result<Self> {
        Self::new(vec![value.into()], Vec::new(), false)
    }

    /// Constructs a two-value preceding/following text assertion for a character offset.
    pub fn text(preceding: impl Into<String>, following: impl Into<String>) -> Result<Self> {
        Self::new(vec![preceding.into(), following.into()], Vec::new(), false)
    }

    /// Constructs an assertion containing parameters but no assertion value.
    pub fn parameter_only(parameters: Vec<Parameter>) -> Result<Self> {
        Self::new(Vec::new(), parameters, false)
    }

    /// Unescaped assertion values in source order.
    pub fn values(&self) -> &[String] {
        self.values.as_slice()
    }

    /// Assertion parameters in source order.
    pub fn parameters(&self) -> &[Parameter] {
        self.parameters.as_slice()
    }

    /// Whether the assertion starts with a comma (following-text-only form).
    pub fn preceding_comma(&self) -> bool {
        self.preceding_comma
    }

    /// Changes following-text-only form and revalidates assertion shape.
    pub fn with_preceding_comma(mut self, preceding_comma: bool) -> Result<Self> {
        self.preceding_comma = preceding_comma;
        validate_assertion(&self)?;
        Ok(self)
    }
}

impl Display for Assertion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "[{}{}{}]",
            if self.preceding_comma() { "," } else { "" },
            self.values
                .iter()
                .map(|value| escape_value(value))
                .collect::<Vec<_>>()
                .join(","),
            self.parameters
                .iter()
                .map(|x: &Parameter| x.to_string())
                .collect::<Vec<String>>()
                .join("")
        )
    }
}

/// A CFI assertion parameter.
///
/// Side bias is the standardized parameter; unknown name/CSV pairs preserve implementation-defined
/// extensions and are escaped during formatting.
#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct Parameter {
    kind: ParameterKind,
}

#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
enum ParameterKind {
    SideBias(SideBias),
    Unknown { name: String, csv: Vec<String> },
}

impl Parameter {
    /// Constructs the standardized side-bias parameter.
    pub fn side_bias(side_bias: SideBias) -> Self {
        Self {
            kind: ParameterKind::SideBias(side_bias),
        }
    }

    /// Constructs an extension parameter with a non-empty, space-free name and non-empty values.
    pub fn unknown(name: impl Into<String>, csv: Vec<String>) -> Result<Self> {
        let parameter = Self {
            kind: ParameterKind::Unknown {
                name: name.into(),
                csv,
            },
        };
        validate_parameter(&parameter)?;
        Ok(parameter)
    }

    /// The standardized side-bias value, if this is that parameter.
    pub fn side_bias_value(&self) -> Option<SideBias> {
        match self.kind {
            ParameterKind::SideBias(value) => Some(value),
            ParameterKind::Unknown { .. } => None,
        }
    }
}

impl Display for Parameter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.kind {
            ParameterKind::SideBias(x) => x.fmt(f),
            ParameterKind::Unknown { name, csv } => write!(
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

fn validate_path_assertions(path: &CfiPath) -> Result<()> {
    validate_steps(path.steps(), false)?;
    validate_tail_assertions(path.tail())
}

fn validate_local_path_assertions(path: &LocalPath) -> Result<()> {
    validate_steps(path.steps(), false)?;
    validate_tail_assertions(path.tail())
}

fn validate_range_endpoint_assertions(path: &LocalPath) -> Result<()> {
    validate_steps(path.steps(), true)?;
    validate_tail_assertions(path.tail())?;
    if path.steps().last().is_some_and(|step| step.step() == 0) && path.offset().is_some() {
        return Err(CfiError::InvalidStep { step: 0 });
    }
    Ok(())
}

fn validate_steps(steps: &[Step], allow_terminal_virtual_boundary: bool) -> Result<()> {
    for (index, step) in steps.iter().enumerate() {
        if step.step() == 0
            && !(allow_terminal_virtual_boundary && index == steps.len().saturating_sub(1))
        {
            return Err(CfiError::InvalidStep { step: 0 });
        }
        if step.step() == 0 && step.assertion().is_some() {
            return Err(CfiError::UnsupportedAssertionPlacement);
        }
        let Some(assertion) = step.assertion() else {
            continue;
        };
        validate_assertion(assertion)?;
        if assertion.values().is_empty() && assertion.parameters().is_empty() {
            return Err(CfiError::UnsupportedAssertionPlacement);
        }
        if !assertion.values().is_empty()
            && (step.step() % 2 != 0
                || assertion.preceding_comma()
                || assertion.values().len() != 1)
        {
            return Err(CfiError::UnsupportedAssertionPlacement);
        }
    }
    Ok(())
}

fn assertion_has_side_bias(assertion: &Assertion) -> bool {
    assertion
        .parameters()
        .iter()
        .any(|parameter| parameter.side_bias_value().is_some())
}

fn validate_tail_assertions(tail: &LocalPathTail) -> Result<()> {
    match tail {
        LocalPathTail::Offset(Some(offset))
        | LocalPathTail::Redirect(RedirectedPath::Offset(offset)) => {
            validate_offset_assertion(offset)?;
        }
        LocalPathTail::Redirect(RedirectedPath::Path(path)) => validate_path_assertions(path)?,
        LocalPathTail::Offset(None) => {}
    }
    Ok(())
}

fn validate_offset_assertion(offset: &Offset) -> Result<()> {
    let OffsetKind::Character {
        assertion: Some(assertion),
        ..
    } = &offset.kind
    else {
        return Ok(());
    };
    validate_assertion(assertion)?;
    let value_count = assertion.values().len();
    let valid = if assertion.preceding_comma() {
        value_count == 1
    } else {
        matches!(value_count, 1 | 2) || (value_count == 0 && !assertion.parameters().is_empty())
    };
    if !valid {
        return Err(CfiError::UnsupportedAssertionPlacement);
    }
    Ok(())
}

fn validate_assertion_values(assertion: &Assertion) -> Result<()> {
    if assertion.values().iter().any(String::is_empty)
        || (assertion.preceding_comma() && assertion.values().len() != 1)
    {
        return Err(CfiError::UnsupportedAssertionPlacement);
    }
    Ok(())
}

fn validate_assertion(assertion: &Assertion) -> Result<()> {
    validate_assertion_values(assertion)?;
    if assertion.values().is_empty() && assertion.parameters().is_empty() {
        return Err(CfiError::InvalidAssertion);
    }
    for parameter in assertion.parameters() {
        validate_parameter(parameter)?;
    }
    Ok(())
}

fn validate_parameter(parameter: &Parameter) -> Result<()> {
    let ParameterKind::Unknown { name, csv } = &parameter.kind else {
        return Ok(());
    };
    if name.is_empty() || name.contains(' ') || csv.is_empty() || csv.iter().any(String::is_empty) {
        return Err(CfiError::InvalidParameter);
    }
    Ok(())
}

fn validate_temporal(value: f32) -> Result<()> {
    if !is_formattable_number(value) {
        return Err(CfiError::InvalidTemporalOffset { value });
    }
    Ok(())
}

fn validate_spatial(x: f32, y: f32) -> Result<()> {
    if !is_formattable_number(x) || !is_formattable_number(y) || x > 100.0 || y > 100.0 {
        return Err(CfiError::InvalidSpatialOffset { x, y });
    }
    Ok(())
}

fn is_formattable_number(value: f32) -> bool {
    let formatted = value.to_string();
    all_consuming(number).parse(formatted.as_str()).is_ok()
}

fn path_has_side_bias(path: &CfiPath) -> bool {
    steps_have_side_bias(path.steps()) || tail_has_side_bias(path.tail())
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
        LocalPathTail::Offset(Some(offset))
        | LocalPathTail::Redirect(RedirectedPath::Offset(offset)) => offset_has_side_bias(offset),
        LocalPathTail::Redirect(RedirectedPath::Path(path)) => path_has_side_bias(path),
        LocalPathTail::Offset(None) => false,
    }
}

fn offset_has_side_bias(offset: &Offset) -> bool {
    matches!(
        &offset.kind,
        OffsetKind::Character {
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
#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
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
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
    }

    #[test]
    fn character_offset_preserves_escaped_text_assertions() {
        let path = "/1:3[aa^[bb^]^^,cc^,dd]".parse::<LocalPath>().unwrap();
        let assertion = path.offset().unwrap().assertion().unwrap();
        assert_eq!(assertion.values(), ["aa[bb]^", "cc,dd"]);
        assert_eq!(path.to_string(), "/1:3[aa^[bb^]^^,cc^,dd]");
    }

    #[test]
    fn range_endpoints_preserve_text_assertions() {
        let range = "/6,/1:1[a,b],/1:2[b,c]".parse::<CfiRange>().unwrap();
        let start = range.start().offset().unwrap().assertion().unwrap();
        let end = range.end().offset().unwrap().assertion().unwrap();
        assert_eq!(start.values(), ["a", "b"]);
        assert_eq!(end.values(), ["b", "c"]);
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
    fn virtual_zero_step_is_only_valid_at_the_end_of_a_range_endpoint() {
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
            Assertion::new(Vec::new(), Vec::new(), false),
            Err(CfiError::InvalidAssertion)
        ));
        assert!(matches!(
            Assertion::new(Vec::new(), Vec::new(), true),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
        assert!(matches!(
            Assertion::new(vec![String::new()], Vec::new(), false),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));

        let two_values = Assertion::new(vec!["a".into(), "b".into()], Vec::new(), false).unwrap();
        assert!(matches!(
            Step::new(2, Some(two_values)),
            Err(CfiError::UnsupportedAssertionPlacement)
        ));
    }

    #[test]
    fn programmatic_points_and_ranges_round_trip() {
        let point = Cfi::new(
            CfiPath::new(vec![Step::new(6, None).unwrap()], LocalPathTail::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(point.to_string().parse::<Cfi>().unwrap(), point);

        let id = Assertion::new(vec!["chapter".into()], Vec::new(), false).unwrap();
        let parent = CfiPath::new(
            vec![Step::new(6, None).unwrap(), Step::new(4, Some(id)).unwrap()],
            LocalPathTail::Redirect(RedirectedPath::Path(Box::new(
                CfiPath::new(vec![Step::new(2, None).unwrap()], LocalPathTail::default()).unwrap(),
            ))),
        )
        .unwrap();
        let start_assertion =
            Assertion::new(vec!["before".into(), "after".into()], Vec::new(), false).unwrap();
        let start = LocalPath::range_boundary(
            vec![Step::new(1, None).unwrap()],
            LocalPathTail::Offset(Some(Offset::character(3, Some(start_assertion)).unwrap())),
        )
        .unwrap();
        let end =
            LocalPath::range_boundary(vec![Step::virtual_boundary()], LocalPathTail::default())
                .unwrap();
        let cfi = Cfi::new_range(parent, start, end).unwrap();
        let formatted = cfi.to_string();

        assert_eq!(formatted.parse::<Cfi>().unwrap(), cfi);
    }

    #[test]
    fn programmatic_construction_rejects_parser_invalid_states() {
        let invalid_step = Step::virtual_boundary();
        assert!(matches!(
            CfiPath::new(vec![invalid_step], LocalPathTail::default()),
            Err(CfiError::InvalidStep { step: 0 })
        ));
        assert!(matches!(
            Offset::spatial(100.1, 0.0),
            Err(CfiError::InvalidSpatialOffset { x, y }) if x == 100.1 && y == 0.0
        ));
        assert!(matches!(
            Offset::temporal(f32::NAN),
            Err(CfiError::InvalidTemporalOffset { value }) if value.is_nan()
        ));
        assert!(matches!(
            Parameter::unknown("has space", vec!["value".into()]),
            Err(CfiError::InvalidParameter)
        ));
    }

    #[test]
    fn every_offset_constructor_round_trips() {
        let assertion = Assertion::new(
            Vec::new(),
            vec![Parameter::side_bias(SideBias::After)],
            false,
        )
        .unwrap();
        let offsets = [
            Offset::character(0, Some(assertion)).unwrap(),
            Offset::temporal(1.25).unwrap(),
            Offset::spatial(0.5, 100.0).unwrap(),
            Offset::temporal_spatial(2.5, 25.0, 75.0).unwrap(),
        ];

        for offset in offsets {
            let path = LocalPath::new(
                vec![Step::new(1, None).unwrap()],
                LocalPathTail::Offset(Some(offset)),
            )
            .unwrap();
            assert_eq!(path.to_string().parse::<LocalPath>().unwrap(), path);
        }
    }
}
