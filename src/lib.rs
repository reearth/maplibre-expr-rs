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
mod error;
mod eval;
mod geometry;
mod parse;
mod typ;
mod typecheck;
mod value;

pub use ast::{Expr, InterpKind, InterpSpace};
pub use color::Color;
pub use context::{EvaluationContext, Feature};
pub use error::{EvalError, ParseError};
pub use typ::Type;
pub use value::Value;

/// Parse a MapLibre expression from its JSON representation.
pub fn parse(json: &serde_json::Value) -> Result<Expr, ParseError> {
    parse::parse(json)
}

/// Statically type-check a parsed expression, optionally against the type a
/// property expects. On success returns a rewritten tree with type-directed
/// coercion/assertion nodes inserted (evaluate this returned expression so the
/// coercions take effect). Returns a [`ParseError`] for expressions the
/// reference implementation rejects at compile time (bad comparisons, malformed
/// `match` branches, non-interpolatable outputs, misused `zoom`, and so on).
pub fn typecheck(expr: &Expr, expected: Option<&Type>) -> Result<Expr, ParseError> {
    typecheck::typecheck(expr, expected)
}

/// Evaluate a parsed expression against an evaluation context.
pub fn evaluate(expr: &Expr, ctx: &EvaluationContext) -> Result<Value, EvalError> {
    eval::eval(expr, ctx)
}
