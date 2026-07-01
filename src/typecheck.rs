//! A static type-checking / inference pass over a parsed [`Expr`].
//!
//! Mirrors the compile-time validation MapLibre performs while parsing: each
//! node's result type is inferred, operators validate their argument types, and
//! an optional `expected` type (from a property's spec) drives the same
//! assert/coerce/subtype reconciliation the reference implementation uses. When
//! a check fails the pass returns a [`ParseError`], which the caller treats as a
//! compile error (mirroring a `"result": "error"` fixture outcome).
//!
//! On success it returns a rewritten tree with [`Expr::Assert`] / [`Expr::Coerce`]
//! nodes inserted at the points where MapLibre annotates, so evaluation performs
//! the same type-directed coercions (e.g. a string output where a color is
//! expected).

use crate::ast::{Expr, InterpKind, InterpSpace};
use crate::error::ParseError;
use crate::typ::{is_subtype, Type};
use crate::value::Value;

/// The largest integer a branch label may hold (JavaScript's `MAX_SAFE_INTEGER`).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Type-check `expr` against an optional `expected` type. Returns the annotated
/// expression (with coercion/assertion nodes inserted) on success, or a
/// [`ParseError`] describing the first problem.
pub fn typecheck(expr: &Expr, expected: Option<&Type>) -> Result<Expr, ParseError> {
    let mut checker = Checker { scope: Vec::new() };
    let (node, ty) = checker.infer_node(expr, expected)?;
    // Top level: string-valued properties coerce rather than assert.
    let coerce_string = matches!(expected, Some(Type::String));
    let (annotated, _) = reconcile(node, ty, expected, coerce_string)?;
    check_zoom_usage(&annotated)?;
    Ok(annotated)
}

/// A checked node paired with its inferred type.
type Typed = (Expr, Type);
type R = Result<Typed, ParseError>;

struct Checker {
    scope: Vec<(String, Type)>,
}

impl Checker {
    /// Infer and annotate `expr`, reconciling against `expected` (assert/coerce
    /// as needed). Sub-expression entry point.
    fn infer(&mut self, expr: &Expr, expected: Option<&Type>) -> R {
        let (node, ty) = self.infer_node(expr, expected)?;
        reconcile(node, ty, expected, false)
    }

    /// Build the node itself (with annotated children) and its intrinsic type,
    /// without wrapping the node against `expected` — the caller reconciles.
    fn infer_node(&mut self, expr: &Expr, expected: Option<&Type>) -> R {
        match expr {
            Expr::Literal(v) => Ok((Expr::Literal(v.clone()), Type::of_value(v))),
            Expr::Var(name) => {
                let ty = self
                    .scope
                    .iter()
                    .rev()
                    .find(|(n, _)| n == name)
                    .map(|(_, t)| t.clone())
                    .ok_or_else(|| ParseError::new(format!("Unknown variable \"{name}\".")))?;
                Ok((Expr::Var(name.clone()), ty))
            }
            Expr::Let { bindings, body } => self.infer_let(bindings, body, expected),
            Expr::Match {
                input,
                arms,
                default,
            } => self.infer_match(input, arms, default, expected),
            Expr::Step {
                input,
                output0,
                stops,
            } => self.infer_step(input, output0, stops, expected),
            Expr::Interpolate {
                kind,
                space,
                input,
                stops,
            } => self.infer_interpolate(*kind, *space, input, stops, expected),
            Expr::Call { op, args } => self.infer_call(op, args, expected),
            // Annotations only appear in already-checked trees.
            Expr::Assert(t, inner) | Expr::Coerce(t, inner) => {
                let (e, _) = self.infer(inner, None)?;
                Ok((
                    wrap(t.clone(), e, matches!(expr, Expr::Coerce(..))),
                    t.clone(),
                ))
            }
        }
    }

    /// Infer each argument with no expectation, returning annotated copies.
    fn infer_args(&mut self, args: &[Expr]) -> Result<Vec<Expr>, ParseError> {
        args.iter().map(|a| Ok(self.infer(a, None)?.0)).collect()
    }

    fn infer_let(
        &mut self,
        bindings: &[(String, Expr)],
        body: &Expr,
        expected: Option<&Type>,
    ) -> R {
        let base = self.scope.len();
        let mut new_bindings = Vec::with_capacity(bindings.len());
        for (name, value) in bindings {
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                self.scope.truncate(base);
                return Err(ParseError::new(
                    "Variable names must contain only alphanumeric characters or '_'.",
                ));
            }
            let (node, t) = match self.infer(value, None) {
                Ok(v) => v,
                Err(e) => {
                    self.scope.truncate(base);
                    return Err(e);
                }
            };
            new_bindings.push((name.clone(), node));
            self.scope.push((name.clone(), t));
        }
        let result = self.infer(body, expected);
        self.scope.truncate(base);
        let (body_node, body_type) = result?;
        Ok((
            Expr::Let {
                bindings: new_bindings,
                body: Box::new(body_node),
            },
            body_type,
        ))
    }

    fn infer_match(
        &mut self,
        input: &Expr,
        arms: &[(Vec<Value>, Expr)],
        default: &Expr,
        expected: Option<&Type>,
    ) -> R {
        let label_type = validate_match_labels(arms)?;

        let mut output_type = concrete(expected);
        let mut new_arms = Vec::with_capacity(arms.len());
        for (labels, output) in arms {
            let (node, t) = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
            new_arms.push((labels.clone(), node));
        }
        let (default_node, dt) = self.infer(default, output_type.as_ref())?;
        output_type.get_or_insert(dt);

        let (input_node, input_type) = self.infer(input, Some(&Type::Value))?;
        if let Some(lt) = &label_type {
            if !matches!(input_type, Type::Value) && !is_subtype(lt, &input_type) {
                return Err(ParseError::new(format!(
                    "Expected {lt} but found {input_type} instead."
                )));
            }
        }
        Ok((
            Expr::Match {
                input: Box::new(input_node),
                arms: new_arms,
                default: Box::new(default_node),
            },
            output_type.unwrap_or(Type::Value),
        ))
    }

    fn infer_step(
        &mut self,
        input: &Expr,
        output0: &Expr,
        stops: &[(f64, Expr)],
        expected: Option<&Type>,
    ) -> R {
        let (input_node, _) = self.infer(input, Some(&Type::Number))?;
        let mut output_type = concrete(expected);
        let (out0_node, t0) = self.infer(output0, output_type.as_ref())?;
        output_type.get_or_insert(t0);
        let mut new_stops = Vec::with_capacity(stops.len());
        for (stop, output) in stops {
            let (node, t) = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
            new_stops.push((*stop, node));
        }
        Ok((
            Expr::Step {
                input: Box::new(input_node),
                output0: Box::new(out0_node),
                stops: new_stops,
            },
            output_type.unwrap_or(Type::Value),
        ))
    }

    fn infer_interpolate(
        &mut self,
        kind: InterpKind,
        space: InterpSpace,
        input: &Expr,
        stops: &[(f64, Expr)],
        expected: Option<&Type>,
    ) -> R {
        let (input_node, _) = self.infer(input, Some(&Type::Number))?;
        // hcl/lab interpolation is color-only regardless of the property spec.
        let mut output_type = match space {
            InterpSpace::Hcl | InterpSpace::Lab => Some(Type::Color),
            InterpSpace::Rgb => concrete(expected),
        };
        let mut new_stops = Vec::with_capacity(stops.len());
        for (stop, output) in stops {
            let (node, t) = self.infer(output, output_type.as_ref())?;
            output_type.get_or_insert(t);
            new_stops.push((*stop, node));
        }
        let out = output_type.unwrap_or(Type::Value);
        if !is_interpolatable(&out) {
            return Err(ParseError::new(format!(
                "Type {out} is not interpolatable."
            )));
        }
        Ok((
            Expr::Interpolate {
                kind,
                space,
                input: Box::new(input_node),
                stops: new_stops,
            },
            out,
        ))
    }

    fn infer_call(&mut self, op: &str, args: &[Expr], expected: Option<&Type>) -> R {
        let mk = |args: Vec<Expr>, ty: Type| {
            (
                Expr::Call {
                    op: op.to_string(),
                    args,
                },
                ty,
            )
        };
        Ok(match op {
            "==" | "!=" | "<" | ">" | "<=" | ">=" => return self.infer_comparison(op, args),

            "!" | "all" | "any" | "has" | "within" | "is-supported-script" => {
                mk(self.infer_args(args)?, Type::Boolean)
            }
            "in" => {
                let needle = self.check_search_needle(&args[0])?;
                let mut new_args = vec![needle];
                new_args.extend(self.infer_args(&args[1..])?);
                mk(new_args, Type::Boolean)
            }
            "index-of" => {
                let needle = self.check_search_needle(&args[0])?;
                let mut new_args = vec![needle];
                new_args.extend(self.infer_args(&args[1..])?);
                mk(new_args, Type::Number)
            }

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
            | "distance" => mk(self.infer_args(args)?, Type::Number),

            "length" => {
                let (node, t) = self.infer(&args[0], None)?;
                if !matches!(t, Type::Array(..) | Type::String | Type::Value) {
                    return Err(ParseError::new(format!(
                        "Expected argument of type string or array, but found {t} instead."
                    )));
                }
                mk(vec![node], Type::Number)
            }

            "concat" | "upcase" | "downcase" | "resolved-locale" | "geometry-type" | "typeof"
            | "number-format" => mk(self.infer_args(args)?, Type::String),

            "join" => {
                let (a0, _) = self.infer(&args[0], Some(&Type::array(Type::String, None)))?;
                let (a1, _) = self.infer(&args[1], None)?;
                mk(vec![a0, a1], Type::String)
            }
            "split" => mk(self.infer_args(args)?, Type::array(Type::String, None)),

            "at" => {
                let (idx, _) = self.infer(&args[0], None)?;
                let (arr, arr_ty) = self.infer(&args[1], None)?;
                let item = match arr_ty {
                    Type::Array(item, _) => *item,
                    _ => Type::Value,
                };
                mk(vec![idx, arr], item)
            }
            "slice" => {
                let (input, t) = self.infer(&args[0], None)?;
                if !matches!(t, Type::Array(..) | Type::String | Type::Value) {
                    return Err(ParseError::new(format!(
                        "Expected first argument to be of type array or string, but found {t} instead."
                    )));
                }
                let mut new_args = vec![input];
                new_args.extend(self.infer_args(&args[1..])?);
                mk(new_args, t)
            }

            "number" => return self.assert_op(op, args, Type::Number),
            "string" => return self.assert_op(op, args, Type::String),
            "boolean" => return self.assert_op(op, args, Type::Boolean),
            "object" => return self.assert_op(op, args, Type::Object),
            "array" => return self.infer_array_assertion(args),

            "to-number" => mk(self.infer_args(args)?, Type::Number),
            "to-string" => mk(self.infer_args(args)?, Type::String),
            "to-boolean" => mk(self.infer_args(args)?, Type::Boolean),
            "to-color" => mk(self.infer_args(args)?, Type::Color),
            "to-rgba" => {
                let (a0, _) = self.infer(&args[0], Some(&Type::Color))?;
                mk(vec![a0], Type::array(Type::Number, Some(4)))
            }
            "rgb" | "rgba" => {
                let mut new_args = Vec::with_capacity(args.len());
                for a in args {
                    new_args.push(self.infer(a, Some(&Type::Number))?.0);
                }
                mk(new_args, Type::Color)
            }

            "get" | "id" | "feature-state" | "config" => mk(self.infer_args(args)?, Type::Value),
            "properties" => mk(self.infer_args(args)?, Type::Object),
            "global-state" => {
                if !matches!(&args[0], Expr::Literal(Value::String(_))) {
                    let (_, t) = self.infer(&args[0], None)?;
                    return Err(ParseError::new(format!(
                        "Global state property must be string, but found {t} instead."
                    )));
                }
                mk(self.infer_args(args)?, Type::Value)
            }

            "coalesce" => return self.infer_coalesce(op, args, expected),
            "case" => return self.infer_case(op, args, expected),

            "image" => {
                let (a0, _) = self.infer(&args[0], Some(&Type::String))?;
                mk(vec![a0], Type::ResolvedImage)
            }

            _ => mk(self.infer_args(args)?, Type::Value),
        })
    }

    fn infer_comparison(&mut self, op: &str, args: &[Expr]) -> R {
        let (lhs, lt) = self.infer(&args[0], None)?;
        let (rhs, rt) = self.infer(&args[1], None)?;
        for t in [&lt, &rt] {
            if !is_comparable(op, t) {
                return Err(ParseError::new(format!(
                    "\"{op}\" comparisons are not supported for type '{t}'."
                )));
            }
        }
        if lt.kind() != rt.kind() && !matches!(lt, Type::Value) && !matches!(rt, Type::Value) {
            return Err(ParseError::new(format!(
                "Cannot compare types '{lt}' and '{rt}'."
            )));
        }
        let mut new_args = vec![lhs, rhs];
        if let Some(third) = args.get(2) {
            new_args.push(self.infer(third, None)?.0);
        }
        Ok((
            Expr::Call {
                op: op.to_string(),
                args: new_args,
            },
            Type::Boolean,
        ))
    }

    fn assert_op(&mut self, op: &str, args: &[Expr], ty: Type) -> R {
        // The explicit assertion operators already type-check at runtime; their
        // arguments carry no useful expectation.
        let mut new_args = Vec::with_capacity(args.len());
        for a in args {
            new_args.push(self.infer(a, Some(&Type::Value))?.0);
        }
        Ok((
            Expr::Call {
                op: op.to_string(),
                args: new_args,
            },
            ty,
        ))
    }

    fn infer_array_assertion(&mut self, args: &[Expr]) -> R {
        // Item type present with >= 2 args; length (nullable) with >= 3; the
        // remaining args are fallback value candidates.
        let (item_type, mut value_start) = if args.len() >= 2 {
            let t = match &args[0] {
                Expr::Literal(Value::String(s)) if s == "string" => Type::String,
                Expr::Literal(Value::String(s)) if s == "number" => Type::Number,
                Expr::Literal(Value::String(s)) if s == "boolean" => Type::Boolean,
                _ => return Err(ParseError::new(
                    "The item type argument of \"array\" must be one of string, number, boolean.",
                )),
            };
            (t, 1)
        } else {
            (Type::Value, 0)
        };
        let n = if args.len() >= 3 {
            value_start = 2;
            match &args[1] {
                Expr::Literal(Value::Null) => None,
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
        let mut new_args = args[..value_start].to_vec();
        for a in &args[value_start..] {
            new_args.push(self.infer(a, Some(&Type::Value))?.0);
        }
        Ok((call("array", new_args), ty))
    }

    fn infer_coalesce(&mut self, op: &str, args: &[Expr], expected: Option<&Type>) -> R {
        let mut output_type = concrete(expected);
        let mut new_args = Vec::with_capacity(args.len());
        for a in args {
            let (node, t) = self.infer(a, output_type.as_ref())?;
            output_type.get_or_insert(t);
            new_args.push(node);
        }
        Ok((
            Expr::Call {
                op: op.to_string(),
                args: new_args,
            },
            output_type.unwrap_or(Type::Value),
        ))
    }

    fn infer_case(&mut self, op: &str, args: &[Expr], expected: Option<&Type>) -> R {
        let mut output_type = concrete(expected);
        let mut new_args = Vec::with_capacity(args.len());
        let mut i = 0;
        while i + 1 < args.len() {
            new_args.push(self.infer(&args[i], Some(&Type::Boolean))?.0);
            let (node, t) = self.infer(&args[i + 1], output_type.as_ref())?;
            output_type.get_or_insert(t);
            new_args.push(node);
            i += 2;
        }
        let (default_node, dt) = self.infer(&args[args.len() - 1], output_type.as_ref())?;
        output_type.get_or_insert(dt);
        new_args.push(default_node);
        Ok((
            Expr::Call {
                op: op.to_string(),
                args: new_args,
            },
            output_type.unwrap_or(Type::Value),
        ))
    }

    fn check_search_needle(&mut self, needle: &Expr) -> Result<Expr, ParseError> {
        let (node, t) = self.infer(needle, Some(&Type::Value))?;
        if !matches!(
            t,
            Type::Boolean | Type::String | Type::Number | Type::Null | Type::Value
        ) {
            return Err(ParseError::new(format!(
                "Expected first argument to be of type boolean, string, number or null, but found {t} instead."
            )));
        }
        Ok(node)
    }
}

fn call(op: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        op: op.to_string(),
        args,
    }
}

fn wrap(ty: Type, inner: Expr, coerce: bool) -> Expr {
    if coerce {
        Expr::Coerce(ty, Box::new(inner))
    } else {
        Expr::Assert(ty, Box::new(inner))
    }
}

/// Reconcile an inferred `actual` type against an `expected` one, inserting an
/// assertion or coercion node as MapLibre's `ParsingContext` would.
fn reconcile(node: Expr, actual: Type, expected: Option<&Type>, coerce_string: bool) -> R {
    let Some(exp) = expected else {
        return Ok((node, actual));
    };
    let assertable = matches!(
        exp,
        Type::String | Type::Number | Type::Boolean | Type::Object | Type::Array(..)
    );
    if assertable && matches!(actual, Type::Value) {
        let coerce = coerce_string && matches!(exp, Type::String);
        return Ok((wrap(exp.clone(), node, coerce), exp.clone()));
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
        return Ok((wrap(exp.clone(), node, true), exp.clone()));
    }
    if !is_subtype(exp, &actual) {
        return Err(ParseError::new(format!(
            "Expected {exp} but found {actual} instead."
        )));
    }
    Ok((node, actual))
}

/// The expectation to pass to output sub-expressions: a concrete expected type,
/// but not the `value` top type (which imposes no constraint).
fn concrete(expected: Option<&Type>) -> Option<Type> {
    match expected {
        Some(t) if !matches!(t, Type::Value) => Some(t.clone()),
        _ => None,
    }
}

/// Validate `match` branch labels (numbers or strings of one type, integer and
/// in range if numeric, unique across branches). Returns the common label type.
fn validate_match_labels(arms: &[(Vec<Value>, Expr)]) -> Result<Option<Type>, ParseError> {
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
    Ok(label_type)
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

type CurveId = *const Expr;

fn find_zoom_curve(expr: &Expr) -> Result<Option<CurveId>, ParseError> {
    let mut result: Option<CurveId> = match expr {
        // Transparent wrappers: recurse into the inner expression.
        Expr::Let { body, .. } => find_zoom_curve(body)?,
        Expr::Assert(_, inner) | Expr::Coerce(_, inner) => find_zoom_curve(inner)?,
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
    is_zoom(expr) || children(expr).iter().any(|c| references_zoom(c))
}

/// All direct sub-expression children of a node.
fn children(expr: &Expr) -> Vec<&Expr> {
    let mut out: Vec<&Expr> = Vec::new();
    match expr {
        Expr::Literal(_) | Expr::Var(_) => {}
        Expr::Assert(_, inner) | Expr::Coerce(_, inner) => out.push(inner),
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
