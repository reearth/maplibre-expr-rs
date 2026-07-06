//! `maplibre_expr` — a pure-Rust parser and evaluator for
//! [MapLibre GL style expressions][spec].
//!
//! The crate turns a MapLibre expression (a `serde_json::Value` such as
//! `["*", ["get", "x"], 2]`) into an [`Expr`] tree with [`parse`], then
//! evaluates that tree against an [`EvaluationContext`] with [`evaluate`].
//!
//! ```
//! use maplibre_expr::{parse, evaluate, EvaluationContext, Feature, Value};
//! use std::collections::BTreeMap;
//!
//! let json: serde_json::Value = serde_json::json!(["*", ["get", "x"], 2]);
//! let expr = parse(&json).unwrap();
//!
//! let mut props = BTreeMap::new();
//! props.insert("x".to_string(), Value::Number(21.0));
//! let ctx = EvaluationContext::new().with_feature(Feature {
//!     properties: props,
//!     ..Feature::default()
//! });
//!
//! assert_eq!(evaluate(&expr, &ctx).unwrap(), Value::Number(42.0));
//! ```
//!
//! Conformance is validated against a vendored snapshot of the
//! `maplibre-style-spec` expression test fixtures; see `tests/spec.rs`.
//!
//! [spec]: https://maplibre.org/maplibre-style-spec/expressions/

mod ast;
mod color;
mod context;
pub mod convert;
mod distance;
mod error;
mod eval;
mod ext;
pub mod filter;
mod geometry;
mod parse;
mod typ;
mod typecheck;
mod value;

pub use ast::{Expr, InterpKind, InterpSpace};
pub use color::Color;
pub use context::{EvaluationContext, Feature};
pub use error::{EvalError, EvalErrorKind, ParseError, ParseErrorKind};
pub use ext::{Function, Macro, Options};
pub use filter::{convert_legacy_filter, is_expression_filter, FilterError};
pub use typ::Type;
pub use value::{Projection, Value};

/// Parse a MapLibre expression from its JSON representation.
pub fn parse(json: &serde_json::Value) -> Result<Expr, ParseError> {
    parse::parse(json, &Options::default())
}

/// Whether `json` is a MapLibre *expression* — an array whose first element
/// names a built-in operator — as opposed to a literal value such as a bare
/// `["Font A", "Font B"]` array or a legacy function object.
///
/// A direct analogue of MapLibre's `isExpression`: a purely syntactic head
/// check that does **not** validate arity or arguments (`["get"]` is still an
/// expression). `["literal", …]` counts as an expression. Objects, scalars,
/// the empty array, and an array whose head is not a built-in operator (e.g. a
/// font-name string) are not expressions. User macros / functions / natives
/// (which are `Options`-scoped, not part of the grammar) are not considered.
///
/// This is the check callers use to tell a data-driven property expression
/// apart from a plain literal that merely happens to be an array.
pub fn is_expression(json: &serde_json::Value) -> bool {
    json.as_array()
        .and_then(|arr| arr.first())
        .and_then(|head| head.as_str())
        .is_some_and(parse::is_operator)
}

/// Parse an expression with user [`Options`] (macros expand at parse time;
/// function names are accepted as callable operators).
pub fn parse_with(json: &serde_json::Value, options: &Options) -> Result<Expr, ParseError> {
    parse::parse(json, options)
}

/// Statically type-check a parsed expression, optionally against the type a
/// property expects. On success returns a rewritten tree with type-directed
/// coercion/assertion nodes inserted (evaluate this returned expression so the
/// coercions take effect). Returns a [`ParseError`] for expressions the
/// reference implementation rejects at compile time (bad comparisons, malformed
/// `match` branches, non-interpolatable outputs, misused `zoom`, and so on).
///
/// `coerce_top_string` reflects whether the target property is string-typed
/// (not merely enum-typed): such properties coerce the top-level result to a
/// string rather than asserting it.
pub fn typecheck(
    expr: &Expr,
    expected: Option<&Type>,
    coerce_top_string: bool,
) -> Result<Expr, ParseError> {
    typecheck::typecheck(expr, expected, coerce_top_string)
}

/// Evaluate a parsed expression against an evaluation context.
pub fn evaluate(expr: &Expr, ctx: &EvaluationContext) -> Result<Value, EvalError> {
    eval::eval(expr, ctx)
}

/// Evaluate with user [`Options`], so calls to user functions are dispatched to
/// their (recursion-limited) bodies.
pub fn evaluate_with(
    expr: &Expr,
    ctx: &EvaluationContext,
    options: &Options,
) -> Result<Value, EvalError> {
    eval::eval_with(expr, ctx, options)
}
