//! Evaluating a parsed [`Expr`] against an [`EvaluationContext`].

use std::collections::HashMap;

use crate::ast::{Expr, FormatArg, InterpKind, InterpSpace};
use crate::color::Color;
use crate::context::EvaluationContext;
use crate::error::{EvalError, EvalErrorKind};
use crate::ext::{NativeFn, Options, MAX_CALL_DEPTH};
use crate::typ::{is_subtype, Type};
use crate::value::{FormatSection, Value};

type Result<T> = std::result::Result<T, EvalError>;

/// A user function prepared for evaluation: parameter names plus a parsed body.
struct UserFn {
    params: Vec<String>,
    body: Expr,
}

/// Evaluate an expression against a context (no user functions).
pub fn eval(expr: &Expr, ctx: &EvaluationContext) -> Result<Value> {
    let funcs = HashMap::new();
    let natives = HashMap::new();
    let mut ev = Evaluator {
        ctx,
        scope: Vec::new(),
        funcs: &funcs,
        natives: &natives,
        depth: 0,
    };
    ev.eval(expr)
}

/// Evaluate with user functions and native functions from [`Options`].
pub fn eval_with(expr: &Expr, ctx: &EvaluationContext, opts: &Options) -> Result<Value> {
    let mut funcs = HashMap::new();
    for (name, f) in &opts.functions {
        let body = crate::parse::parse(&f.body, opts).map_err(|e| EvalError::new(e.to_string()))?;
        funcs.insert(
            name.clone(),
            UserFn {
                params: f.params.clone(),
                body,
            },
        );
    }
    let mut ev = Evaluator {
        ctx,
        scope: Vec::new(),
        funcs: &funcs,
        natives: &opts.natives,
        depth: 0,
    };
    ev.eval(expr)
}

struct Evaluator<'a> {
    ctx: &'a EvaluationContext,
    scope: Vec<(String, Value)>,
    funcs: &'a HashMap<String, UserFn>,
    natives: &'a HashMap<String, (usize, NativeFn)>,
    depth: usize,
}

impl Evaluator<'_> {
    fn eval(&mut self, expr: &Expr) -> Result<Value> {
        match expr {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Var(name) => self
                .scope
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    EvalError::of(EvalErrorKind::UnknownVariable { name: name.clone() })
                }),
            Expr::Let { bindings, body } => self.eval_let(bindings, body),
            Expr::Match {
                input,
                arms,
                default,
            } => self.eval_match(input, arms, default),
            Expr::Step {
                input,
                output0,
                stops,
            } => self.eval_step(input, output0, stops),
            Expr::Interpolate {
                kind,
                space,
                input,
                stops,
                projection,
            } => self.eval_interpolate(*kind, *space, input, stops, *projection),
            Expr::Call { op, args } => self.eval_call(op, args),
            Expr::Format(sections) => self.eval_format(sections),
            Expr::Within(polygons) => {
                let inside = match (
                    self.ctx.canonical,
                    self.ctx.feature.geometry_type.as_deref(),
                ) {
                    (Some(canon), Some(gt)) if !self.ctx.feature.geometry.is_empty() => {
                        crate::geometry::within(&self.ctx.feature.geometry, gt, canon, polygons)
                    }
                    _ => false,
                };
                Ok(Value::Bool(inside))
            }
            Expr::Distance(geoms) => {
                let d = match (
                    self.ctx.canonical,
                    self.ctx.feature.geometry_type.as_deref(),
                ) {
                    (Some((z, _, _)), Some(gt)) if !self.ctx.feature.geometry.is_empty() => {
                        crate::distance::distance(&self.ctx.feature.geometry, gt, z, geoms)
                    }
                    _ => f64::NAN,
                };
                Ok(Value::Number(d))
            }
            Expr::Collator {
                case_sensitive,
                diacritic_sensitive,
                locale,
            } => {
                let flag = |e: &Option<Box<Expr>>, this: &mut Self| -> Result<bool> {
                    match e {
                        Some(e) => Ok(this.eval(e)?.is_truthy()),
                        None => Ok(false),
                    }
                };
                Ok(Value::Collator {
                    case_sensitive: flag(case_sensitive, self)?,
                    diacritic_sensitive: flag(diacritic_sensitive, self)?,
                    locale: self.eval_opt_string(locale)?,
                })
            }
            Expr::NumberFormat {
                value,
                currency,
                min_fraction_digits,
                max_fraction_digits,
                unit,
                ..
            } => {
                let n = self.eval_number(value)?;
                let currency = self.eval_opt_string(currency)?;
                let unit = self.eval_opt_string(unit)?;
                let min_frac = self.eval_opt_number(min_fraction_digits)?;
                let max_frac = self.eval_opt_number(max_fraction_digits)?;
                Ok(Value::String(format_number_intl(
                    n,
                    currency.as_deref(),
                    unit.as_deref(),
                    min_frac.map(|v| v as usize),
                    max_frac.map(|v| v as usize),
                )))
            }
            Expr::Assert(ty, inner) => {
                let v = self.eval(inner)?;
                assert_value(ty, v)
            }
            Expr::Coerce(ty, inner) => {
                let v = self.eval(inner)?;
                coerce_value(ty, v)
            }
        }
    }

    fn eval_number(&mut self, expr: &Expr) -> Result<f64> {
        match self.eval(expr)? {
            Value::Number(n) => Ok(n),
            other => Err(type_error("number", &other)),
        }
    }

    fn eval_let(&mut self, bindings: &[(String, Expr)], body: &Expr) -> Result<Value> {
        let base = self.scope.len();
        for (name, value_expr) in bindings {
            let v = self.eval(value_expr)?;
            self.scope.push((name.clone(), v));
        }
        let result = self.eval(body);
        self.scope.truncate(base);
        result
    }

    fn eval_match(
        &mut self,
        input: &Expr,
        arms: &[(Vec<Value>, Expr)],
        default: &Expr,
    ) -> Result<Value> {
        let subject = self.eval(input)?;
        for (labels, output) in arms {
            if labels.iter().any(|l| values_equal(l, &subject)) {
                return self.eval(output);
            }
        }
        self.eval(default)
    }

    fn eval_step(&mut self, input: &Expr, output0: &Expr, stops: &[(f64, Expr)]) -> Result<Value> {
        let x = self.eval_number(input)?;
        let mut chosen = output0;
        for (stop, output) in stops {
            if x >= *stop {
                chosen = output;
            } else {
                break;
            }
        }
        self.eval(chosen)
    }

    fn eval_interpolate(
        &mut self,
        kind: InterpKind,
        space: InterpSpace,
        input: &Expr,
        stops: &[(f64, Expr)],
        projection: bool,
    ) -> Result<Value> {
        let x = self.eval_number(input)?;
        // Below the first / above the last stop: clamp to the endpoint (raw).
        if x <= stops[0].0 {
            return self.eval_stop(&stops[0].1, projection);
        }
        if x >= stops[stops.len() - 1].0 {
            return self.eval_stop(&stops[stops.len() - 1].1, projection);
        }
        // Find the bracketing pair.
        let mut idx = 0;
        for i in 0..stops.len() - 1 {
            if x >= stops[i].0 && x < stops[i + 1].0 {
                idx = i;
                break;
            }
        }
        let (lo, hi) = (stops[idx].0, stops[idx + 1].0);
        let lo_v = self.eval_stop(&stops[idx].1, projection)?;
        let hi_v = self.eval_stop(&stops[idx + 1].1, projection)?;
        let t = interpolation_factor(kind, x, lo, hi);
        if projection {
            use crate::value::Projection;
            let name = |v: &Value| match v {
                Value::String(s) => s.clone(),
                Value::Projection(Projection::Named(s)) => s.clone(),
                Value::Projection(Projection::Transition { from, .. }) => from.clone(),
                other => other.to_string(),
            };
            return Ok(Value::Projection(Projection::Transition {
                from: name(&lo_v),
                to: name(&hi_v),
                transition: t,
            }));
        }
        interpolate_values(&lo_v, &hi_v, t, space)
    }

    /// Evaluate a stop output. For projection outputs the value stays raw;
    /// otherwise a bare color string is parsed to a color.
    fn eval_stop(&mut self, expr: &Expr, projection: bool) -> Result<Value> {
        if projection {
            self.eval(expr)
        } else {
            self.eval_interp_output(expr)
        }
    }

    /// Evaluate an interpolation stop output, coercing bare color strings
    /// (e.g. `"red"`, `"#f00"`) to colors as MapLibre does when the output
    /// type is `color`.
    fn eval_interp_output(&mut self, expr: &Expr) -> Result<Value> {
        // Interpolation outputs can only be numbers, colors, or number arrays,
        // so a bare string output is always a color to be parsed.
        match self.eval(expr)? {
            Value::String(s) => match Color::parse(&s) {
                Some(c) => Ok(Value::Color(c)),
                None => Err(EvalError::of(EvalErrorKind::CouldNotParse {
                    ty: "color",
                    value: s.clone(),
                })),
            },
            other => Ok(other),
        }
    }

    fn eval_format(&mut self, sections: &[FormatArg]) -> Result<Value> {
        let mut out = Vec::with_capacity(sections.len());
        for s in sections {
            let content = self.eval(&s.content)?;
            let vertical_align = match &s.vertical_align {
                Some(e) => Some(self.eval_string(e)?),
                None => None,
            };
            if let Value::Image { name, available } = content {
                out.push(FormatSection {
                    text: String::new(),
                    image: Some((name, available)),
                    scale: None,
                    font_stack: None,
                    text_color: None,
                    vertical_align,
                });
                continue;
            }
            let scale = match &s.scale {
                Some(e) => Some(self.eval_number(e)?),
                None => None,
            };
            let font_stack = match &s.font {
                Some(e) => match self.eval(e)? {
                    Value::Array(a) => {
                        Some(a.iter().map(to_string_value).collect::<Vec<_>>().join(","))
                    }
                    _ => None,
                },
                None => None,
            };
            let text_color = match &s.text_color {
                Some(e) => match self.eval(e)? {
                    Value::Color(c) => Some(c),
                    _ => None,
                },
                None => None,
            };
            out.push(FormatSection {
                text: to_string_value(&content),
                image: None,
                scale,
                font_stack,
                text_color,
                vertical_align,
            });
        }
        Ok(Value::Formatted(out))
    }

    fn eval_call(&mut self, op: &str, args: &[Expr]) -> Result<Value> {
        // A user function takes priority: evaluate its arguments, bind them in a
        // fresh scope, and evaluate its body (recursion is depth-limited).
        let funcs = self.funcs;
        if let Some(func) = funcs.get(op) {
            if self.depth + 1 > MAX_CALL_DEPTH {
                return Err(EvalError::of(EvalErrorKind::MaxCallDepth {
                    op: op.to_string(),
                }));
            }
            let mut arg_values = Vec::with_capacity(args.len());
            for a in args {
                arg_values.push(self.eval(a)?);
            }
            let saved = std::mem::replace(
                &mut self.scope,
                func.params.iter().cloned().zip(arg_values).collect(),
            );
            self.depth += 1;
            let result = self.eval(&func.body);
            self.depth -= 1;
            self.scope = saved;
            return result;
        }
        // A native function: evaluate the arguments and hand them to the closure.
        let natives = self.natives;
        if let Some((_, f)) = natives.get(op) {
            let mut arg_values = Vec::with_capacity(args.len());
            for a in args {
                arg_values.push(self.eval(a)?);
            }
            return f(&arg_values, self.ctx);
        }
        match op {
            // --- feature / object lookups ---
            "get" => self.op_get(args),
            "has" => self.op_has(args),
            "properties" => Ok(Value::Object(self.ctx.feature.properties.clone())),
            "id" => Ok(self.ctx.feature.id.clone().unwrap_or(Value::Null)),
            "geometry-type" => Ok(self
                .ctx
                .feature
                .geometry_type
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null)),
            "zoom" => self
                .ctx
                .zoom
                .map(Value::Number)
                .ok_or_else(|| EvalError::of(EvalErrorKind::ZoomUnavailable)),
            "global-state" => {
                let key = self.eval_string(&args[0])?;
                Ok(self
                    .ctx
                    .global_state
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Null))
            }
            "feature-state" => {
                let key = self.eval_string(&args[0])?;
                Ok(self
                    .ctx
                    .feature
                    .state
                    .get(&key)
                    .cloned()
                    .unwrap_or(Value::Null))
            }
            "image" => {
                let name = self.eval_string(&args[0])?;
                let available = self.ctx.available_images.iter().any(|n| n == &name);
                Ok(Value::Image { name, available })
            }
            "resolved-locale" => match self.eval(&args[0])? {
                Value::Collator { locale, .. } => Ok(Value::String(locale.unwrap_or_default())),
                other => Err(type_error("collator", &other)),
            },
            "heatmap-density" => Ok(Value::Number(self.ctx.heatmap_density.unwrap_or(0.0))),
            "elevation" => Ok(Value::Number(self.ctx.elevation.unwrap_or(0.0))),
            "line-progress" => Ok(Value::Number(self.ctx.line_progress.unwrap_or(0.0))),
            // Without the RTL-text plugin the reference reports every script as
            // supported.
            "is-supported-script" => {
                self.eval(&args[0])?;
                Ok(Value::Bool(true))
            }
            "typeof" => Ok(Value::String(type_string(&self.eval(&args[0])?))),

            // --- collections ---
            "at" => self.op_at(args),
            "in" => self.op_in(args),
            "index-of" => self.op_index_of(args),
            "length" => self.op_length(args),
            "slice" => self.op_slice(args),

            // --- decisions / booleans ---
            "!" => Ok(Value::Bool(!self.eval(&args[0])?.is_truthy())),
            "all" => self.op_all(args),
            "any" => self.op_any(args),
            "case" => self.op_case(args),
            "coalesce" => self.op_coalesce(args),
            "==" => self.op_eq(args, true),
            "!=" => self.op_eq(args, false),
            "<" => self.op_cmp(op, args, Ordering::Lt),
            ">" => self.op_cmp(op, args, Ordering::Gt),
            "<=" => self.op_cmp(op, args, Ordering::Le),
            ">=" => self.op_cmp(op, args, Ordering::Ge),

            // --- arithmetic ---
            "+" => self.fold_num(args, 0.0, |a, b| a + b),
            "*" => self.fold_num(args, 1.0, |a, b| a * b),
            "-" => self.op_minus(args),
            "/" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a / b))
            }
            "%" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a % b))
            }
            "^" => {
                let a = self.eval_number(&args[0])?;
                let b = self.eval_number(&args[1])?;
                Ok(Value::Number(a.powf(b)))
            }
            "abs" => self.map_num(args, f64::abs),
            "ceil" => self.map_num(args, f64::ceil),
            "floor" => self.map_num(args, f64::floor),
            "round" => self.map_num(args, f64::round),
            "sqrt" => self.map_num(args, f64::sqrt),
            "sin" => self.map_num(args, f64::sin),
            "cos" => self.map_num(args, f64::cos),
            "tan" => self.map_num(args, f64::tan),
            "asin" => self.map_num(args, f64::asin),
            "acos" => self.map_num(args, f64::acos),
            "atan" => self.map_num(args, f64::atan),
            "ln" => self.map_num(args, f64::ln),
            "log2" => self.map_num(args, f64::log2),
            "log10" => self.map_num(args, f64::log10),
            "min" => self.fold_num(args, f64::INFINITY, f64::min),
            "max" => self.fold_num(args, f64::NEG_INFINITY, f64::max),
            "error" => Err(EvalError::new(self.eval_string(&args[0])?)),
            "e" => Ok(Value::Number(std::f64::consts::E)),
            "pi" => Ok(Value::Number(std::f64::consts::PI)),
            "ln2" => Ok(Value::Number(std::f64::consts::LN_2)),

            // --- strings ---
            "concat" => self.op_concat(args),
            "upcase" => Ok(Value::String(self.eval_string(&args[0])?.to_uppercase())),
            "downcase" => Ok(Value::String(self.eval_string(&args[0])?.to_lowercase())),
            "join" => self.op_join(args),
            "split" => self.op_split(args),

            // --- type assertions & conversions ---
            "array" => self.op_array(args),
            "boolean" => self.assert_type(args, "boolean", |v| matches!(v, Value::Bool(_))),
            "number" => self.assert_type(args, "number", |v| matches!(v, Value::Number(_))),
            "string" => self.assert_type(args, "string", |v| matches!(v, Value::String(_))),
            "object" => self.assert_type(args, "object", |v| matches!(v, Value::Object(_))),
            "to-boolean" => Ok(Value::Bool(self.eval(&args[0])?.is_truthy())),
            "to-number" => self.op_to_number(args),
            "to-string" => Ok(Value::String(to_string_value(&self.eval(&args[0])?))),
            "to-color" => self.op_to_color(args),
            "to-rgba" => self.op_to_rgba(args),
            "rgb" => self.op_rgb(args, false),
            "rgba" => self.op_rgb(args, true),

            other => Err(EvalError::of(EvalErrorKind::Unimplemented {
                op: other.to_string(),
            })),
        }
    }

    // ---- lookups ------------------------------------------------------

    fn op_get(&mut self, args: &[Expr]) -> Result<Value> {
        let key = self.eval_string(&args[0])?;
        if args.len() >= 2 {
            match self.eval(&args[1])? {
                Value::Object(o) => Ok(o.get(&key).cloned().unwrap_or(Value::Null)),
                other => Err(type_error("object", &other)),
            }
        } else {
            Ok(self
                .ctx
                .feature
                .properties
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null))
        }
    }

    fn op_has(&mut self, args: &[Expr]) -> Result<Value> {
        let key = self.eval_string(&args[0])?;
        let present = if args.len() >= 2 {
            match self.eval(&args[1])? {
                Value::Object(o) => o.contains_key(&key),
                other => return Err(type_error("object", &other)),
            }
        } else {
            self.ctx.feature.properties.contains_key(&key)
        };
        Ok(Value::Bool(present))
    }

    fn op_at(&mut self, args: &[Expr]) -> Result<Value> {
        let index = self.eval_number(&args[0])?;
        let array = match self.eval(&args[1])? {
            Value::Array(a) => a,
            other => return Err(type_error("array", &other)),
        };
        // Order mirrors MapLibre's `At`: negative, then out-of-range, then
        // non-integer — each with its own message.
        if index < 0.0 {
            return Err(EvalError::of(EvalErrorKind::ArrayIndexNegative { index }));
        }
        if index >= array.len() as f64 {
            return Err(EvalError::of(EvalErrorKind::ArrayIndexOutOfBounds {
                index,
                max: array.len().saturating_sub(1),
            }));
        }
        if index != index.trunc() {
            return Err(EvalError::of(EvalErrorKind::ArrayIndexNotInteger { index }));
        }
        Ok(array[index as usize].clone())
    }

    fn op_in(&mut self, args: &[Expr]) -> Result<Value> {
        let needle = self.eval(&args[0])?;
        let haystack = self.eval(&args[1])?;
        // Mirrors MapLibre: a falsy haystack (null, empty string) is a miss
        // before any type checking kicks in.
        if !haystack.is_truthy() {
            return Ok(Value::Bool(false));
        }
        require_searchable_needle(&needle)?;
        let found = match &haystack {
            Value::String(s) => s.contains(&js_string(&needle)),
            Value::Array(a) => a.iter().any(|v| values_equal(v, &needle)),
            other => return Err(arg_type_error("second argument", "array or string", other)),
        };
        Ok(Value::Bool(found))
    }

    fn op_index_of(&mut self, args: &[Expr]) -> Result<Value> {
        let needle = self.eval(&args[0])?;
        require_searchable_needle(&needle)?;
        let haystack = self.eval(&args[1])?;
        let from = if args.len() >= 3 {
            Some(self.eval_number(&args[2])?)
        } else {
            None
        };
        match &haystack {
            Value::String(s) => Ok(Value::Number(str_index_of(s, &js_string(&needle), from))),
            Value::Array(a) => Ok(Value::Number(array_index_of(a, &needle, from))),
            other => Err(arg_type_error("second argument", "array or string", other)),
        }
    }

    fn op_length(&mut self, args: &[Expr]) -> Result<Value> {
        match self.eval(&args[0])? {
            Value::String(s) => Ok(Value::Number(s.chars().count() as f64)),
            Value::Array(a) => Ok(Value::Number(a.len() as f64)),
            other => Err(type_error("string or array", &other)),
        }
    }

    fn op_slice(&mut self, args: &[Expr]) -> Result<Value> {
        let value = self.eval(&args[0])?;
        let begin = self.eval_number(&args[1])?;
        let end = if args.len() >= 3 {
            Some(self.eval_number(&args[2])?)
        } else {
            None
        };
        match value {
            Value::Array(a) => {
                let (s, e) = js_slice_bounds(begin, end, a.len());
                Ok(Value::Array(a[s..e].to_vec()))
            }
            Value::String(s) => {
                let chars: Vec<char> = s.chars().collect();
                let (a, b) = js_slice_bounds(begin, end, chars.len());
                Ok(Value::String(chars[a..b].iter().collect()))
            }
            other => Err(arg_type_error("first argument", "array or string", &other)),
        }
    }

    // ---- decisions ----------------------------------------------------

    fn op_all(&mut self, args: &[Expr]) -> Result<Value> {
        for a in args {
            if !self.eval(a)?.is_truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    }

    fn op_any(&mut self, args: &[Expr]) -> Result<Value> {
        for a in args {
            if self.eval(a)?.is_truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }

    fn op_case(&mut self, args: &[Expr]) -> Result<Value> {
        let mut i = 0;
        while i + 1 < args.len() {
            match self.eval(&args[i])? {
                Value::Bool(true) => return self.eval(&args[i + 1]),
                Value::Bool(false) => {}
                other => return Err(type_error("boolean", &other)),
            }
            i += 2;
        }
        self.eval(&args[args.len() - 1])
    }

    fn op_coalesce(&mut self, args: &[Expr]) -> Result<Value> {
        // Errors propagate; only null results (and unavailable images) are
        // skipped. If the final argument is an unavailable image, its name is
        // returned.
        let mut requested: Option<String> = None;
        let mut result = Value::Null;
        for (i, a) in args.iter().enumerate() {
            result = self.eval(a)?;
            if let Value::Image {
                name,
                available: false,
            } = &result
            {
                requested.get_or_insert_with(|| name.clone());
                result = if i + 1 == args.len() {
                    Value::String(requested.clone().unwrap())
                } else {
                    Value::Null
                };
            }
            if !matches!(result, Value::Null) {
                break;
            }
        }
        Ok(result)
    }

    fn op_eq(&mut self, args: &[Expr], want_equal: bool) -> Result<Value> {
        let a = self.eval(&args[0])?;
        let b = self.eval(&args[1])?;
        // A collator applies only when both operands are strings at runtime;
        // otherwise equality is by value (so 1 == "1" is false).
        let equal = match self.eval_collator(args)? {
            Some(c) if matches!(a, Value::String(_)) && matches!(b, Value::String(_)) => {
                collator_compare(&c, &a, &b) == Some(std::cmp::Ordering::Equal)
            }
            _ => values_equal(&a, &b),
        };
        Ok(Value::Bool(equal == want_equal))
    }

    fn op_cmp(&mut self, op: &str, args: &[Expr], ord: Ordering) -> Result<Value> {
        let a = self.eval(&args[0])?;
        let b = self.eval(&args[1])?;
        if let Some(c) = self.eval_collator(args)? {
            return Ok(Value::Bool(ord.test(collator_compare(&c, &a, &b))));
        }
        let result = match (&a, &b) {
            (Value::Number(x), Value::Number(y)) => ord.test(x.partial_cmp(y)),
            (Value::String(x), Value::String(y)) => ord.test(Some(x.cmp(y))),
            // Reached only when both operands were statically `value` (a single
            // typed operand is asserted at type-check time), so their runtime
            // types disagree or aren't ordered — MapLibre's combined-signature
            // error.
            _ => {
                return Err(EvalError::of(EvalErrorKind::NotOrderedComparable {
                    op: op.to_string(),
                    lhs: runtime_type_str(&a),
                    rhs: runtime_type_str(&b),
                }))
            }
        };
        Ok(Value::Bool(result))
    }

    /// Evaluate the optional third (collator) argument of a comparison.
    fn eval_collator(&mut self, args: &[Expr]) -> Result<Option<Value>> {
        match args.get(2) {
            Some(e) => Ok(Some(self.eval(e)?)),
            None => Ok(None),
        }
    }

    // ---- arithmetic helpers ------------------------------------------

    fn fold_num(&mut self, args: &[Expr], init: f64, f: fn(f64, f64) -> f64) -> Result<Value> {
        let mut acc = init;
        for a in args {
            acc = f(acc, self.eval_number(a)?);
        }
        Ok(Value::Number(acc))
    }

    fn map_num(&mut self, args: &[Expr], f: fn(f64) -> f64) -> Result<Value> {
        Ok(Value::Number(f(self.eval_number(&args[0])?)))
    }

    fn op_minus(&mut self, args: &[Expr]) -> Result<Value> {
        let a = self.eval_number(&args[0])?;
        if args.len() == 1 {
            Ok(Value::Number(-a))
        } else {
            Ok(Value::Number(a - self.eval_number(&args[1])?))
        }
    }

    // ---- strings ------------------------------------------------------

    fn eval_string(&mut self, expr: &Expr) -> Result<String> {
        match self.eval(expr)? {
            Value::String(s) => Ok(s),
            other => Err(type_error("string", &other)),
        }
    }

    fn eval_opt_string(&mut self, expr: &Option<Box<Expr>>) -> Result<Option<String>> {
        match expr {
            Some(e) => Ok(Some(self.eval_string(e)?)),
            None => Ok(None),
        }
    }

    fn eval_opt_number(&mut self, expr: &Option<Box<Expr>>) -> Result<Option<f64>> {
        match expr {
            Some(e) => Ok(Some(self.eval_number(e)?)),
            None => Ok(None),
        }
    }

    fn op_concat(&mut self, args: &[Expr]) -> Result<Value> {
        let mut out = String::new();
        for a in args {
            out.push_str(&to_string_value(&self.eval(a)?));
        }
        Ok(Value::String(out))
    }

    fn op_join(&mut self, args: &[Expr]) -> Result<Value> {
        let array = match self.eval(&args[0])? {
            Value::Array(a) => a,
            other => return Err(type_error("array", &other)),
        };
        let sep = self.eval_string(&args[1])?;
        let parts: Vec<String> = array.iter().map(to_string_value).collect();
        Ok(Value::String(parts.join(&sep)))
    }

    fn op_split(&mut self, args: &[Expr]) -> Result<Value> {
        let s = self.eval_string(&args[0])?;
        let sep = self.eval_string(&args[1])?;
        let parts: Vec<Value> = if sep.is_empty() {
            s.chars().map(|c| Value::String(c.to_string())).collect()
        } else {
            s.split(&sep)
                .map(|p| Value::String(p.to_string()))
                .collect()
        };
        Ok(Value::Array(parts))
    }

    // ---- type assertions & conversions -------------------------------

    fn assert_type(
        &mut self,
        args: &[Expr],
        name: &str,
        pred: fn(&Value) -> bool,
    ) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            if pred(&last) {
                return Ok(last);
            }
        }
        Err(type_error(name, &last))
    }

    fn op_array(&mut self, args: &[Expr]) -> Result<Value> {
        // ["array", value] | ["array", type, value...] | ["array", type, N, value...]
        // The item type is present with >= 2 args, the length (nullable) with
        // >= 3; the remaining args are fallback value candidates.
        let (item_type, n, value_start) = if args.len() >= 3 {
            let ty = self.eval_string(&args[0])?;
            let n = match self.eval(&args[1])? {
                Value::Null => None,
                Value::Number(x) => Some(x as usize),
                other => return Err(type_error("number", &other)),
            };
            (Some(ty), n, 2)
        } else if args.len() == 2 {
            (Some(self.eval_string(&args[0])?), None, 1)
        } else {
            (None, None, 0)
        };
        self.op_array_typed(item_type.as_deref(), n, &args[value_start..])
    }

    fn op_array_typed(
        &mut self,
        item_type: Option<&str>,
        n: Option<usize>,
        values: &[Expr],
    ) -> Result<Value> {
        let type_ok = |a: &[Value]| match item_type {
            Some("string") => a.iter().all(|v| matches!(v, Value::String(_))),
            Some("number") => a.iter().all(|v| matches!(v, Value::Number(_))),
            Some("boolean") => a.iter().all(|v| matches!(v, Value::Bool(_))),
            _ => true,
        };
        let mut last = Value::Null;
        for arg in values {
            last = self.eval(arg)?;
            if let Value::Array(a) = &last {
                if n.is_none_or(|n| a.len() == n) && type_ok(a) {
                    return Ok(last);
                }
            }
        }
        let desc = match (item_type, n) {
            (Some(t), Some(n)) => format!("array<{t}, {n}>"),
            (Some(t), None) => format!("array<{t}>"),
            _ => "array".to_string(),
        };
        Err(type_error(&desc, &last))
    }

    fn op_to_number(&mut self, args: &[Expr]) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            match &last {
                Value::Number(n) => return Ok(Value::Number(*n)),
                Value::Null => return Ok(Value::Number(0.0)),
                Value::Bool(b) => return Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
                Value::String(s) => {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return Ok(Value::Number(0.0));
                    }
                    if let Ok(n) = trimmed.parse::<f64>() {
                        return Ok(Value::Number(n));
                    }
                }
                _ => {}
            }
        }
        Err(EvalError::of(EvalErrorKind::CouldNotConvertToNumber {
            value: json_stringify(&last),
        }))
    }

    fn op_to_color(&mut self, args: &[Expr]) -> Result<Value> {
        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            if let Some(c) = coerce_color(&last) {
                return Ok(Value::Color(c));
            }
        }
        Err(match &last {
            // An array of the wrong length/shape has its own message.
            Value::Array(_) => EvalError::of(EvalErrorKind::InvalidRgba {
                value: json_stringify(&last),
                reason: "expected an array containing either three or four numeric values.",
            }),
            other => EvalError::of(EvalErrorKind::CouldNotParse {
                ty: "color",
                value: coercion_value_repr(other),
            }),
        })
    }

    fn op_to_rgba(&mut self, args: &[Expr]) -> Result<Value> {
        let value = self.eval(&args[0])?;
        // The argument type is Color; MapLibre coerces strings/arrays here.
        let color = coerce_color(&value).ok_or_else(|| type_error("color", &value))?;
        let [r, g, b, a] = color.to_rgba255();
        Ok(Value::Array(vec![
            Value::Number(r),
            Value::Number(g),
            Value::Number(b),
            Value::Number(a),
        ]))
    }

    fn op_rgb(&mut self, args: &[Expr], with_alpha: bool) -> Result<Value> {
        let r = self.eval_number(&args[0])?;
        let g = self.eval_number(&args[1])?;
        let b = self.eval_number(&args[2])?;
        let a = if with_alpha {
            self.eval_number(&args[3])?
        } else {
            1.0
        };
        let rgba = || {
            use crate::value::format_number as f;
            format!("[{}, {}, {}, {}]", f(r), f(g), f(b), f(a))
        };
        if [r, g, b].iter().any(|v| !(0.0..=255.0).contains(v)) {
            return Err(EvalError::of(EvalErrorKind::InvalidRgba {
                value: rgba(),
                reason: "'r', 'g', and 'b' must be between 0 and 255.",
            }));
        }
        if !(0.0..=1.0).contains(&a) {
            return Err(EvalError::of(EvalErrorKind::InvalidRgba {
                value: rgba(),
                reason: "'a' must be between 0 and 1.",
            }));
        }
        Ok(Value::Color(Color::from_rgba8(r, g, b, a)))
    }
}

// ---- free helpers -----------------------------------------------------

#[derive(Clone, Copy)]
enum Ordering {
    Lt,
    Gt,
    Le,
    Ge,
}

impl Ordering {
    fn test(self, ord: Option<std::cmp::Ordering>) -> bool {
        use std::cmp::Ordering as O;
        match ord {
            None => false,
            Some(o) => match self {
                Ordering::Lt => o == O::Less,
                Ordering::Gt => o == O::Greater,
                Ordering::Le => o != O::Greater,
                Ordering::Ge => o != O::Less,
            },
        }
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y,
        _ => a == b,
    }
}

/// The detailed type name reported by `typeof`, e.g. `"array<number, 3>"`,
/// mirroring MapLibre's `typeToString(typeOf(value))`.
fn type_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(_) => "string".to_string(),
        Value::Image { .. } => "resolvedImage".to_string(),
        Value::Formatted(_) => "formatted".to_string(),
        Value::NumberArray(_) => "numberArray".to_string(),
        Value::ColorArray(_) => "colorArray".to_string(),
        Value::Padding(_) => "padding".to_string(),
        Value::Projection(_) => "projectionDefinition".to_string(),
        Value::Collator { .. } => "collator".to_string(),
        Value::Color(_) => "color".to_string(),
        Value::Object(_) => "object".to_string(),
        Value::Array(items) => {
            let mut item_type: Option<String> = None;
            for it in items {
                let t = type_string(it);
                match &item_type {
                    None => item_type = Some(t),
                    Some(existing) if *existing == t => {}
                    Some(_) => {
                        item_type = Some("value".to_string());
                        break;
                    }
                }
            }
            format!(
                "array<{}, {}>",
                item_type.unwrap_or_else(|| "value".to_string()),
                items.len()
            )
        }
    }
}

/// The runtime type of a value, rendered the way MapLibre's `typeOf` +
/// `toString` do: arrays become `array<itemType, length>`, where the item type
/// is the common element type or `value` when they differ.
fn runtime_type_str(v: &Value) -> String {
    match v {
        Value::Array(a) => {
            let mut item: Option<String> = None;
            for e in a {
                let t = runtime_type_str(e);
                match &item {
                    None => item = Some(t),
                    Some(prev) if *prev == t => {}
                    Some(_) => {
                        item = Some("value".to_string());
                        break;
                    }
                }
            }
            format!(
                "array<{}, {}>",
                item.unwrap_or_else(|| "value".to_string()),
                a.len()
            )
        }
        other => other.type_name().to_string(),
    }
}

fn type_error(expected: &str, found: &Value) -> EvalError {
    EvalError::of(crate::error::EvalErrorKind::TypeMismatch {
        expected: expected.to_string(),
        found: runtime_type_str(found),
    })
}

/// A `JSON.stringify`-equivalent rendering of a value (strings quoted, arrays
/// and objects compact), used verbatim in several MapLibre error messages.
fn json_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\"")),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| format!("\"{k}\""));
                    format!("{key}:{}", json_stringify(val))
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Render a value for a "Could not parse ... from value '...'" message the way
/// MapLibre does: `typeof input === 'string' ? input : JSON.stringify(input)`.
fn coercion_value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => json_stringify(other),
    }
}

/// Like [`type_error`], but naming the offending argument (e.g. the `in` /
/// `index-of` haystack is the "second argument").
fn arg_type_error(arg: &'static str, expected: &str, found: &Value) -> EvalError {
    EvalError::of(crate::error::EvalErrorKind::TypeMismatchArg {
        arg,
        expected: expected.to_string(),
        found: runtime_type_str(found),
    })
}

/// A type-directed runtime assertion (`Expr::Assert`): the value must already
/// be of the asserted type, or evaluation errors.
fn assert_value(ty: &Type, v: Value) -> Result<Value> {
    if is_subtype(ty, &Type::of_value(&v)) {
        Ok(v)
    } else {
        Err(type_error(&ty.to_string(), &v))
    }
}

/// A type-directed runtime coercion (`Expr::Coerce`): convert the value to the
/// target type, matching MapLibre's `Coercion`.
fn coerce_value(ty: &Type, v: Value) -> Result<Value> {
    match ty {
        Type::String => Ok(Value::String(to_string_value(&v))),
        Type::Boolean => Ok(Value::Bool(v.is_truthy())),
        Type::Number => match &v {
            Value::Number(n) => Ok(Value::Number(*n)),
            Value::Null => Ok(Value::Number(0.0)),
            Value::Bool(b) => Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
            Value::String(s) => {
                let t = s.trim();
                if t.is_empty() {
                    Ok(Value::Number(0.0))
                } else {
                    t.parse::<f64>().map(Value::Number).map_err(|_| {
                        EvalError::of(EvalErrorKind::CouldNotConvertToNumber {
                            value: t.to_string(),
                        })
                    })
                }
            }
            _ => Err(type_error("number", &v)),
        },
        Type::Color => match coerce_color(&v) {
            Some(c) => Ok(Value::Color(c)),
            None => Err(EvalError::of(EvalErrorKind::CouldNotParse {
                ty: "color",
                value: coercion_value_repr(&v),
            })),
        },
        Type::Formatted => Ok(match v {
            Value::Formatted(_) => v,
            other => Value::Formatted(vec![FormatSection {
                text: to_string_value(&other),
                image: None,
                scale: None,
                font_stack: None,
                text_color: None,
                vertical_align: None,
            }]),
        }),
        Type::NumberArray => coerce_number_array(v),
        Type::Padding => coerce_padding(v),
        Type::ColorArray => coerce_color_array(v),
        Type::ProjectionDefinition => coerce_projection(v),
        // Types without a dedicated runtime coercion pass through unchanged.
        _ => Ok(v),
    }
}

fn coerce_number_array(v: Value) -> Result<Value> {
    match &v {
        Value::NumberArray(_) => Ok(v),
        Value::Number(n) => Ok(Value::NumberArray(vec![*n])),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a {
                match e {
                    Value::Number(n) => out.push(*n),
                    other => return Err(type_error("number", other)),
                }
            }
            Ok(Value::NumberArray(out))
        }
        _ => Err(EvalError::of(EvalErrorKind::CouldNotParse {
            ty: "numberArray",
            value: coercion_value_repr(&v),
        })),
    }
}

fn coerce_padding(v: Value) -> Result<Value> {
    let err = || {
        EvalError::of(EvalErrorKind::CouldNotParse {
            ty: "padding",
            value: coercion_value_repr(&v),
        })
    };
    match &v {
        Value::Padding(_) => Ok(v),
        Value::Number(n) => Ok(Value::Padding([*n; 4])),
        Value::Array(a) => {
            let ns: Option<Vec<f64>> = a.iter().map(Value::as_number).collect();
            let ns = ns.ok_or_else(err)?;
            let p = match ns.len() {
                1 => [ns[0]; 4],
                2 => [ns[0], ns[1], ns[0], ns[1]],
                3 => [ns[0], ns[1], ns[2], ns[1]],
                4 => [ns[0], ns[1], ns[2], ns[3]],
                _ => return Err(err()),
            };
            Ok(Value::Padding(p))
        }
        _ => Err(err()),
    }
}

fn coerce_color_array(v: Value) -> Result<Value> {
    let err = || {
        EvalError::of(EvalErrorKind::CouldNotParse {
            ty: "colorArray",
            value: coercion_value_repr(&v),
        })
    };
    match &v {
        Value::ColorArray(_) => Ok(v),
        Value::Color(c) => Ok(Value::ColorArray(vec![*c])),
        Value::String(s) => Color::parse(s)
            .map(|c| Value::ColorArray(vec![c]))
            .ok_or_else(err),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a {
                match coerce_color(e) {
                    Some(c) => out.push(c),
                    None => return Err(err()),
                }
            }
            Ok(Value::ColorArray(out))
        }
        _ => Err(err()),
    }
}

fn coerce_projection(v: Value) -> Result<Value> {
    use crate::value::Projection;
    match &v {
        Value::Projection(_) => Ok(v),
        Value::String(s) => Ok(Value::Projection(Projection::Named(s.clone()))),
        Value::Array(a) if a.len() == 3 => match (a[0].as_str(), a[1].as_str(), a[2].as_number()) {
            (Some(from), Some(to), Some(t)) => Ok(Value::Projection(Projection::Transition {
                from: from.to_string(),
                to: to.to_string(),
                transition: t,
            })),
            _ => Err(EvalError::of(EvalErrorKind::CouldNotParse {
                ty: "projection",
                value: coercion_value_repr(&v),
            })),
        },
        _ => Err(EvalError::of(EvalErrorKind::CouldNotParse {
            ty: "projection",
            value: coercion_value_repr(&v),
        })),
    }
}

/// `in` / `index-of` accept only primitive needles.
fn require_searchable_needle(needle: &Value) -> Result<()> {
    match needle {
        Value::Bool(_) | Value::String(_) | Value::Number(_) | Value::Null => Ok(()),
        other => Err(EvalError::of(EvalErrorKind::SearchNeedle {
            found: other.type_name().to_string(),
        })),
    }
}

/// How a value stringifies when searched inside a string haystack, matching
/// JavaScript's coercion in `String.prototype.indexOf` (`null` -> `"null"`).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `String.prototype.indexOf` over code points: negative `from` clamps to 0.
fn str_index_of(hay: &str, needle: &str, from: Option<f64>) -> f64 {
    let hay: Vec<char> = hay.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let start = from.map_or(0, |f| {
        if f < 0.0 {
            0
        } else {
            (f as usize).min(hay.len())
        }
    });
    if needle.is_empty() {
        return start.min(hay.len()) as f64;
    }
    if needle.len() > hay.len() {
        return -1.0;
    }
    for i in start..=hay.len() - needle.len() {
        if hay[i..i + needle.len()] == needle[..] {
            return i as f64;
        }
    }
    -1.0
}

/// `Array.prototype.indexOf`: negative `from` counts back from the end.
fn array_index_of(arr: &[Value], needle: &Value, from: Option<f64>) -> f64 {
    let len = arr.len();
    let start = match from {
        Some(f) if f < 0.0 => (len as f64 + f).max(0.0) as usize,
        Some(f) => (f as usize).min(len),
        None => 0,
    };
    arr.iter()
        .enumerate()
        .skip(start)
        .find(|(_, v)| values_equal(v, needle))
        .map_or(-1.0, |(i, _)| i as f64)
}

/// Normalize `slice(begin, end)` indices the way JavaScript's `slice` does:
/// negatives count from the end, everything clamps to `0..=len`, and an empty
/// range yields `start == end`.
fn js_slice_bounds(begin: f64, end: Option<f64>, len: usize) -> (usize, usize) {
    let clamp = |i: f64| -> usize {
        if i < 0.0 {
            (len as f64 + i).max(0.0) as usize
        } else {
            (i as usize).min(len)
        }
    };
    let start = clamp(begin);
    let stop = end.map_or(len, clamp);
    (start, stop.max(start))
}

/// Coerce a value to a [`Color`]: pass colors through, parse CSS strings, and
/// read `[r, g, b]` / `[r, g, b, a]` numeric arrays (channels in `0..=255`).
fn coerce_color(v: &Value) -> Option<Color> {
    match v {
        Value::Color(c) => Some(*c),
        Value::String(s) => Color::parse(s),
        Value::Array(a) if a.len() == 3 || a.len() == 4 => {
            let n = |i: usize| a.get(i).and_then(Value::as_number);
            match (n(0), n(1), n(2)) {
                (Some(r), Some(g), Some(b)) => {
                    Some(Color::from_rgba8(r, g, b, n(3).unwrap_or(1.0)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// MapLibre's `toString`: `null` -> `""`, scalars via their native string form,
/// colors as `rgba(...)`, and arrays/objects as compact JSON.
fn to_string_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => s.clone(),
        Value::Color(c) => c.to_string(),
        Value::Image { name, .. } => name.clone(),
        Value::Formatted(sections) => sections.iter().map(|s| s.text.clone()).collect(),
        Value::NumberArray(_)
        | Value::ColorArray(_)
        | Value::Padding(_)
        | Value::Projection(_)
        | Value::Collator { .. } => v.to_string(),
        Value::Array(_) | Value::Object(_) => json_string(v),
    }
}

/// Compact JSON serialization for `to-string`/`concat` of arrays and objects.
fn json_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::value::format_number(*n),
        Value::String(s) => format!("{s:?}"),
        Value::Color(c) => format!("{:?}", c.to_string()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_string).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{k:?}:{}", json_string(val)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Image { name, available } => {
            format!("{{\"name\":{name:?},\"available\":{available}}}")
        }
        Value::Formatted(sections) => {
            let s: String = sections.iter().map(|s| s.text.clone()).collect();
            format!("{s:?}")
        }
        Value::NumberArray(_) | Value::Padding(_) => {
            let nums = match v {
                Value::NumberArray(a) => a.clone(),
                Value::Padding(a) => a.to_vec(),
                _ => unreachable!(),
            };
            let parts: Vec<String> = nums
                .iter()
                .map(|n| crate::value::format_number(*n))
                .collect();
            format!("{{\"values\":[{}]}}", parts.join(","))
        }
        Value::ColorArray(_) | Value::Projection(_) | Value::Collator { .. } => {
            format!("{:?}", v.to_string())
        }
    }
}

fn interpolation_factor(kind: InterpKind, x: f64, lo: f64, hi: f64) -> f64 {
    let span = hi - lo;
    match kind {
        InterpKind::Linear => (x - lo) / span,
        InterpKind::Exponential(base) => {
            if (base - 1.0).abs() < f64::EPSILON {
                (x - lo) / span
            } else {
                (base.powf(x - lo) - 1.0) / (base.powf(span) - 1.0)
            }
        }
        InterpKind::CubicBezier(x1, y1, x2, y2) => {
            let t = (x - lo) / span;
            unit_bezier(x1, y1, x2, y2, t)
        }
    }
}

fn interpolate_values(lo: &Value, hi: &Value, t: f64, space: InterpSpace) -> Result<Value> {
    match (lo, hi) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + (b - a) * t)),
        (Value::Color(a), Value::Color(b)) => Ok(Value::Color(interpolate_color(*a, *b, t, space))),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            let mut out = Vec::with_capacity(a.len());
            for (x, y) in a.iter().zip(b) {
                out.push(interpolate_values(x, y, t, space)?);
            }
            Ok(Value::Array(out))
        }
        (Value::NumberArray(a), Value::NumberArray(b)) if a.len() == b.len() => Ok(
            Value::NumberArray(a.iter().zip(b).map(|(x, y)| x + (y - x) * t).collect()),
        ),
        (Value::Padding(a), Value::Padding(b)) => {
            let mut p = [0.0; 4];
            for i in 0..4 {
                p[i] = a[i] + (b[i] - a[i]) * t;
            }
            Ok(Value::Padding(p))
        }
        (Value::ColorArray(a), Value::ColorArray(b)) if a.len() == b.len() => {
            Ok(Value::ColorArray(
                a.iter()
                    .zip(b)
                    .map(|(x, y)| interpolate_color(*x, *y, t, space))
                    .collect(),
            ))
        }
        (Value::Projection(a), Value::Projection(b)) => {
            use crate::value::Projection;
            let name = |p: &Projection| match p {
                Projection::Named(s) => s.clone(),
                Projection::Transition { from, .. } => from.clone(),
            };
            Ok(Value::Projection(Projection::Transition {
                from: name(a),
                to: name(b),
                transition: t,
            }))
        }
        _ => Err(EvalError::of(EvalErrorKind::InterpolationOutputs)),
    }
}

fn interpolate_color(a: Color, b: Color, t: f64, space: InterpSpace) -> Color {
    let lerp = |x: f64, y: f64| x + (y - x) * t;
    match space {
        InterpSpace::Rgb => Color::new(
            lerp(a.r, b.r),
            lerp(a.g, b.g),
            lerp(a.b, b.b),
            lerp(a.a, b.a),
        ),
        InterpSpace::Lab => {
            let [l0, a0, b0, al0] = a.to_lab();
            let [l1, a1, b1, al1] = b.to_lab();
            Color::from_lab([lerp(l0, l1), lerp(a0, a1), lerp(b0, b1), lerp(al0, al1)])
        }
        InterpSpace::Hcl => {
            // Hue takes the shortest path around the circle; NaN (achromatic)
            // hues pin to the defined endpoint. Mirrors chroma.js / MapLibre.
            let [h0, c0, l0, al0] = a.to_hcl();
            let [h1, c1, l1, al1] = b.to_hcl();
            let (hue, chroma) = if !h0.is_nan() && !h1.is_nan() {
                let mut dh = h1 - h0;
                if h1 > h0 && dh > 180.0 {
                    dh -= 360.0;
                } else if h1 < h0 && h0 - h1 > 180.0 {
                    dh += 360.0;
                }
                (h0 + t * dh, lerp(c0, c1))
            } else if !h0.is_nan() {
                (
                    h0,
                    if l1 == 1.0 || l1 == 0.0 {
                        c0
                    } else {
                        lerp(c0, c1)
                    },
                )
            } else if !h1.is_nan() {
                (
                    h1,
                    if l0 == 1.0 || l0 == 0.0 {
                        c1
                    } else {
                        lerp(c0, c1)
                    },
                )
            } else {
                (f64::NAN, lerp(c0, c1))
            };
            Color::from_hcl([hue, chroma, lerp(l0, l1), lerp(al0, al1)])
        }
    }
}

/// Solve a unit cubic Bézier easing curve for `y` at parameter `x` (both in
/// `0..=1`), matching MapLibre's `UnitBezier` implementation.
fn unit_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;

    let sample_x = |t: f64| ((ax * t + bx) * t + cx) * t;
    let sample_y = |t: f64| ((ay * t + by) * t + cy) * t;
    let sample_dx = |t: f64| (3.0 * ax * t + 2.0 * bx) * t + cx;

    // Newton-Raphson, then bisection fallback.
    let mut t = x;
    for _ in 0..8 {
        let x2 = sample_x(t) - x;
        if x2.abs() < 1e-6 {
            return sample_y(t);
        }
        let d = sample_dx(t);
        if d.abs() < 1e-6 {
            break;
        }
        t -= x2 / d;
    }
    let (mut lo, mut hi, mut t) = (0.0, 1.0, x);
    while lo < hi {
        let x2 = sample_x(t);
        if (x2 - x).abs() < 1e-6 {
            return sample_y(t);
        }
        if x > x2 {
            lo = t;
        } else {
            hi = t;
        }
        t = (hi - lo) * 0.5 + lo;
    }
    sample_y(t)
}

// ---- number-format (en-US) --------------------------------------------

fn currency_digits(code: &str) -> usize {
    // Currencies with zero fractional digits; everything else uses two.
    match code {
        "JPY" | "KRW" | "CLP" | "VND" | "ISK" | "HUF" => 0,
        _ => 2,
    }
}

fn currency_symbol(code: &str) -> String {
    match code {
        "USD" => "$".into(),
        "EUR" => "€".into(),
        "JPY" | "CNY" => "¥".into(),
        "GBP" => "£".into(),
        "KRW" => "₩".into(),
        other => format!("{other}\u{a0}"),
    }
}

fn unit_suffix(unit: &str) -> String {
    match unit {
        "celsius" => "°C".into(),
        "meter" => " m".into(),
        "kilometer" => " km".into(),
        "centimeter" => " cm".into(),
        "millimeter" => " mm".into(),
        "kilobyte" => " kB".into(),
        "megabyte" => " MB".into(),
        "byte" => " byte".into(),
        "percent" => "%".into(),
        other => format!(" {other}"),
    }
}

/// Format a number the way `Intl.NumberFormat('en-US', ...)` does for the
/// options exercised by the spec fixtures.
fn format_number_intl(
    n: f64,
    currency: Option<&str>,
    unit: Option<&str>,
    min_frac: Option<usize>,
    max_frac: Option<usize>,
) -> String {
    let (def_min, def_max) = match currency {
        Some(code) => {
            let d = currency_digits(code);
            (d, d)
        }
        None => (0, 3),
    };
    let min = min_frac.unwrap_or(def_min);
    let max = max_frac.unwrap_or(def_max).max(min);
    let body = format_decimal_us(n, min, max);
    if let Some(code) = currency {
        return format!("{}{}", currency_symbol(code), body);
    }
    if let Some(u) = unit {
        return format!("{}{}", body, unit_suffix(u));
    }
    body
}

fn format_decimal_us(n: f64, min: usize, max: usize) -> String {
    let neg = n < 0.0;
    // The shortest round-trip decimal, matching JavaScript's Number->String.
    let s = format!("{}", n.abs());
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s, String::new()),
    };
    let (int_r, frac_r) = round_decimal(&int_part, &frac_part, max);
    let mut frac = frac_r;
    while frac.len() < min {
        frac.push('0');
    }
    while frac.len() > min && frac.ends_with('0') {
        frac.pop();
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&group_thousands(&int_r));
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    out
}

/// Round a decimal `int.frac` to `max` fraction digits (half-up), returning the
/// new integer and fraction parts.
fn round_decimal(int: &str, frac: &str, max: usize) -> (String, String) {
    if frac.len() <= max {
        return (int.to_string(), frac.to_string());
    }
    let round_up = frac.as_bytes()[max] >= b'5';
    let mut digits: Vec<u8> = format!("{int}{}", &frac[..max]).into_bytes();
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s = String::from_utf8(digits).unwrap();
    let split = s.len() - max;
    (s[..split].to_string(), s[split..].to_string())
}

fn group_thousands(int: &str) -> String {
    let bytes = int.as_bytes();
    let mut out = String::new();
    let n = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

// ---- collator comparison ----------------------------------------------

/// Compare two values with a collator, mirroring `Intl.Collator.compare`.
/// Intl's `sensitivity` is expressed as an ICU collation strength plus a case
/// level: base → primary, accent → secondary, case → primary + case level,
/// variant → tertiary. Locale tailoring comes from CLDR via `icu_collator`.
fn collator_compare(collator: &Value, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use icu::collator::{
        options::{CaseLevel, CollatorOptions, Strength},
        Collator, CollatorPreferences,
    };
    use icu::locale::Locale;

    let Value::Collator {
        case_sensitive,
        diacritic_sensitive,
        locale,
    } = collator
    else {
        return None;
    };

    let (strength, case_level) = match (*case_sensitive, *diacritic_sensitive) {
        (false, false) => (Strength::Primary, CaseLevel::Off),
        (false, true) => (Strength::Secondary, CaseLevel::Off),
        (true, false) => (Strength::Primary, CaseLevel::On),
        (true, true) => (Strength::Tertiary, CaseLevel::Off),
    };
    let mut options = CollatorOptions::default();
    options.strength = Some(strength);
    options.case_level = Some(case_level);

    let prefs: CollatorPreferences = match locale {
        Some(l) => {
            // German uses phonebook ordering here (ü ≈ ue, ä sorts after a),
            // matching the reference fixtures.
            let tag = if l.split(['-', '_']).next() == Some("de") && !l.contains("-co-") {
                "de-u-co-phonebk".to_string()
            } else {
                l.clone()
            };
            match tag.parse::<Locale>() {
                Ok(loc) => (&loc).into(),
                Err(_) => CollatorPreferences::default(),
            }
        }
        None => CollatorPreferences::default(),
    };
    let coll = Collator::try_new(prefs, options).ok()?;
    Some(coll.compare(&to_string_value(a), &to_string_value(b)))
}
