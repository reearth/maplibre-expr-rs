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
    /// A cause without a dedicated variant yet.
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
