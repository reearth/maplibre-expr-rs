//! The parsed expression tree.

use crate::typ::Type;
use crate::value::Value;

/// A parsed MapLibre expression.
///
/// Most operators are represented uniformly as [`Expr::Call`]; the handful of
/// operators with irregular argument shapes (bindings, unquoted match labels,
/// stop lists) get dedicated variants so evaluation stays simple.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A constant value (bare literal or the `literal` operator).
    Literal(Value),
    /// A generic operator call whose arguments are all sub-expressions.
    Call { op: String, args: Vec<Expr> },
    /// `["let", name, value, ..., body]`
    Let {
        bindings: Vec<(String, Expr)>,
        body: Box<Expr>,
    },
    /// `["var", name]`
    Var(String),
    /// `["match", input, label, output, ..., default]`
    Match {
        input: Box<Expr>,
        arms: Vec<(Vec<Value>, Expr)>,
        default: Box<Expr>,
    },
    /// `["step", input, output0, stop1, output1, ...]`
    Step {
        input: Box<Expr>,
        output0: Box<Expr>,
        stops: Vec<(f64, Expr)>,
    },
    /// `["interpolate"|"interpolate-hcl"|"interpolate-lab", type, input, stop, output, ...]`
    Interpolate {
        kind: InterpKind,
        space: InterpSpace,
        input: Box<Expr>,
        stops: Vec<(f64, Expr)>,
        /// Set when the output type is `projectionDefinition`, which is
        /// interpolated specially (stop outputs stay raw).
        projection: bool,
    },
    /// `["format", content, options?, ...]` — styled text sections.
    Format(Vec<FormatArg>),
    /// `["within", geojson]` — the argument polygons as `[lng, lat]` rings
    /// (a multipolygon: list of polygons, each a list of rings).
    Within(Vec<Vec<Vec<(f64, f64)>>>),
    /// `["distance", geojson]` — the argument geometries in `[lng, lat]`.
    Distance(Vec<crate::distance::SimpleGeom>),
    /// `["number-format", value, options]`.
    NumberFormat {
        value: Box<Expr>,
        locale: Option<Box<Expr>>,
        currency: Option<Box<Expr>>,
        min_fraction_digits: Option<Box<Expr>>,
        max_fraction_digits: Option<Box<Expr>>,
        unit: Option<Box<Expr>>,
    },
    /// A runtime type assertion inserted by type checking: the inner expression
    /// must already produce the given type at runtime, or evaluation errors.
    Assert(Type, Box<Expr>),
    /// A runtime coercion inserted by type checking: the inner expression's
    /// value is converted to the given type (e.g. string → color).
    Coerce(Type, Box<Expr>),
}

/// One section of a `format` expression: content plus optional styling.
#[derive(Debug, Clone)]
pub struct FormatArg {
    pub content: Expr,
    pub scale: Option<Expr>,
    pub font: Option<Expr>,
    pub text_color: Option<Expr>,
    pub vertical_align: Option<Expr>,
}

/// The interpolation curve used by an `interpolate` expression.
#[derive(Debug, Clone, Copy)]
pub enum InterpKind {
    Linear,
    Exponential(f64),
    CubicBezier(f64, f64, f64, f64),
}

/// The color space an `interpolate` expression blends in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InterpSpace {
    /// Plain component-wise interpolation (`interpolate`).
    Rgb,
    /// `interpolate-hcl`
    Hcl,
    /// `interpolate-lab`
    Lab,
}
