//! Error types for parsing and evaluation.
//!
//! Errors are modeled as semantic *kinds* (an enum), with a [`Display`] "printer"
//! that renders the message text, so callers can match on the cause rather than
//! parse a string. [`ParseError`] also carries a `key` — the location path of
//! the offending sub-expression (e.g. `[2][1]`), collected as the error bubbles
//! up through parsing — mirroring the reference implementation's error keys.
//!
//! [`Display`]: std::fmt::Display

use std::fmt;

/// The semantic cause of a parse/compile error.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    /// A cause without a dedicated variant yet (structural/shape checks).
    Other(String),
    /// An unrecognized operator name.
    UnknownExpression(String),
    /// Wrong number of arguments to an operator (`expected` is a human range).
    WrongArgCount {
        op: String,
        expected: String,
        found: usize,
    },
    /// An expression's type did not satisfy the expected type.
    TypeMismatch { expected: String, found: String },
    /// A comparison operator applied to an unsupported operand type.
    NotComparable { op: String, ty: String },
    /// A comparison between two incompatible concrete types.
    CannotCompare { lhs: String, rhs: String },
    /// An `interpolate` output whose type cannot be interpolated.
    NotInterpolatable(String),
    /// An unbound `var` reference.
    UnboundVariable(String),
    /// Misuse of the `zoom` expression.
    Zoom(&'static str),
    /// An operator that takes exactly one argument (`literal`/`within`/…).
    RequiresExactlyOneArg { op: String, found: usize },
    /// A single-argument coercion (`to-boolean`/`to-string`) with wrong arity.
    ExpectedOneArgument,
    /// A `CompoundExpression` whose arity matched no typed overload.
    ExpectedArgsOfType { sig: String, found: String },
    /// `match` with fewer than four arguments.
    MatchAtLeast4 { found: usize },
    /// The first (item-type) argument of `array` was not a valid type name.
    ArrayItemType,
    /// The length argument of `array` was not a positive integer literal.
    ArrayLength,
    /// A bare object used where an expression was expected.
    BareObject,
    /// An empty array (no operator).
    EmptyArray,
    /// The operator slot was not a string. `found` is the JS `typeof`.
    ExpressionNameNotString { found: &'static str },
    /// The `global-state` property argument was not a string. `found` is a type.
    GlobalStateProperty { found: String },
    /// `step`/`interpolate` stop inputs were not strictly ascending.
    AscendingStops { kind: String },
    /// `exponential` interpolation without a numeric base.
    ExponentialBase,
    /// A malformed `cubic-bezier` interpolation type.
    CubicBezier,
    /// The `collator` options argument was not an object.
    CollatorOptions,
    /// `number-format` given both `currency` and `unit`.
    NumberFormatExclusive,
    /// `slice`'s first argument was not an array or string.
    SliceFirstArg { found: String },
    /// An `in`/`index-of` needle (checked statically) was not a primitive.
    SearchNeedle { found: String },
    /// A `match` branch label was not a number or string.
    BranchLabelsType,
    /// Duplicate `match` branch labels.
    BranchLabelsUnique,
    /// A `match` branch with no labels.
    BranchLabelsEmpty,
    /// A non-integer numeric `match` branch label.
    BranchLabelNotInteger,
    /// A numeric `match` branch label beyond the safe-integer range.
    BranchLabelTooLarge,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseErrorKind::Other(s) => write!(f, "{s}"),
            ParseErrorKind::UnknownExpression(op) => write!(
                f,
                "Unknown expression \"{op}\". If you wanted a literal array, use [\"literal\", [...]]."
            ),
            ParseErrorKind::WrongArgCount {
                op,
                expected,
                found,
            } => {
                let _ = op;
                write!(f, "Expected {expected}, but found {found} instead.")
            }
            ParseErrorKind::TypeMismatch { expected, found } => {
                write!(f, "Expected {expected} but found {found} instead.")
            }
            ParseErrorKind::NotComparable { op, ty } => {
                write!(f, "\"{op}\" comparisons are not supported for type '{ty}'.")
            }
            ParseErrorKind::CannotCompare { lhs, rhs } => {
                write!(f, "Cannot compare types '{lhs}' and '{rhs}'.")
            }
            ParseErrorKind::NotInterpolatable(ty) => write!(f, "Type {ty} is not interpolatable."),
            ParseErrorKind::UnboundVariable(name) => write!(
                f,
                "Unknown variable \"{name}\". Make sure \"{name}\" has been bound in an enclosing \"let\" expression before using it."
            ),
            ParseErrorKind::Zoom(msg) => write!(f, "{msg}"),
            ParseErrorKind::RequiresExactlyOneArg { op, found } => write!(
                f,
                "'{op}' expression requires exactly one argument, but found {found} instead."
            ),
            ParseErrorKind::ExpectedOneArgument => write!(f, "Expected one argument."),
            ParseErrorKind::ExpectedArgsOfType { sig, found } => write!(
                f,
                "Expected arguments of type {sig}, but found ({found}) instead."
            ),
            ParseErrorKind::MatchAtLeast4 { found } => {
                write!(f, "Expected at least 4 arguments, but found only {found}.")
            }
            ParseErrorKind::ArrayItemType => write!(
                f,
                "The item type argument of \"array\" must be one of string, number, boolean"
            ),
            ParseErrorKind::ArrayLength => write!(
                f,
                "The length argument to \"array\" must be a positive integer literal"
            ),
            ParseErrorKind::BareObject => {
                write!(f, "Bare objects invalid. Use [\"literal\", {{...}}] instead.")
            }
            ParseErrorKind::EmptyArray => write!(
                f,
                "Expected an array with at least one element. If you wanted a literal array, use [\"literal\", []]."
            ),
            ParseErrorKind::ExpressionNameNotString { found } => write!(
                f,
                "Expression name must be a string, but found {found} instead. If you wanted a literal array, use [\"literal\", [...]]."
            ),
            ParseErrorKind::GlobalStateProperty { found } => {
                write!(f, "Global state property must be string, but found {found} instead.")
            }
            ParseErrorKind::AscendingStops { kind } => write!(
                f,
                "Input/output pairs for \"{kind}\" expressions must be arranged with input values in strictly ascending order."
            ),
            ParseErrorKind::ExponentialBase => {
                write!(f, "Exponential interpolation requires a numeric base.")
            }
            ParseErrorKind::CubicBezier => write!(
                f,
                "Cubic bezier interpolation requires four numeric arguments with values between 0 and 1."
            ),
            ParseErrorKind::CollatorOptions => {
                write!(f, "Collator options argument must be an object.")
            }
            ParseErrorKind::NumberFormatExclusive => write!(
                f,
                "NumberFormat options `currency` and `unit` are mutually exclusive"
            ),
            ParseErrorKind::SliceFirstArg { found } => write!(
                f,
                "Expected first argument to be of type array or string, but found {found} instead"
            ),
            ParseErrorKind::SearchNeedle { found } => write!(
                f,
                "Expected first argument to be of type boolean, string, number or null, but found {found} instead"
            ),
            ParseErrorKind::BranchLabelsType => {
                write!(f, "Branch labels must be numbers or strings.")
            }
            ParseErrorKind::BranchLabelsUnique => write!(f, "Branch labels must be unique."),
            ParseErrorKind::BranchLabelsEmpty => write!(f, "Expected at least one branch label."),
            ParseErrorKind::BranchLabelNotInteger => {
                write!(f, "Numeric branch labels must be integer values.")
            }
            ParseErrorKind::BranchLabelTooLarge => write!(
                f,
                "Branch labels must be integers no larger than 9007199254740991."
            ),
        }
    }
}

/// An error raised while turning JSON into an [`Expr`](crate::Expr).
///
/// Corresponds to a `"result": "error"` compile outcome in the spec fixtures.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    /// Location path of the offending sub-expression, e.g. `"[2][1]"`.
    pub key: String,
}

impl ParseError {
    /// Build an error from a semantic kind.
    pub fn of(kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            key: String::new(),
        }
    }

    /// Build an ad-hoc error from a message (kind [`ParseErrorKind::Other`]).
    pub fn new(message: impl Into<String>) -> ParseError {
        ParseError::of(ParseErrorKind::Other(message.into()))
    }

    /// Prepend an argument index to the location key as the error bubbles up.
    pub(crate) fn at(mut self, index: usize) -> ParseError {
        self.key = format!("[{index}]{}", self.key);
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for ParseError {}

/// The semantic cause of an evaluation error.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalErrorKind {
    /// A cause without a dedicated variant (a few structural/runtime one-offs).
    Other(String),
    /// A value was not of the expected type.
    TypeMismatch { expected: String, found: String },
    /// Like [`TypeMismatch`](Self::TypeMismatch), but naming the offending
    /// argument (e.g. `"second argument"`) instead of the generic "value".
    TypeMismatchArg {
        arg: &'static str,
        expected: String,
        found: String,
    },
    /// A value could not be parsed into a type (`to-color`, coercions, …).
    /// `value` is already rendered (raw string, else `JSON.stringify`).
    CouldNotParse { ty: &'static str, value: String },
    /// A value could not be converted to a number (`to-number`).
    CouldNotConvertToNumber { value: String },
    /// An `at` index was negative.
    ArrayIndexNegative { index: f64 },
    /// An `at` index was past the end of the array.
    ArrayIndexOutOfBounds { index: f64, max: usize },
    /// An `at` index was not an integer.
    ArrayIndexNotInteger { index: f64 },
    /// An `rgb`/`rgba`/`to-color` array value was out of range or malformed.
    /// `reason` is the clause after the value.
    InvalidRgba { value: String, reason: &'static str },
    /// Ordered comparison of two runtime values of incompatible types.
    NotOrderedComparable {
        op: String,
        lhs: String,
        rhs: String,
    },
    /// An `in`/`index-of` needle was not a primitive.
    SearchNeedle { found: String },
}

impl fmt::Display for EvalErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalErrorKind::Other(s) => write!(f, "{s}"),
            EvalErrorKind::TypeMismatch { expected, found } => write!(
                f,
                "Expected value to be of type {expected}, but found {found} instead."
            ),
            EvalErrorKind::TypeMismatchArg {
                arg,
                expected,
                found,
            } => write!(
                f,
                "Expected {arg} to be of type {expected}, but found {found} instead."
            ),
            EvalErrorKind::CouldNotParse { ty, value } => {
                write!(f, "Could not parse {ty} from value '{value}'")
            }
            EvalErrorKind::CouldNotConvertToNumber { value } => {
                write!(f, "Could not convert {value} to number.")
            }
            EvalErrorKind::ArrayIndexNegative { index } => {
                write!(f, "Array index out of bounds: {index} < 0.")
            }
            EvalErrorKind::ArrayIndexOutOfBounds { index, max } => {
                write!(f, "Array index out of bounds: {index} > {max}.")
            }
            EvalErrorKind::ArrayIndexNotInteger { index } => {
                write!(f, "Array index must be an integer, but found {index} instead.")
            }
            EvalErrorKind::InvalidRgba { value, reason } => {
                write!(f, "Invalid rgba value {value}: {reason}")
            }
            EvalErrorKind::NotOrderedComparable { op, lhs, rhs } => write!(
                f,
                "Expected arguments for \"{op}\" to be (string, string) or (number, number), but found ({lhs}, {rhs}) instead."
            ),
            EvalErrorKind::SearchNeedle { found } => write!(
                f,
                "Expected first argument to be of type boolean, string, number or null, but found {found} instead."
            ),
        }
    }
}

/// An error raised while evaluating a well-formed expression.
///
/// Corresponds to a per-input `{ "error": ... }` output in the spec fixtures.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalError {
    pub kind: EvalErrorKind,
}

impl EvalError {
    pub fn of(kind: EvalErrorKind) -> EvalError {
        EvalError { kind }
    }

    pub fn new(message: impl Into<String>) -> EvalError {
        EvalError::of(EvalErrorKind::Other(message.into()))
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for EvalError {}
