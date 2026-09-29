//! Evaluating a parsed [`Expr`] against an [`EvaluationContext`].

use std::collections::HashMap;

use crate::ast::{Expr, FormatArg, InterpKind, InterpSpace};
use crate::color::Color;
use crate::context::EvaluationContext;
use crate::error::{EvalError, EvalErrorKind};
use crate::ext::{CompiledFn, ExternalFn, Options, MAX_CALL_DEPTH};
use crate::typ::{is_subtype, Type};
use crate::value::{FormatSection, Value};

type Result<T> = std::result::Result<T, EvalError>;

/// Evaluate an expression against a context (no user extensions).
pub(crate) fn eval(expr: &Expr, ctx: &EvaluationContext) -> Result<Value> {
    let funcs = HashMap::new();
    let externals = HashMap::new();
    let mut ev = Evaluator {
        ctx,
        scope: Vec::new(),
        funcs: &funcs,
        externals: &externals,
        depth: 0,
    };
    ev.eval(expr)
}

/// Evaluate with expression functions and external functions from [`Options`].
pub(crate) fn eval_with(expr: &Expr, ctx: &EvaluationContext, opts: &Options) -> Result<Value> {
    // Bodies are parsed once per `Options` and cached; see `Options::compiled_fns`.
    let funcs = opts
        .compiled_fns()
        .map_err(|e| EvalError::new(e.to_string()))?;
    let mut ev = Evaluator {
        ctx,
        scope: Vec::new(),
        funcs,
        externals: &opts.externals,
        depth: 0,
    };
    ev.eval(expr)
}

struct Evaluator<'a> {
    ctx: &'a EvaluationContext,
    scope: Vec<(String, Value)>,
    funcs: &'a HashMap<String, CompiledFn>,
    externals: &'a HashMap<String, (usize, ExternalFn)>,
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
                let min_frac = self
                    .eval_opt_number(min_fraction_digits)?
                    .map(|v| fraction_digit_option("minimumFractionDigits", v))
                    .transpose()?;
                let max_frac = self
                    .eval_opt_number(max_fraction_digits)?
                    .map(|v| fraction_digit_option("maximumFractionDigits", v))
                    .transpose()?;
                Ok(Value::String(format_number_intl(
                    n,
                    currency.as_deref(),
                    unit.as_deref(),
                    min_frac,
                    max_frac,
                )?))
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
        // Interpolatable outputs are wider than numbers and colors — padding,
        // colorArray, projectionDefinition and variableAnchorOffsetCollection
        // interpolate too (see `is_interpolatable`). Those all reach here
        // already converted, because type-checking against the property's
        // expected type wraps each stop in the matching coercion. A *bare*
        // string therefore only survives to this point when the expected type
        // was unknown, where MapLibre reads it as a color literal.
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
        // An expression function takes priority: evaluate its arguments, bind them in a
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
        // An external function: evaluate the arguments and hand them to the closure.
        let externals = self.externals;
        if let Some((_, f)) = externals.get(op) {
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
            // Unconditionally true, matching upstream. `is-supported-script`
            // consults `ctx.globals.isSupportedScript`, a hook the renderer
            // installs only once the RTL-text plugin is loaded, and returns
            // `true` when it is absent:
            //
            //     const isSupportedScript = ctx.globals && ctx.globals.isSupportedScript;
            //     if (isSupportedScript) { return isSupportedScript(s.evaluate(ctx)); }
            //     return true;
            //
            // (`src/expression/compound_expression.ts:500-511` at the pinned
            // upstream commit; the comment there reads "At parse time this will
            // always return true".) maplibre-style-spec never sets that global
            // itself — it is `GlobalProperties.isSupportedScript?`, declared in
            // `src/expression/index.ts:109` and only forwarded — so the
            // spec-level behaviour this crate ports is the `true` branch, and
            // the evaluation context here has no plugin hook to install.
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
                max: array.len() as i64 - 1,
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
        // The runtime type check comes first, before the collator is even
        // looked at: MapLibre throws in `comparison.ts:167-179` and only then
        // reaches the `this.collator ? …` return on line 189. A collator on an
        // untyped ordered comparison must therefore not smuggle non-string,
        // non-number operands past this.
        let result = match (&a, &b) {
            (Value::Number(x), Value::Number(y)) => ord.test(x.partial_cmp(y)),
            (Value::String(x), Value::String(y)) => ord.test(Some(x.cmp(y))),
            // Reached only when both operands were statically `value` (a single
            // typed operand is asserted at type-check time), so their runtime
            // types disagree or aren't ordered — MapLibre's combined-signature
            // error. It names the bare type *kinds* (`${lt.kind}`), not the
            // `typeToString` rendering, so an array is `array`, not
            // `array<number, 1>`.
            _ => {
                return Err(EvalError::of(EvalErrorKind::NotOrderedComparable {
                    op: op.to_string(),
                    lhs: a.type_name().to_string(),
                    rhs: b.type_name().to_string(),
                }))
            }
        };
        if let Some(c) = self.eval_collator(args)? {
            return Ok(Value::Bool(ord.test(collator_compare(&c, &a, &b))));
        }
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

    /// `to-number`, transcribing `Coercion.evaluate`'s `'number'` case
    /// (`src/expression/definitions/coercion.ts:153-163`):
    ///
    /// ```js
    /// let value = null;
    /// for (const arg of this.args) {
    ///     value = arg.evaluate(ctx);
    ///     if (value === null) return 0;
    ///     const num = Number(value);
    ///     if (isNaN(num)) continue;
    ///     return num;
    /// }
    /// throw new RuntimeError(`Could not convert ${JSON.stringify(value)} to number.`);
    /// ```
    ///
    /// Two details are easy to lose. `null` short-circuits to `0` *without*
    /// trying the remaining arguments, while every other non-numeric argument
    /// falls through to the next one. And `isNaN(num) → continue` covers a NaN
    /// that arrives as a number as much as one produced by the conversion, so
    /// `["to-number", NaN]` throws rather than answering NaN — with
    /// `JSON.stringify(NaN)` being `"null"`, hence
    /// `Could not convert null to number.`
    fn op_to_number(&mut self, args: &[Expr]) -> Result<Value> {
        /// `Number(string)` (ECMA-262 §7.1.4.1, `StringToNumber`). `None` is
        /// JavaScript's `NaN`.
        fn number_from_str(s: &str) -> Option<f64> {
            // `StrWhiteSpace` is Unicode whitespace plus U+FEFF; an all-blank
            // (or empty) string is `0`.
            let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            if t.is_empty() {
                return Some(0.0);
            }
            match t {
                "Infinity" | "+Infinity" => return Some(f64::INFINITY),
                "-Infinity" => return Some(f64::NEG_INFINITY),
                _ => {}
            }
            // `NonDecimalIntegerLiteral`: unsigned, and with at least one digit.
            if let Some(radix) = match t.get(..2) {
                Some("0x" | "0X") => Some(16u32),
                Some("0o" | "0O") => Some(8),
                Some("0b" | "0B") => Some(2),
                _ => None,
            } {
                let digits = &t[2..];
                if digits.is_empty() {
                    return None;
                }
                // Accumulated in `f64` rather than an integer so that literals
                // beyond `u128` round to the nearest double instead of
                // overflowing, as JavaScript's `MV` does.
                let mut acc = 0.0f64;
                for c in digits.chars() {
                    let d = c.to_digit(radix)?;
                    acc = acc * f64::from(radix) + f64::from(d);
                }
                return Some(acc);
            }
            // `StrDecimalLiteral`. Rust's `f64::from_str` accepts exactly this
            // grammar *plus* `inf`, `infinity` and `nan`, which JavaScript does
            // not — rejecting every letter but the exponent's `e` rules those
            // out (`Infinity` itself was handled above).
            if t.chars()
                .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
            {
                return None;
            }
            t.parse::<f64>().ok()
        }

        /// `Number(value)`, i.e. `ToNumber(ToPrimitive(value))`. `None` is NaN.
        fn number_of(v: &Value) -> Option<f64> {
            match v {
                Value::Null => Some(0.0),
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                Value::Number(n) => Some(*n),
                Value::String(s) => number_from_str(s),
                // An array coerces through `Array.prototype.toString`, so
                // `["to-number", ["literal", [7]]]` is `7` and `[]` is `0`.
                // Any other element type stringifies to something non-numeric
                // (`"[object Object]"`, `"rgba(…)"`), which is NaN either way.
                Value::Array(items) => number_from_str(&array_to_string(items)?),
                _ => None,
            }
        }

        /// `Array.prototype.toString`: elements joined by `","`, with `null`
        /// contributing the empty string. `None` where an element has no
        /// numeric-looking stringification, which makes the whole join NaN.
        fn array_to_string(items: &[Value]) -> Option<String> {
            let mut out = String::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match item {
                    Value::Null => {}
                    Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Value::Number(n) => out.push_str(&crate::value::format_number(*n)),
                    Value::String(s) => out.push_str(s),
                    Value::Array(nested) => out.push_str(&array_to_string(nested)?),
                    _ => return None,
                }
            }
            Some(out)
        }

        let mut last = Value::Null;
        for a in args {
            last = self.eval(a)?;
            if matches!(last, Value::Null) {
                return Ok(Value::Number(0.0));
            }
            match number_of(&last) {
                Some(n) if !n.is_nan() => return Ok(Value::Number(n)),
                // `isNaN(num) → continue`.
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
        // MapLibre keeps the error from the *last* argument only (`error` is
        // reset each iteration), so the message comes from `last`.
        Err(color_coercion_error(&last))
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

/// A JSON string literal, with the escaping JSON actually specifies (``,
/// not Rust's `\u{7}`).
fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// A number as `JSON.stringify` writes it: the same rendering as `String(n)`,
/// except that the non-finite values become `null` rather than `NaN` /
/// `Infinity`, which are not JSON.
fn json_number(n: f64) -> String {
    if n.is_finite() {
        crate::value::format_number(n)
    } else {
        "null".to_string()
    }
}

/// A `JSON.stringify`-equivalent rendering of a value (strings quoted, arrays
/// and objects compact), used verbatim in several MapLibre error messages.
fn json_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => json_number(*n),
        Value::String(s) => json_quote(s),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{}:{}", json_quote(k), json_stringify(val)))
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
        // The implicit coercion is the same `Coercion` expression as an
        // explicit `to-color`, so it raises the same errors.
        Type::Color => match coerce_color(&v) {
            Some(c) => Ok(Value::Color(c)),
            None => Err(color_coercion_error(&v)),
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

/// `String.prototype.indexOf`, as MapLibre's `IndexOf` calls it
/// (`src/expression/definitions/index_of.ts:72-81`): the search itself — and
/// therefore `from` — is in UTF-16 code units, and only the *result* is
/// converted to code points, by `[...haystack.slice(0, rawIndex)].length`.
///
/// So `["index-of", "a", "𝐀ab", 2]` is `1`: the match is at UTF-16 index 2,
/// which is code point 1. Treating `from` as a code point index instead would
/// start the search past the `"a"` and wrongly answer `-1`.
///
/// A negative `from` clamps to 0.
fn str_index_of(hay: &str, needle: &str, from: Option<f64>) -> f64 {
    let hay: Vec<u16> = hay.encode_utf16().collect();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    // Code points in `hay[..units]`. A well-formed UTF-16 sequence has exactly
    // one low surrogate per astral code point, so subtracting them counts code
    // points; a `from` landing between a pair leaves its high surrogate
    // counted, which is what spreading `haystack.slice(0, rawIndex)` does too.
    let code_points = |units: usize| {
        hay[..units]
            .iter()
            .filter(|u| !(0xDC00..0xE000).contains(*u))
            .count() as f64
    };
    let start = from.map_or(0, |f| {
        if f < 0.0 || f.is_nan() {
            0
        } else {
            (f as usize).min(hay.len())
        }
    });
    if needle.is_empty() {
        return code_points(start.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return -1.0;
    }
    for i in start..=hay.len() - needle.len() {
        if hay[i..i + needle.len()] == needle[..] {
            return code_points(i);
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
/// read `[r, g, b]` / `[r, g, b, a]` numeric arrays (channels in `0..=255`,
/// alpha in `0..=1`).
///
/// Arrays go through the same `validateRGBA` check as `["rgb", …]`
/// (`src/expression/values.ts:31-51`), which MapLibre's `Coercion` applies
/// before building the `Color` (`src/expression/definitions/coercion.ts:76-89`).
fn coerce_color(v: &Value) -> Option<Color> {
    match v {
        Value::Color(c) => Some(*c),
        Value::String(s) => Color::parse(s),
        Value::Array(a) if a.len() == 3 || a.len() == 4 => {
            if rgba_array_error(a).is_some() {
                return None;
            }
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

/// MapLibre's `validateRGBA` (`src/expression/values.ts:31-51`) applied to an
/// `[r, g, b]` / `[r, g, b, a]` array: the rendered value and the clause that
/// follows it, or `None` when the array is a valid colour.
///
/// The value is rendered with JavaScript's `Array.prototype.join(', ')`, i.e.
/// each element through `String(x)` and *not* through `JSON.stringify` — so a
/// bad string channel reads `[a, b, c]`, unquoted.
fn rgba_array_error(a: &[Value]) -> Option<(String, &'static str)> {
    let n = |i: usize| a.get(i).and_then(Value::as_number);
    let render = |with_alpha: bool| {
        let end = if with_alpha { 4 } else { 3 };
        let parts: Vec<String> = a.iter().take(end).map(js_string).collect();
        format!("[{}]", parts.join(", "))
    };
    // `(0.0..=255.0).contains` is false for NaN, matching `r >= 0 && r <= 255`.
    let in_range = |x: Option<f64>, hi: f64| x.is_some_and(|x| (0.0..=hi).contains(&x));
    if !(in_range(n(0), 255.0) && in_range(n(1), 255.0) && in_range(n(2), 255.0)) {
        // `a` is only part of the message when it is a number, mirroring
        // `typeof a === 'number' ? [r, g, b, a] : [r, g, b]`.
        let with_alpha = n(3).is_some();
        return Some((
            render(with_alpha),
            "'r', 'g', and 'b' must be between 0 and 255.",
        ));
    }
    // Alpha is optional; when present it must be a number in `0..=1`.
    if a.len() > 3 && !in_range(n(3), 1.0) {
        return Some((render(true), "'a' must be between 0 and 1."));
    }
    None
}

/// The runtime error MapLibre's `Coercion` throws for a value that cannot
/// become a colour: the `validateRGBA` message for an `[r, g, b(, a)]`-shaped
/// array, the length message for any other array, and the generic
/// "Could not parse color" otherwise (`coercion.ts:76-95`).
fn color_coercion_error(v: &Value) -> EvalError {
    match v {
        Value::Array(a) if a.len() == 3 || a.len() == 4 => {
            let (value, reason) = rgba_array_error(a)
                .expect("coerce_color only fails on a 3/4-element array when validateRGBA does");
            EvalError::of(EvalErrorKind::InvalidRgba { value, reason })
        }
        Value::Array(_) => EvalError::of(EvalErrorKind::InvalidRgba {
            value: json_stringify(v),
            reason: "expected an array containing either three or four numeric values.",
        }),
        other => EvalError::of(EvalErrorKind::CouldNotParse {
            ty: "color",
            value: coercion_value_repr(other),
        }),
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

/// Compact JSON serialization for `to-string`/`concat` of arrays and objects —
/// the `JSON.stringify` branch of MapLibre's `valueToString`
/// (`src/expression/values.ts:160-179`).
///
/// Strings are escaped as JSON, not with Rust's `{:?}`: the two agree on `\n`
/// and `"` but not on control characters, where `{:?}` writes Rust syntax
/// (`\u{7}`) instead of JSON's ``.
fn json_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => json_number(*n),
        Value::String(s) => json_quote(s),
        Value::Color(c) => json_quote(&c.to_string()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(json_string).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{}:{}", json_quote(k), json_string(val)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Image { name, available } => {
            format!(
                "{{\"name\":{},\"available\":{available}}}",
                json_quote(name)
            )
        }
        Value::Formatted(sections) => {
            let s: String = sections.iter().map(|s| s.text.clone()).collect();
            json_quote(&s)
        }
        Value::NumberArray(_) | Value::Padding(_) => {
            let nums = match v {
                Value::NumberArray(a) => a.clone(),
                Value::Padding(a) => a.to_vec(),
                _ => unreachable!(),
            };
            let parts: Vec<String> = nums.iter().map(|n| json_number(*n)).collect();
            format!("{{\"values\":[{}]}}", parts.join(","))
        }
        Value::ColorArray(_) | Value::Projection(_) | Value::Collator { .. } => {
            json_quote(&v.to_string())
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

/// Resolve one fraction-digit option the way ECMA-402
/// `DefaultNumberOption(value, 0, 100, undefined)` (ECMA-402 §9.2.13) does:
/// a value that is not finite, or that falls outside `[0, 100]`, is a
/// `RangeError`; otherwise it is floored.
///
/// Measured against node v25.2.1:
/// `new Intl.NumberFormat('en-US', {minimumFractionDigits: -1})` →
/// `RangeError: minimumFractionDigits value is out of range.`, and
/// `{maximumFractionDigits: 1.7}` formats `1.55` as `"1.6"` (floored to 1).
fn fraction_digit_option(option: &'static str, v: f64) -> Result<usize> {
    if !v.is_finite() || v < 0.0 || v > 100.0 {
        return Err(EvalError::of(EvalErrorKind::NumberFormatDigits {
            option,
            value: v,
        }));
    }
    Ok(v.floor() as usize)
}

/// Format a number the way `Intl.NumberFormat('en-US', ...)` does.
///
/// Only the options `number-format` itself exposes are handled (currency, unit
/// and the fraction-digit pair); notation, rounding modes and significant
/// digits are not reachable from the expression language. Within that surface
/// the digit resolution follows ECMA-402 §16.1.2 `SetNumberFormatDigitOptions`
/// rather than approximating it, and the results below were checked against
/// node v25.2.1.
///
/// The one deliberate narrowing is the locale: upstream passes the style's
/// `locale` option through to `Intl.NumberFormat` (`number_format.ts:105`),
/// while this port always formats in `en-US`.
fn format_number_intl(
    n: f64,
    currency: Option<&str>,
    unit: Option<&str>,
    min_frac: Option<usize>,
    max_frac: Option<usize>,
) -> Result<String> {
    let (def_min, def_max) = match currency {
        Some(code) => {
            let d = currency_digits(code);
            (d, d)
        }
        None => (0, 3),
    };
    // ECMA-402 §16.1.2 SetNumberFormatDigitOptions: "If mnfd is undefined, set
    // mnfd to min(mnfdDefault, mxfd). Else if mxfd is undefined, set mxfd to
    // max(mxfdDefault, mnfd). Else if mnfd is greater than mxfd, throw a
    // RangeError exception." The default is only ever relaxed towards the
    // option the caller gave; two explicit options are never reconciled.
    let (min, max) = match (min_frac, max_frac) {
        (None, None) => (def_min, def_max),
        (None, Some(mx)) => (def_min.min(mx), mx),
        (Some(mn), None) => (mn, def_max.max(mn)),
        (Some(mn), Some(mx)) => {
            if mn > mx {
                return Err(EvalError::of(EvalErrorKind::NumberFormatDigits {
                    option: "maximumFractionDigits",
                    value: mx as f64,
                }));
            }
            (mn, mx)
        }
    };
    // ECMA-402 §16.5.4 PartitionNumberPattern formats a NaN or infinite value
    // as an implementation- and locale-dependent string *before* any digit
    // option applies, and §16.5.5 emits it as a single "nan" / "infinity" part.
    // For en-US those strings are "NaN" and "∞" (node v25.2.1:
    // `Intl.NumberFormat('en-US').format(1/0)` → "∞", `format(-1/0)` → "-∞",
    // `format(0/0)` → "NaN").
    let negative = n.is_sign_negative() && !n.is_nan();
    let body = if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        "∞".to_string()
    } else {
        format_decimal_us(n.abs(), min, max)
    };
    let sign = if negative { "-" } else { "" };
    // The currency symbol sits inside the sign, not outside it: node v25.2.1
    // formats -1 USD as "-$1.00" and -Infinity USD as "-$∞".
    if let Some(code) = currency {
        return Ok(format!("{sign}{}{body}", currency_symbol(code)));
    }
    if let Some(u) = unit {
        return Ok(format!("{sign}{body}{}", unit_suffix(u)));
    }
    Ok(format!("{sign}{body}"))
}

/// Format the magnitude `n` (assumed finite and non-negative) with `min`..`max`
/// fraction digits and en-US thousands grouping. The sign is the caller's job.
fn format_decimal_us(n: f64, min: usize, max: usize) -> String {
    // The shortest round-trip decimal, matching JavaScript's Number->String.
    let s = format!("{n}");
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
///
/// Upstream builds the collator from the two booleans exactly as below, then
/// asks Intl for `usage: 'search'`:
///
/// ```js
/// if (caseSensitive) this.sensitivity = diacriticSensitive ? 'variant' : 'case';
/// else this.sensitivity = diacriticSensitive ? 'accent' : 'base';
/// this.collator = new Intl.Collator(this.locale ? this.locale : [],
///                                   {sensitivity: this.sensitivity, usage: 'search'});
/// ```
///
/// (`src/expression/types/collator.ts:6-15` at the pinned upstream commit.)
/// ECMA-402 §10.3.3.2 Table 4 fixes what each sensitivity distinguishes
/// ("a" vs. "á" / "a" vs. "A"): base = equal/equal, accent = unequal/equal,
/// case = equal/unequal, variant = unequal/unequal. `icu_collator` documents
/// the same four values as the `Strength` + `CaseLevel` pairs used below —
/// base → `Primary`, accent → `Secondary`, case → `Primary` + `CaseLevel::On`,
/// variant → `Tertiary` (icu_collator 2.3.1, `src/options.rs`, "ECMA-402
/// Sensitivity"), so the match arms are that table read through the two
/// booleans.
///
/// `usage: 'search'` has no counterpart here: ICU4X spells it `-u-co-search`
/// and "ICU4X data does not include search collations by default"
/// (icu_collator 2.3.1, `src/options.rs`, "ECMA-402 Usage"), which the
/// `compiled_data` feature this crate links is. The German rewrite below
/// substitutes for it; see the comment there.
#[cfg(feature = "collator")]
fn collator_compare(collator: &Value, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use icu_collator::{
        options::{CaseLevel, CollatorOptions, Strength},
        Collator, CollatorPreferences,
    };
    use icu_locale_core::Locale;

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
            // Stand-in for the `usage: 'search'` collation ICU4X's compiled
            // data omits. German search collation expands the umlauts
            // (ü ≈ ue, ö ≈ oe, ä ≈ ae) the way the phonebook collation does,
            // and the standard German collation this crate would otherwise get
            // does not — which the `collator/accent-equals-de` fixture pins
            // ("ü" == "ue" is expected `true`, "ü" == "u" `false`).
            //
            // Measured rather than assumed: over 21 German string pairs × the
            // four sensitivities, `de-u-co-phonebk` here and
            // `new Intl.Collator('de', {sensitivity, usage: 'search'})` on node
            // v25.2.1 agree on all 84 comparisons, including the pairs that
            // separate the two collations (e.g. "ö" vs. "od", where search and
            // phonebook both order "ö" after and standard German before).
            // Upstream has no such rewrite; it gets the behaviour from
            // `usage: 'search'`.
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

/// Fallback used when the `collator` feature is off: no CLDR data is linked, so
/// the locale and the sensitivity flags are ignored and operands are compared
/// in code-point order. Parsing and type-checking are unaffected — a `collator`
/// expression is still accepted, it just does not tailor the ordering.
#[cfg(not(feature = "collator"))]
fn collator_compare(collator: &Value, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let Value::Collator { .. } = collator else {
        return None;
    };
    Some(to_string_value(a).cmp(&to_string_value(b)))
}

#[cfg(test)]
mod tests {
    use crate::{evaluate, parse, typecheck, EvaluationContext, Feature, Value};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// A control character that JSON escapes as `` but Rust's `{:?}`
    /// writes as `\u{7}`.
    const BEL: char = '\u{7}';

    /// Evaluate an expression against a feature with `props`, returning the
    /// value or the rendered error message.
    ///
    /// Type-checking constant-folds, so an expression over literals raises its
    /// runtime error there instead; the message is the same either way, and
    /// both are reported here as `Err`.
    fn run(expr: serde_json::Value, props: &[(&str, Value)]) -> Result<Value, String> {
        let parsed = parse(&expr).expect("parses");
        let checked = typecheck(&parsed, None, false).map_err(|e| e.to_string())?;
        let properties: BTreeMap<String, Value> = props
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        let ctx = EvaluationContext::new().with_feature(Feature {
            properties,
            ..Feature::default()
        });
        evaluate(&checked, &ctx).map_err(|e| e.to_string())
    }

    fn eval_str(expr: serde_json::Value) -> String {
        match run(expr, &[]) {
            Ok(Value::String(s)) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn eval_err(expr: serde_json::Value, props: &[(&str, Value)]) -> String {
        match run(expr, props) {
            Err(e) => e,
            Ok(v) => panic!("expected an error, got {v:?}"),
        }
    }

    /// B2: `to-string` is the user-visible face of `format_number`, and the
    /// window `[2^63, 1e21)` used to saturate at `i64::MAX`.
    #[test]
    fn to_string_of_large_and_small_numbers_matches_javascript() {
        let cases: &[(f64, &str)] = &[
            (1e18, "1000000000000000000"),
            (1e19, "10000000000000000000"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (-0.0, "0"),
        ];
        for (n, expected) in cases {
            assert_eq!(
                eval_str(json!(["to-string", n])),
                *expected,
                "to-string {n}"
            );
        }
        // Non-finite results have to be produced, not written as literals.
        assert_eq!(eval_str(json!(["to-string", ["/", 1, 0]])), "Infinity");
        assert_eq!(eval_str(json!(["to-string", ["/", -1, 0]])), "-Infinity");
        assert_eq!(
            eval_str(json!(["to-string", ["-", ["/", 1, 0], ["/", 1, 0]]])),
            "NaN"
        );
        // `concat` goes through the same renderer.
        assert_eq!(
            eval_str(json!(["concat", 1e20, "!"])),
            "100000000000000000000!"
        );
    }

    /// B2 must not disturb `number-format`, which formats through its own
    /// US-decimal grouping path rather than `format_number`.
    #[test]
    fn number_format_still_groups_large_integers() {
        assert_eq!(
            eval_str(json!(["number-format", 1e20, {}])),
            "100,000,000,000,000,000,000"
        );
    }

    /// `number-format` of a non-finite number. ECMA-402 §16.5.4 formats NaN
    /// and ±∞ as locale strings before any digit option applies; the expected
    /// values here are node v25.2.1's
    /// `new Intl.NumberFormat('en-US', opts).format(v)`.
    #[test]
    fn number_format_of_non_finite_numbers_matches_intl() {
        let inf = json!(["/", 1.0, 0.0]);
        let neg_inf = json!(["/", -1.0, 0.0]);
        let nan = json!(["/", 0.0, 0.0]);
        assert_eq!(eval_str(json!(["number-format", inf, {}])), "∞");
        assert_eq!(eval_str(json!(["number-format", neg_inf, {}])), "-∞");
        assert_eq!(eval_str(json!(["number-format", nan, {}])), "NaN");
        // Digit options do not turn into fraction digits on a non-finite value.
        assert_eq!(
            eval_str(json!(["number-format", inf, {"min-fraction-digits": 2}])),
            "∞"
        );
        // …and the currency symbol sits inside the sign, not outside it.
        assert_eq!(
            eval_str(json!(["number-format", inf, {"currency": "USD"}])),
            "$∞"
        );
        assert_eq!(
            eval_str(json!(["number-format", neg_inf, {"currency": "USD"}])),
            "-$∞"
        );
        assert_eq!(
            eval_str(json!(["number-format", neg_inf, {"unit": "kilometer"}])),
            "-∞ km"
        );
        // Negative zero keeps its sign, as `format(-0)` does.
        assert_eq!(
            eval_str(json!(["number-format", ["*", -1.0, 0.0], {}])),
            "-0"
        );
    }

    /// The same sign placement for ordinary negative amounts: node v25.2.1
    /// formats -1 as "-$1.00", not "$-1.00".
    #[test]
    fn number_format_puts_the_sign_outside_the_currency_symbol() {
        assert_eq!(
            eval_str(json!(["number-format", -1.0, {"currency": "USD"}])),
            "-$1.00"
        );
        assert_eq!(
            eval_str(json!(["number-format", -1234.5, {"unit": "meter"}])),
            "-1,234.5 m"
        );
    }

    /// ECMA-402 §16.1.2 `SetNumberFormatDigitOptions` reconciles a missing
    /// fraction-digit option with the style's default, but rejects a
    /// `min > max` pair given explicitly rather than clamping it. Upstream
    /// hands both options to `Intl.NumberFormat` and so raises the same
    /// `RangeError`; over a constant expression it surfaces while
    /// constant-folding, as every folded runtime error does.
    #[test]
    fn number_format_rejects_min_fraction_digits_above_max() {
        assert_eq!(
            eval_err(
                json!(["number-format", 1.23456, {"min-fraction-digits": 4, "max-fraction-digits": 1}]),
                &[]
            ),
            "maximumFractionDigits value is out of range."
        );
        // Also on the evaluation path, where the options come from the feature
        // and nothing was folded. (Upstream, both spellings were run against
        // the pinned commit: the constant one is a compile error carrying that
        // message, the `["get", …]` one throws the same `RangeError` from
        // `evaluateWithoutErrorHandling`.)
        assert_eq!(
            eval_err(
                json!(["number-format", 1.23456, {"min-fraction-digits": ["get", "min"], "max-fraction-digits": ["get", "max"]}]),
                &[("min", Value::Number(4.0)), ("max", Value::Number(1.0))]
            ),
            "maximumFractionDigits value is out of range."
        );
        // A default is still relaxed towards an explicit option, in both
        // directions: node v25.2.1 formats 1.5 USD as "$1.5" with
        // maximumFractionDigits 1 (the currency default of 2 lowered to 1) and
        // as "$1.500" with minimumFractionDigits 3 (raised to 3).
        assert_eq!(
            eval_str(json!(["number-format", 1.5, {"currency": "USD", "max-fraction-digits": 1}])),
            "$1.5"
        );
        assert_eq!(
            eval_str(json!(["number-format", 1.5, {"currency": "USD", "min-fraction-digits": 3}])),
            "$1.500"
        );
    }

    /// ECMA-402 §9.2.13 `DefaultNumberOption(value, 0, 100, undefined)`: a
    /// fraction-digit option outside `[0, 100]`, or not finite, is a
    /// `RangeError`. Before this was enforced, `min-fraction-digits` doubled as
    /// an allocation knob — 1e6 produced a megabyte of zeros.
    #[test]
    fn number_format_rejects_out_of_range_fraction_digits() {
        let cases: &[(serde_json::Value, &str)] = &[
            (
                json!(["number-format", 1.0, {"min-fraction-digits": -1}]),
                "minimumFractionDigits value is out of range.",
            ),
            (
                json!(["number-format", 1.0, {"max-fraction-digits": 101}]),
                "maximumFractionDigits value is out of range.",
            ),
            (
                json!(["number-format", 1.0, {"min-fraction-digits": 1000000}]),
                "minimumFractionDigits value is out of range.",
            ),
            (
                json!(["number-format", 1.0, {"max-fraction-digits": ["/", 1.0, 0.0]}]),
                "maximumFractionDigits value is out of range.",
            ),
        ];
        for (expr, message) in cases {
            assert_eq!(&eval_err(expr.clone(), &[]), message, "for {expr}");
        }
        // The boundary itself is accepted, and a fractional option floors.
        assert_eq!(
            eval_str(json!(["number-format", 1.0, {"max-fraction-digits": 100}])),
            "1"
        );
        assert_eq!(
            eval_str(json!(["number-format", 1.55, {"max-fraction-digits": 1.7}])),
            "1.6"
        );
    }

    /// `is-supported-script` reports every script as supported, because
    /// upstream's `ctx.globals.isSupportedScript` hook is absent outside a
    /// renderer with the RTL-text plugin loaded
    /// (`compound_expression.ts:500-511`).
    #[test]
    fn is_supported_script_is_true_for_every_script() {
        for s in ["Hello", "مرحبا", "こんにちは", "दिल्ली", ""] {
            assert_eq!(
                run(json!(["is-supported-script", s]), &[]),
                Ok(Value::Bool(true)),
                "for {s:?}"
            );
        }
    }

    /// The ECMA-402 sensitivity table (§10.3.3.2, Table 4) read through the two
    /// collator booleans, checked against node v25.2.1's
    /// `new Intl.Collator('en', {sensitivity, usage: 'search'})`.
    #[cfg(feature = "collator")]
    #[test]
    fn collator_sensitivity_matches_the_ecma402_table() {
        // (case-sensitive, diacritic-sensitive) => ("a" vs "á", "a" vs "A")
        let cases: &[(bool, bool, bool, bool)] = &[
            (false, false, true, true), // base:    equal,   equal
            (false, true, false, true), // accent:  unequal, equal
            (true, false, true, false), // case:    equal,   unequal
            (true, true, false, false), // variant: unequal, unequal
        ];
        for &(cs, ds, accent_equal, case_equal) in cases {
            let coll = json!(["collator", {"case-sensitive": cs, "diacritic-sensitive": ds, "locale": "en"}]);
            let eq = |a: &str, b: &str| {
                run(json!(["==", a, b, coll.clone()]), &[]) == Ok(Value::Bool(true))
            };
            assert_eq!(eq("a", "á"), accent_equal, "a/á for ({cs}, {ds})");
            assert_eq!(eq("a", "A"), case_equal, "a/A for ({cs}, {ds})");
        }
    }

    /// German comparisons stand in for `usage: 'search'`, which ICU4X's
    /// compiled data omits. Expected values are node v25.2.1's
    /// `new Intl.Collator('de', {sensitivity: 'case', usage: 'search'})`.
    #[cfg(feature = "collator")]
    #[test]
    fn collator_de_expands_umlauts_like_the_search_collation() {
        let coll = json!(["collator", {"case-sensitive": true, "diacritic-sensitive": false, "locale": "de"}]);
        let cmp = |a: &str, b: &str| {
            if run(json!(["<", a, b, coll.clone()]), &[]) == Ok(Value::Bool(true)) {
                -1
            } else if run(json!([">", a, b, coll.clone()]), &[]) == Ok(Value::Bool(true)) {
                1
            } else {
                0
            }
        };
        assert_eq!(cmp("ü", "ue"), 0);
        assert_eq!(cmp("ö", "oe"), 0);
        assert_eq!(cmp("ä", "ae"), 0);
        assert_eq!(cmp("ü", "u"), 1);
        // The pair that separates the search/phonebook expansion from the
        // standard German collation, where "ö" would sort before "od".
        assert_eq!(cmp("ö", "od"), 1);
        assert_eq!(cmp("Müller", "Mueller"), 0);
    }

    /// B9: `to-string` of an array is JSON, so a control character comes out
    /// as a JSON escape, not as Rust's `{:?}` syntax.
    #[test]
    fn to_string_of_array_is_json_not_rust_debug() {
        let rendered = eval_str(json!(["to-string", ["literal", [format!("a{BEL}b")]]]));
        // The rendering has to *be* JSON: it must parse back to the input.
        let back: serde_json::Value =
            serde_json::from_str(&rendered).expect("to-string of an array is JSON");
        assert_eq!(back, json!([format!("a{BEL}b")]));
        // Specifically, not Rust's `{:?}`, which writes a `\u{N}` escape.
        assert!(
            !rendered.contains("u{"),
            "Rust-syntax escape in {rendered:?}"
        );
        assert!(
            !rendered.contains(BEL),
            "unescaped control char in {rendered:?}"
        );

        let rendered = eval_str(json!(["to-string", ["literal", {format!("k{BEL}"): 1}]]));
        let back: serde_json::Value =
            serde_json::from_str(&rendered).expect("to-string of an object is JSON");
        assert_eq!(back, json!({format!("k{BEL}"): 1}));
        assert!(
            !rendered.contains("u{"),
            "Rust-syntax escape in {rendered:?}"
        );

        // The cases that already agreed must keep agreeing, byte for byte.
        assert_eq!(
            eval_str(json!(["to-string", ["literal", ["a\nb\"c"]]])),
            r#"["a\nb\"c"]"#
        );
    }

    /// B9, second half: `JSON.stringify` writes `null` for the non-finite
    /// numbers, which `String(n)` does not. Reachable through the `to-color`
    /// error message, whose operand is rendered with `JSON.stringify`.
    #[test]
    fn json_stringify_renders_non_finite_numbers_as_null() {
        let nan = json!(["-", ["/", 1, 0], ["/", 1, 0]]);
        assert_eq!(
            eval_err(json!(["to-color", nan]), &[]),
            "Could not parse color from value 'null'"
        );
        assert_eq!(
            eval_err(json!(["to-color", ["/", 1, 0]]), &[]),
            "Could not parse color from value 'null'"
        );
        // A non-finite number *inside* an array operand too.
        assert_eq!(
            eval_err(json!(["to-color", ["literal", [1, 2, 3, 4, 5]]]), &[]),
            "Invalid rgba value [1,2,3,4,5]: expected an array containing either three or four numeric values."
        );
    }

    /// B4: `to-color` validates the RGBA range the way `["rgb", …]` does.
    #[test]
    fn to_color_validates_rgba_range() {
        assert_eq!(
            eval_err(json!(["to-color", ["literal", [300, 0, 0]]]), &[]),
            "Invalid rgba value [300, 0, 0]: 'r', 'g', and 'b' must be between 0 and 255."
        );
        assert_eq!(
            eval_err(json!(["to-color", ["literal", [-1, 0, 0]]]), &[]),
            "Invalid rgba value [-1, 0, 0]: 'r', 'g', and 'b' must be between 0 and 255."
        );
        assert_eq!(
            eval_err(json!(["to-color", ["literal", [0, 0, 0, 5]]]), &[]),
            "Invalid rgba value [0, 0, 0, 5]: 'a' must be between 0 and 1."
        );
        // A non-numeric channel gets the range message too, with the elements
        // rendered by `String(x)` (unquoted), not `JSON.stringify`.
        assert_eq!(
            eval_err(json!(["to-color", ["literal", ["a", "b", "c"]]]), &[]),
            "Invalid rgba value [a, b, c]: 'r', 'g', and 'b' must be between 0 and 255."
        );
        // Only a wrong *length* keeps the length message.
        assert_eq!(
            eval_err(json!(["to-color", ["literal", [1, 2]]]), &[]),
            "Invalid rgba value [1,2]: expected an array containing either three or four numeric values."
        );
        // In range still works.
        assert!(matches!(
            run(json!(["to-color", ["literal", [255, 0, 0]]]), &[]),
            Ok(Value::Color(_))
        ));
    }

    /// B4: the implicit `Coerce(Color)` is the same expression upstream, so it
    /// raises the same error.
    #[test]
    fn implicit_color_coercion_validates_rgba_range() {
        let parsed = parse(&json!(["get", "c"])).unwrap();
        let checked = typecheck(&parsed, Some(&crate::Type::Color), false).unwrap();
        let mut properties = BTreeMap::new();
        properties.insert(
            "c".to_string(),
            Value::Array(vec![
                Value::Number(300.0),
                Value::Number(0.0),
                Value::Number(0.0),
            ]),
        );
        let ctx = EvaluationContext::new().with_feature(Feature {
            properties,
            ..Feature::default()
        });
        assert_eq!(
            evaluate(&checked, &ctx).unwrap_err().to_string(),
            "Invalid rgba value [300, 0, 0]: 'r', 'g', and 'b' must be between 0 and 255."
        );
    }

    /// C13: `from` is a UTF-16 index, only the result is in code points.
    #[test]
    fn index_of_from_is_a_utf16_index() {
        // "𝐀" is one code point but two UTF-16 units, so `from: 2` still finds
        // the "a" that follows it — at code point 1.
        assert_eq!(
            run(json!(["index-of", "a", "𝐀ab", 2]), &[]),
            Ok(Value::Number(1.0))
        );
        assert_eq!(
            run(json!(["index-of", "b", "𝐀ab", 2]), &[]),
            Ok(Value::Number(2.0))
        );
        assert_eq!(
            run(json!(["index-of", "a", "𝐀ab", 3]), &[]),
            Ok(Value::Number(-1.0))
        );
        // Results stay in code points with no `from`, as before.
        assert_eq!(
            run(json!(["index-of", "a", "𝐀ab"]), &[]),
            Ok(Value::Number(1.0))
        );
        assert_eq!(
            run(json!(["length", "丐𦨭市镇"]), &[]),
            Ok(Value::Number(4.0))
        );
        // Negative `from` clamps to 0; BMP-only strings are unaffected.
        assert_eq!(
            run(json!(["index-of", "b", "abcb", -5]), &[]),
            Ok(Value::Number(1.0))
        );
        assert_eq!(
            run(json!(["index-of", "b", "abcb", 2]), &[]),
            Ok(Value::Number(3.0))
        );
    }

    /// C14 + C15: an ordered comparison type-checks its operands at runtime
    /// before the collator is consulted, and the error names bare type kinds.
    #[test]
    fn ordered_comparison_checks_types_before_the_collator() {
        let arrays: &[(&str, Value)] = &[
            ("a", Value::Array(vec![Value::Number(1.0)])),
            ("b", Value::Array(vec![Value::Number(2.0)])),
        ];
        let expected = "Expected arguments for \"<\" to be (string, string) or (number, number), but found (array, array) instead.";
        // Without a collator — and, since C14, with one too.
        assert_eq!(
            eval_err(json!(["<", ["get", "a"], ["get", "b"]]), arrays),
            expected
        );
        assert_eq!(
            eval_err(
                json!(["<", ["get", "a"], ["get", "b"], ["collator", {}]]),
                arrays
            ),
            expected
        );
        // Missing properties are `null`; non-array operands were already right.
        assert_eq!(
            eval_err(json!(["<", ["get", "a"], ["get", "b"]]), &[]),
            "Expected arguments for \"<\" to be (string, string) or (number, number), but found (null, null) instead."
        );
        // A collator over two runtime strings still compares with the collator.
        let strings: &[(&str, Value)] = &[
            ("a", Value::String("a".to_string())),
            ("b", Value::String("b".to_string())),
        ];
        assert_eq!(
            run(
                json!(["<", ["get", "a"], ["get", "b"], ["collator", {}]]),
                strings
            ),
            Ok(Value::Bool(true))
        );
        // …and `==` still only uses the collator for two runtime strings.
        assert_eq!(
            run(
                json!(["==", ["get", "a"], ["get", "b"], ["collator", {}]]),
                arrays
            ),
            Ok(Value::Bool(false))
        );
    }
}
