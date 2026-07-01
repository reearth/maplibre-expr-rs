//! A static type-checking / inference pass over a parsed [`Expr`].
//!
//! Mirrors the compile-time validation MapLibre performs while parsing: each
//! node's result type is inferred, operators validate their argument types, and
//! an optional `expected` type (from a property's spec) drives the same
//! assert/coerce/subtype reconciliation the reference implementation uses. When
//! a check fails the pass returns a [`ParseError`], which the caller treats as a
//! compile error (mirroring a `"result": "error"` fixture outcome).
//!
//! The pass reports errors but does not (yet) rewrite the tree with inserted
//! assertions/coercions, so type-directed *runtime* coercion is out of scope
//! here — this is purely about rejecting ill-typed expressions.

use crate::ast::{Expr, InterpKind, InterpSpace};
use crate::error::ParseError;
use crate::typ::{is_subtype, Type};
use crate::value::Value;

/// The largest integer a branch label may hold (JavaScript's `MAX_SAFE_INTEGER`).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Type-check `expr` against an optional `expected` type. Returns `Ok` if the
/// expression is well typed, or a [`ParseError`] describing the first problem.
pub fn typecheck(expr: &Expr, expected: Option<&Type>) -> Result<(), ParseError> {
    let mut checker = Checker { scope: Vec::new() };
    checker.infer(expr, expected)?;
    check_zoom_usage(expr)?;
    Ok(())
}

type R = Result<Type, ParseError>;

struct Checker {
    scope: Vec<(String, Type)>,
}

impl Checker {
    fn infer(&mut self, expr: &Expr, expected: Option<&Type>) -> R {
        let actual = match expr {
            Expr::Literal(v) => Type::of_value(v),
            Expr::Var(name) => self
                .scope
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .ok_or_else(|| ParseError::new(format!("Unknown variable \"{name}\".")))?,
            Expr::Let { bindings, body } => return self.infer_let(bindings, body, expected),
            Expr::Match {
                input,
                arms,
                default,
            } => self.infer_match(input, arms, default, expected)?,
            Expr::Step {
                input,
                output0,
                stops,
            } => self.infer_step(input, output0, stops, expected)?,
            Expr::Interpolate {
                kind,
                space,
                input,
                stops,
            } => self.infer_interpolate(*kind, *space, input, stops, expected)?,
            Expr::Call { op, args } => self.infer_call(op, args, expected)?,
        };
        reconcile(actual, expected)
    }

    /// Infer every argument against no expectation, propagating errors.
    fn infer_all(&mut self, args: &[Expr]) -> Result<(), ParseError> {
        for a in args {
            self.infer(a, None)?;
        }
        Ok(())
    }

    fn infer_let(
        &mut self,
        bindings: &[(String, Expr)],
        body: &Expr,
        expected: Option<&Type>,
    ) -> R {
        let base = self.scope.len();
        for (name, value) in bindings {
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                self.scope.truncate(base);
                return Err(ParseError::new(
                    "Variable names must contain only alphanumeric characters or '_'.",
                ));
            }
            let t = match self.infer(value, None) {
                Ok(t) => t,
                Err(e) => {
                    self.scope.truncate(base);
                    return Err(e);
                }
            };
            self.scope.push((name.clone(), t));
        }
        let result = self.infer(body, expected);
        self.scope.truncate(base);
        result
    }

    fn infer_match(
        &mut self,
        input: &Expr,
        arms: &[(Vec<Value>, Expr)],
        default: &Expr,
        expected: Option<&Type>,
    ) -> R {
        // Validate branch labels: non-empty, numbers-or-strings of one type,
        // integer/in-range if numeric, and unique across all branches.
        let mut label_type: Option<Type> = None;
        let mut seen: Vec<String> = Vec::new();
        for (labels, _) in arms {
            if labels.is_empty() {
                return Err(ParseError::new("Expected at least one branch label."));
            }
            for label in labels {
                let lt = match label {
                    Value::Number(n) => {
                        if n.abs() > MAX_SAFE_INTEGER {
                            return Err(ParseError::new(
                                "Branch labels must be integers no larger than 9007199254740991.",
                            ));
                        }
                        if n.fract() != 0.0 {
                            return Err(ParseError::new(
                                "Numeric branch labels must be integer values.",
                            ));
                        }
                        Type::Number
                    }
                    Value::String(_) => Type::String,
                    _ => return Err(ParseError::new("Branch labels must be numbers or strings.")),
                };
                match &label_type {
                    None => label_type = Some(lt),
                    Some(existing) if *existing == lt => {}
                    Some(existing) => {
                        return Err(ParseError::new(format!(
                            "Expected {existing} but found {lt} instead."
                        )))
                    }
                }
                let key = format!("{label:?}");
                if seen.contains(&key) {
                    return Err(ParseError::new("Branch labels must be unique."));
                }
                seen.push(key);
            }
        }

        // Output type: driven by the outer expectation when concrete, otherwise
        // inferred from (and unified across) the branch outputs.
        let mut output_type = concrete(expected);
        for (_, output) in arms {
            let t = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
        }
        let dt = self.infer(default, output_type.as_ref())?;
        output_type.get_or_insert(dt);

        // The input must be compatible with the label type.
        let input_type = self.infer(input, Some(&Type::Value))?;
        if let Some(lt) = &label_type {
            if !matches!(input_type, Type::Value) && !is_subtype(lt, &input_type) {
                return Err(ParseError::new(format!(
                    "Expected {lt} but found {input_type} instead."
                )));
            }
        }
        Ok(output_type.unwrap_or(Type::Value))
    }

    fn infer_step(
        &mut self,
        input: &Expr,
        output0: &Expr,
        stops: &[(f64, Expr)],
        expected: Option<&Type>,
    ) -> R {
        self.infer(input, Some(&Type::Number))?;
        let mut output_type = concrete(expected);
        let t0 = self.infer(output0, output_type.as_ref())?;
        output_type.get_or_insert(t0);
        for (_, output) in stops {
            let t = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
        }
        Ok(output_type.unwrap_or(Type::Value))
    }

    fn infer_interpolate(
        &mut self,
        _kind: InterpKind,
        space: InterpSpace,
        input: &Expr,
        stops: &[(f64, Expr)],
        expected: Option<&Type>,
    ) -> R {
        self.infer(input, Some(&Type::Number))?;
        // `interpolate-hcl` / `interpolate-lab` operate only on colors, so the
        // output type is fixed to color regardless of the property spec.
        let mut output_type = match space {
            InterpSpace::Hcl | InterpSpace::Lab => Some(Type::Color),
            InterpSpace::Rgb => concrete(expected),
        };
        for (_, output) in stops {
            let t = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
        }
        let out = output_type.unwrap_or(Type::Value);
        if !is_interpolatable(&out) {
            return Err(ParseError::new(format!(
                "Type {out} is not interpolatable."
            )));
        }
        Ok(out)
    }

    fn infer_call(&mut self, op: &str, args: &[Expr], _expected: Option<&Type>) -> R {
        match op {
            // --- comparisons ---
            "==" | "!=" | "<" | ">" | "<=" | ">=" => self.infer_comparison(op, args),

            // --- booleans ---
            "!" | "all" | "any" | "has" => {
                self.infer_all(args)?;
                Ok(Type::Boolean)
            }
            "in" => {
                self.check_search_needle(&args[0])?;
                self.infer_all(&args[1..])?;
                Ok(Type::Boolean)
            }
            "index-of" => {
                self.check_search_needle(&args[0])?;
                self.infer_all(&args[1..])?;
                Ok(Type::Number)
            }
            "within" => {
                self.infer_all(args)?;
                Ok(Type::Boolean)
            }
            "is-supported-script" => {
                self.infer_all(args)?;
                Ok(Type::Boolean)
            }

            // --- arithmetic / numeric ---
            "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "^"
            | "abs"
            | "acos"
            | "asin"
            | "atan"
            | "ceil"
            | "cos"
            | "floor"
            | "ln"
            | "log10"
            | "log2"
            | "round"
            | "sin"
            | "sqrt"
            | "tan"
            | "min"
            | "max"
            | "e"
            | "pi"
            | "ln2"
            | "zoom"
            | "heatmap-density"
            | "line-progress"
            | "sky-radial-progress"
            | "raster-value"
            | "measure-light"
            | "elevation"
            | "accumulated"
            | "distance" => {
                self.infer_all(args)?;
                Ok(Type::Number)
            }
            "length" => {
                let t = self.infer(&args[0], None)?;
                if !matches!(t, Type::Array(..) | Type::String | Type::Value) {
                    return Err(ParseError::new(format!(
                        "Expected argument of type string or array, but found {t} instead."
                    )));
                }
                Ok(Type::Number)
            }

            // --- strings ---
            "concat" | "upcase" | "downcase" | "resolved-locale" => {
                self.infer_all(args)?;
                Ok(Type::String)
            }
            "geometry-type" | "typeof" => {
                self.infer_all(args)?;
                Ok(Type::String)
            }
            "number-format" => {
                self.infer_all(args)?;
                Ok(Type::String)
            }
            "join" => {
                self.infer(&args[0], Some(&Type::array(Type::String, None)))?;
                self.infer(&args[1], None)?;
                Ok(Type::String)
            }
            "split" => {
                self.infer_all(args)?;
                Ok(Type::array(Type::String, None))
            }

            // --- collections ---
            "at" => {
                self.infer(&args[0], None)?;
                let arr = self.infer(&args[1], None)?;
                match arr {
                    Type::Array(item, _) => Ok(*item),
                    _ => Ok(Type::Value),
                }
            }
            "slice" => {
                let t = self.infer(&args[0], None)?;
                if !matches!(t, Type::Array(..) | Type::String | Type::Value) {
                    return Err(ParseError::new(format!(
                        "Expected first argument to be of type array or string, but found {t} instead."
                    )));
                }
                self.infer_all(&args[1..])?;
                Ok(t)
            }

            // --- assertions ---
            "number" => self.assert_type(args, Type::Number),
            "string" => self.assert_type(args, Type::String),
            "boolean" => self.assert_type(args, Type::Boolean),
            "object" => self.assert_type(args, Type::Object),
            "array" => self.infer_array_assertion(args),

            // --- conversions ---
            "to-number" => {
                self.infer_all(args)?;
                Ok(Type::Number)
            }
            "to-string" => {
                self.infer_all(args)?;
                Ok(Type::String)
            }
            "to-boolean" => {
                self.infer_all(args)?;
                Ok(Type::Boolean)
            }
            "to-color" => {
                self.infer_all(args)?;
                Ok(Type::Color)
            }
            "to-rgba" => {
                self.infer(&args[0], Some(&Type::Color))?;
                Ok(Type::array(Type::Number, Some(4)))
            }

            // --- colors ---
            "rgb" | "rgba" => {
                for a in args {
                    self.infer(a, Some(&Type::Number))?;
                }
                Ok(Type::Color)
            }

            // --- feature / context lookups ---
            "get" | "id" | "feature-state" | "config" => {
                self.infer_all(args)?;
                Ok(Type::Value)
            }
            "properties" => Ok(Type::Object),
            "global-state" => {
                if !matches!(&args[0], Expr::Literal(Value::String(_))) {
                    let t = self.infer(&args[0], None)?;
                    return Err(ParseError::new(format!(
                        "Global state property must be string, but found {t} instead."
                    )));
                }
                Ok(Type::Value)
            }

            // --- decisions ---
            "coalesce" => self.infer_coalesce(args, _expected),
            "case" => self.infer_case(args, _expected),

            // Operators not modeled precisely (still recurse to catch nested
            // errors); their result type is treated as the top type.
            _ => {
                self.infer_all(args)?;
                Ok(Type::Value)
            }
        }
    }

    fn infer_comparison(&mut self, op: &str, args: &[Expr]) -> R {
        let lhs = self.infer(&args[0], None)?;
        let rhs = self.infer(&args[1], None)?;
        for t in [&lhs, &rhs] {
            if !is_comparable(op, t) {
                return Err(ParseError::new(format!(
                    "\"{op}\" comparisons are not supported for type '{t}'."
                )));
            }
        }
        if lhs.kind() != rhs.kind() && !matches!(lhs, Type::Value) && !matches!(rhs, Type::Value) {
            return Err(ParseError::new(format!(
                "Cannot compare types '{lhs}' and '{rhs}'."
            )));
        }
        // An optional third argument is a collator.
        if let Some(third) = args.get(2) {
            self.infer(third, None)?;
        }
        Ok(Type::Boolean)
    }

    fn assert_type(&mut self, args: &[Expr], ty: Type) -> R {
        for a in args {
            self.infer(a, Some(&Type::Value))?;
        }
        Ok(ty)
    }

    fn infer_array_assertion(&mut self, args: &[Expr]) -> R {
        // ["array", value] | ["array", type, value] | ["array", type, N, value]
        if args.len() == 1 {
            self.infer(&args[0], None)?;
            return Ok(Type::array(Type::Value, None));
        }
        let item_type =
            match &args[0] {
                Expr::Literal(Value::String(s)) if s == "string" => Type::String,
                Expr::Literal(Value::String(s)) if s == "number" => Type::Number,
                Expr::Literal(Value::String(s)) if s == "boolean" => Type::Boolean,
                _ => return Err(ParseError::new(
                    "The item type argument of \"array\" must be one of string, number, boolean.",
                )),
            };
        let n = if args.len() >= 3 {
            match &args[1] {
                Expr::Literal(Value::Number(v)) if *v >= 0.0 && v.fract() == 0.0 => {
                    Some(*v as usize)
                }
                _ => {
                    return Err(ParseError::new(
                        "The length argument to \"array\" must be a positive integer literal.",
                    ))
                }
            }
        } else {
            None
        };
        let ty = Type::array(item_type, n);
        self.infer(&args[args.len() - 1], Some(&ty))?;
        Ok(ty)
    }

    fn infer_coalesce(&mut self, args: &[Expr], expected: Option<&Type>) -> R {
        let mut output_type = concrete(expected);
        for a in args {
            let t = self.infer(a, output_type.as_ref())?;
            output_type.get_or_insert(t);
        }
        Ok(output_type.unwrap_or(Type::Value))
    }

    fn infer_case(&mut self, args: &[Expr], expected: Option<&Type>) -> R {
        // ["case", cond, out, cond, out, ..., default]
        let mut output_type = concrete(expected);
        let mut i = 0;
        while i + 1 < args.len() {
            self.infer(&args[i], Some(&Type::Boolean))?;
            let t = self.infer(&args[i + 1], output_type.as_ref())?;
            output_type.get_or_insert(t);
            i += 2;
        }
        let dt = self.infer(&args[args.len() - 1], output_type.as_ref())?;
        output_type.get_or_insert(dt);
        Ok(output_type.unwrap_or(Type::Value))
    }

    fn check_search_needle(&mut self, needle: &Expr) -> Result<(), ParseError> {
        let t = self.infer(needle, Some(&Type::Value))?;
        if !matches!(
            t,
            Type::Boolean | Type::String | Type::Number | Type::Null | Type::Value
        ) {
            return Err(ParseError::new(format!(
                "Expected first argument to be of type boolean, string, number or null, but found {t} instead."
            )));
        }
        Ok(())
    }
}

/// Reconcile an inferred `actual` type against an `expected` one, mirroring the
/// assert/coerce/subtype logic in MapLibre's `ParsingContext`.
fn reconcile(actual: Type, expected: Option<&Type>) -> R {
    let Some(exp) = expected else {
        return Ok(actual);
    };
    let assert = matches!(
        exp,
        Type::String | Type::Number | Type::Boolean | Type::Object | Type::Array(..)
    ) && matches!(actual, Type::Value);
    if assert {
        return Ok(exp.clone());
    }
    let coerce = match exp {
        Type::ProjectionDefinition => matches!(actual, Type::String | Type::Array(..)),
        Type::Color | Type::Formatted | Type::ResolvedImage => {
            matches!(actual, Type::Value | Type::String)
        }
        Type::Padding | Type::NumberArray => {
            matches!(actual, Type::Value | Type::Number | Type::Array(..))
        }
        Type::ColorArray => matches!(actual, Type::Value | Type::String | Type::Array(..)),
        Type::VariableAnchorOffsetCollection => matches!(actual, Type::Value | Type::Array(..)),
        _ => false,
    };
    if coerce {
        return Ok(exp.clone());
    }
    if !is_subtype(exp, &actual) {
        return Err(ParseError::new(format!(
            "Expected {exp} but found {actual} instead."
        )));
    }
    Ok(actual)
}

/// The expectation to pass to output sub-expressions: a concrete expected type,
/// but not the `value` top type (which imposes no constraint).
fn concrete(expected: Option<&Type>) -> Option<Type> {
    match expected {
        Some(t) if !matches!(t, Type::Value) => Some(t.clone()),
        _ => None,
    }
}

fn is_comparable(op: &str, t: &Type) -> bool {
    match op {
        "==" | "!=" => matches!(
            t,
            Type::Boolean | Type::String | Type::Number | Type::Null | Type::Value
        ),
        _ => matches!(t, Type::String | Type::Number | Type::Value),
    }
}

fn is_interpolatable(t: &Type) -> bool {
    match t {
        Type::Number
        | Type::Color
        | Type::Padding
        | Type::NumberArray
        | Type::ColorArray
        | Type::ProjectionDefinition
        | Type::VariableAnchorOffsetCollection => true,
        // Only fixed-length numeric arrays are interpolatable (matches the
        // reference `verifyType`, which requires a numeric `N`).
        Type::Array(item, Some(_)) => matches!(**item, Type::Number),
        _ => false,
    }
}

// ---- zoom-usage validation -------------------------------------------------

/// Enforce that `zoom` is only used as the direct input of a single top-level
/// `step`/`interpolate` curve, mirroring MapLibre's `findZoomCurve`.
fn check_zoom_usage(expr: &Expr) -> Result<(), ParseError> {
    let curve = find_zoom_curve(expr)?;
    if curve.is_none() && references_zoom(expr) {
        return Err(ParseError::new(
            "\"zoom\" expression may only be used as input to a top-level \"step\" or \"interpolate\" expression.",
        ));
    }
    Ok(())
}

/// A pointer identifying the discovered zoom curve node (for identity compares).
type CurveId = *const Expr;

fn find_zoom_curve(expr: &Expr) -> Result<Option<CurveId>, ParseError> {
    let mut result: Option<CurveId> = match expr {
        Expr::Let { body, .. } => find_zoom_curve(body)?,
        Expr::Call { op, args } if op == "coalesce" => {
            let mut found = None;
            for arg in args {
                found = find_zoom_curve(arg)?;
                if found.is_some() {
                    break;
                }
            }
            found
        }
        Expr::Step { input, .. } | Expr::Interpolate { input, .. } if is_zoom(input) => {
            Some(expr as CurveId)
        }
        _ => None,
    };

    for child in children(expr) {
        let child_result = find_zoom_curve(child)?;
        match (result, child_result) {
            (None, Some(_)) => {
                return Err(ParseError::new(
                    "\"zoom\" expression may only be used as input to a top-level \"step\" or \"interpolate\" expression.",
                ));
            }
            (Some(a), Some(b)) if a != b => {
                return Err(ParseError::new(
                    "Only one zoom-based \"step\" or \"interpolate\" subexpression may be used in an expression.",
                ));
            }
            _ => {}
        }
        if result.is_none() {
            result = child_result;
        }
    }
    Ok(result)
}

fn is_zoom(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { op, args } if op == "zoom" && args.is_empty())
}

fn references_zoom(expr: &Expr) -> bool {
    if is_zoom(expr) {
        return true;
    }
    children(expr).iter().any(|c| references_zoom(c))
}

/// All direct sub-expression children of a node.
fn children(expr: &Expr) -> Vec<&Expr> {
    let mut out: Vec<&Expr> = Vec::new();
    match expr {
        Expr::Literal(_) | Expr::Var(_) => {}
        Expr::Let { bindings, body } => {
            for (_, v) in bindings {
                out.push(v);
            }
            out.push(body);
        }
        Expr::Match {
            input,
            arms,
            default,
        } => {
            out.push(input);
            for (_, o) in arms {
                out.push(o);
            }
            out.push(default);
        }
        Expr::Step {
            input,
            output0,
            stops,
        } => {
            out.push(input);
            out.push(output0);
            for (_, o) in stops {
                out.push(o);
            }
        }
        Expr::Interpolate { input, stops, .. } => {
            out.push(input);
            for (_, o) in stops {
                out.push(o);
            }
        }
        Expr::Call { args, .. } => {
            for a in args {
                out.push(a);
            }
        }
    }
    out
}
