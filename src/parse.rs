//! Turning raw JSON (`serde_json::Value`) into an [`Expr`] tree.

use std::cell::Cell;

use serde_json::Value as Json;

use crate::ast::{Expr, FormatArg, InterpKind, InterpSpace};
use crate::distance::SimpleGeom;
use crate::error::{ParseError, ParseErrorKind};
use crate::ext::{Options, MAX_MACRO_DEPTH};
use crate::value::Value;

/// Valid `vertical-align` option values for the `format` operator.
const VERTICAL_ALIGN: [&str; 3] = ["bottom", "center", "top"];

/// Maximum nesting depth of the JSON expression tree accepted by the parser.
///
/// Parsing is recursive descent over the native stack, so deep enough input
/// would overflow it (an abort, not an error). The bound is set from measured
/// frame cost: a level costs roughly 10 KiB in a debug build and 1–2 KiB in a
/// release build, so a debug parse on Rust's default 2 MiB spawned-thread stack
/// runs out somewhere between 150 and 200 levels. 100 keeps a comfortable
/// margin there while staying far above any realistic style — MapLibre's own
/// expression fixtures nest only a handful of levels deep.
///
/// Unrelated to the macro-expansion bound ([`MAX_MACRO_DEPTH`]), and with no
/// counterpart upstream: MapLibre's `ParsingContext` imposes no depth limit.
pub(crate) const MAX_NEST_DEPTH: usize = 100;

type Result<T> = std::result::Result<T, ParseError>;

/// Per-parse state: the (shared, immutable) extension registry plus the depth
/// counters of the parse currently in progress.
///
/// The counters live here — one `Ctx` per top-level [`parse`] call — and never
/// on [`Options`], so the same `&Options` can be parsed against concurrently
/// from several threads without one parse's depth leaking into another's.
struct Ctx<'a> {
    opts: &'a Options,
    /// Current JSON nesting depth (see [`MAX_NEST_DEPTH`]).
    nest: Cell<usize>,
    /// Current macro-expansion depth (see [`MAX_MACRO_DEPTH`]).
    macro_depth: Cell<usize>,
}

/// Restores a depth counter when the parse of a nested level returns, by any
/// path (including `?`).
struct DepthGuard<'a>(&'a Cell<usize>);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl<'a> Ctx<'a> {
    fn new(opts: &'a Options) -> Ctx<'a> {
        Ctx {
            opts,
            nest: Cell::new(0),
            macro_depth: Cell::new(0),
        }
    }

    /// Enter one more level of JSON nesting, or error if that would exceed
    /// [`MAX_NEST_DEPTH`].
    fn enter_nest(&self) -> Result<DepthGuard<'_>> {
        if self.nest.get() >= MAX_NEST_DEPTH {
            return Err(ParseError::of(ParseErrorKind::NestingTooDeep {
                max: MAX_NEST_DEPTH,
            }));
        }
        self.nest.set(self.nest.get() + 1);
        Ok(DepthGuard(&self.nest))
    }

    /// Enter one more level of macro expansion of `op`, or error if that would
    /// exceed [`MAX_MACRO_DEPTH`].
    fn enter_macro(&self, op: &str) -> Result<DepthGuard<'_>> {
        if self.macro_depth.get() >= MAX_MACRO_DEPTH {
            return Err(ParseError::of(ParseErrorKind::MacroDepth {
                op: op.to_string(),
            }));
        }
        self.macro_depth.set(self.macro_depth.get() + 1);
        Ok(DepthGuard(&self.macro_depth))
    }
}

/// Parse a MapLibre expression from JSON.
pub(crate) fn parse(json: &Json, opts: &Options) -> Result<Expr> {
    parse_expr(json, &Ctx::new(opts))
}

fn parse_expr(json: &Json, ctx: &Ctx<'_>) -> Result<Expr> {
    match json {
        Json::Array(items) => {
            let _guard = ctx.enter_nest()?;
            parse_array(items, ctx)
        }
        // As in MapLibre's `createExpression`, an object is never an expression
        // here — legacy function objects are converted with their property spec
        // beforehand (`parse_property` / `migrate`), not guessed at spec-less.
        Json::Object(_) => Err(ParseError::of(ParseErrorKind::BareObject)),
        _ => Ok(Expr::Literal(Value::from_json(json))),
    }
}

/// Parse each element of `args` as an expression, tagging errors with the
/// argument's location (its index in the enclosing array).
fn parse_all(args: &[Json], ctx: &Ctx<'_>) -> Result<Vec<Expr>> {
    args.iter()
        .enumerate()
        .map(|(i, a)| parse_expr(a, ctx).map_err(|e| e.at(i + 1)))
        .collect()
}

fn parse_array(items: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    let first = items
        .first()
        .ok_or_else(|| ParseError::of(ParseErrorKind::EmptyArray))?;
    let op = first.as_str().ok_or_else(|| {
        // Reported at the operator slot, position [0].
        ParseError::of(ParseErrorKind::ExpressionNameNotString {
            found: js_typeof(first),
        })
        .at(0)
    })?;
    let args = &items[1..];

    // User macros expand at parse time; expression and external functions
    // become ordinary calls that the evaluator dispatches.
    if ctx.opts.macros.contains_key(op) {
        return expand_macro(op, args, ctx);
    }
    if let Some(f) = ctx.opts.expr_fns.get(op) {
        if args.len() != f.params.len() {
            return Err(ParseError::of(ParseErrorKind::ExtArgCount {
                kind: "Expression function",
                op: op.to_string(),
                expected: f.params.len(),
                found: args.len(),
            }));
        }
        return Ok(Expr::Call {
            op: op.to_string(),
            args: parse_all(args, ctx)?,
        });
    }
    if let Some((arity, _)) = ctx.opts.externals.get(op) {
        // `None` is variadic: any count goes, and the closure sees them all.
        if let Some(expected) = arity.filter(|&n| n != args.len()) {
            return Err(ParseError::of(ParseErrorKind::ExtArgCount {
                kind: "External function",
                op: op.to_string(),
                expected,
                found: args.len(),
            }));
        }
        return Ok(Expr::Call {
            op: op.to_string(),
            args: parse_all(args, ctx)?,
        });
    }

    match op {
        "literal" => {
            expect_one_arg(op, args)?;
            Ok(Expr::Literal(Value::from_json(&args[0])))
        }
        "let" => parse_let(args, ctx),
        "var" => {
            // One guard upstream, not two (`var.ts:18-21`): a wrong count and
            // a non-string name are the same error.
            let name = match args {
                [Json::String(name)] => name,
                _ => return Err(ParseError::of(ParseErrorKind::VarOneStringLiteral)),
            };
            Ok(Expr::Var(name.to_string()))
        }
        "match" => parse_match(args, ctx),
        "step" => parse_step(args, ctx),
        "interpolate" => parse_interpolate(InterpSpace::Rgb, args, ctx),
        "interpolate-hcl" => parse_interpolate(InterpSpace::Hcl, args, ctx),
        "interpolate-lab" => parse_interpolate(InterpSpace::Lab, args, ctx),
        "format" => parse_format(args, ctx),
        "collator" => parse_collator(args, ctx),
        "number-format" => parse_number_format(args, ctx),
        "within" => parse_within(args),
        "distance" => parse_distance(args),
        "global-state" => {
            check_generic_arity(op, args.len())?;
            // The property must be a string *literal*; MapLibre reports the raw
            // argument's JS `typeof` (an array/object/null all read "object").
            if !args[0].is_string() {
                return Err(ParseError::of(ParseErrorKind::GlobalStateProperty {
                    found: js_typeof(&args[0]).to_string(),
                }));
            }
            Ok(Expr::Call {
                op: op.to_string(),
                args: parse_all(args, ctx)?,
            })
        }
        "array" => {
            check_generic_arity(op, args.len())?;
            validate_array_type_args(args)?;
            let parsed = parse_all(args, ctx)?;
            Ok(Expr::Call {
                op: op.to_string(),
                args: parsed,
            })
        }
        _ => {
            if let Some(e) = signature_arity_error(op, args) {
                return Err(e);
            }
            check_generic_arity(op, args.len())?;
            let args = parse_all(args, ctx)?;
            Ok(Expr::Call {
                op: op.to_string(),
                args,
            })
        }
    }
}

/// Expand a macro call into a `let` binding its parameters to the arguments,
/// bounding expansion with a per-parse nesting-depth limit (which a recursive
/// macro reaches, but so can deeply nested non-recursive ones).
fn expand_macro(op: &str, args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    let m = &ctx.opts.macros[op];
    if args.len() != m.params.len() {
        return Err(ParseError::of(ParseErrorKind::ExtArgCount {
            kind: "Macro",
            op: op.to_string(),
            expected: m.params.len(),
            found: args.len(),
        }));
    }
    let _guard = ctx.enter_macro(op)?;
    let arg_exprs = parse_all(args, ctx)?;
    let body = parse_expr(&m.body, ctx)?;
    let bindings = m.params.iter().cloned().zip(arg_exprs).collect();
    Ok(Expr::Let {
        bindings,
        body: Box::new(body),
    })
}

/// JavaScript's `typeof` for a JSON value: arrays, objects and `null` all
/// report as `"object"`.
fn js_typeof(v: &Json) -> &'static str {
    match v {
        Json::Number(_) => "number",
        Json::Bool(_) => "boolean",
        Json::String(_) => "string",
        _ => "object",
    }
}

/// One typed overload of a `CompoundExpression`: its parameter count (`None`
/// for varargs, which accept any count) and the signature as MapLibre's
/// `stringifySignature` renders it.
type Overload = (Option<usize>, &'static str);

/// MapLibre's `CompoundExpression` registry, transcribed from
/// `src/expression/compound_expression.ts` (`CompoundExpression.register`,
/// lines 218–535 of the pinned commit).
///
/// These operators are not special forms: MapLibre resolves them by matching
/// the call against a list of typed overloads, and reports a wrong argument
/// count against *the signatures*, not as a count (see
/// [`signature_arity_error`]). Having the whole registry here — rather than the
/// handful of operators that happened to have fixtures — is what makes that
/// message come out for every one of them, as it does upstream.
///
/// The remaining operators (the "special forms" of
/// `src/expression/definitions/index.ts`) have hand-written parsers and their
/// own messages; their argument counts live in [`arity`].
const COMPOUND_EXPRESSIONS: &[(&str, &[Overload])] = &[
    // --- lookups and feature/global properties -----------------------------
    ("error", &[(Some(1), "(string)")]),
    ("typeof", &[(Some(1), "(value)")]),
    ("to-rgba", &[(Some(1), "(color)")]),
    ("rgb", &[(Some(3), "(number, number, number)")]),
    ("rgba", &[(Some(4), "(number, number, number, number)")]),
    (
        "has",
        &[(Some(1), "(string)"), (Some(2), "(string, object)")],
    ),
    (
        "get",
        &[(Some(1), "(string)"), (Some(2), "(string, object)")],
    ),
    ("feature-state", &[(Some(1), "(string)")]),
    ("properties", &[(Some(0), "()")]),
    ("geometry-type", &[(Some(0), "()")]),
    ("id", &[(Some(0), "()")]),
    ("zoom", &[(Some(0), "()")]),
    ("heatmap-density", &[(Some(0), "()")]),
    ("elevation", &[(Some(0), "()")]),
    ("line-progress", &[(Some(0), "()")]),
    ("accumulated", &[(Some(0), "()")]),
    // --- arithmetic --------------------------------------------------------
    ("+", &[(None, "(number...)")]),
    ("*", &[(None, "(number...)")]),
    ("-", &[(Some(2), "(number, number)"), (Some(1), "(number)")]),
    ("/", &[(Some(2), "(number, number)")]),
    ("%", &[(Some(2), "(number, number)")]),
    ("ln2", &[(Some(0), "()")]),
    ("pi", &[(Some(0), "()")]),
    ("e", &[(Some(0), "()")]),
    ("^", &[(Some(2), "(number, number)")]),
    ("sqrt", &[(Some(1), "(number)")]),
    ("log10", &[(Some(1), "(number)")]),
    ("ln", &[(Some(1), "(number)")]),
    ("log2", &[(Some(1), "(number)")]),
    ("sin", &[(Some(1), "(number)")]),
    ("cos", &[(Some(1), "(number)")]),
    ("tan", &[(Some(1), "(number)")]),
    ("asin", &[(Some(1), "(number)")]),
    ("acos", &[(Some(1), "(number)")]),
    ("atan", &[(Some(1), "(number)")]),
    ("min", &[(None, "(number...)")]),
    ("max", &[(None, "(number...)")]),
    ("abs", &[(Some(1), "(number)")]),
    ("round", &[(Some(1), "(number)")]),
    ("floor", &[(Some(1), "(number)")]),
    ("ceil", &[(Some(1), "(number)")]),
    // --- the internal `filter-*` family MapLibre compiles legacy filters into
    ("filter-==", &[(Some(2), "(string, value)")]),
    ("filter-id-==", &[(Some(1), "(value)")]),
    ("filter-type-==", &[(Some(1), "(string)")]),
    ("filter-<", &[(Some(2), "(string, value)")]),
    ("filter-id-<", &[(Some(1), "(value)")]),
    ("filter->", &[(Some(2), "(string, value)")]),
    ("filter-id->", &[(Some(1), "(value)")]),
    ("filter-<=", &[(Some(2), "(string, value)")]),
    ("filter-id-<=", &[(Some(1), "(value)")]),
    ("filter->=", &[(Some(2), "(string, value)")]),
    ("filter-id->=", &[(Some(1), "(value)")]),
    ("filter-has", &[(Some(1), "(value)")]),
    ("filter-has-id", &[(Some(0), "()")]),
    ("filter-type-in", &[(Some(1), "(array<string>)")]),
    ("filter-id-in", &[(Some(1), "(array)")]),
    ("filter-in-small", &[(Some(2), "(string, array)")]),
    ("filter-in-large", &[(Some(2), "(string, array)")]),
    // --- booleans and strings ----------------------------------------------
    (
        "all",
        &[(Some(2), "(boolean, boolean)"), (None, "(boolean...)")],
    ),
    (
        "any",
        &[(Some(2), "(boolean, boolean)"), (None, "(boolean...)")],
    ),
    ("!", &[(Some(1), "(boolean)")]),
    ("is-supported-script", &[(Some(1), "(string)")]),
    ("upcase", &[(Some(1), "(string)")]),
    ("downcase", &[(Some(1), "(string)")]),
    ("concat", &[(None, "(value...)")]),
    ("split", &[(Some(2), "(string, string)")]),
    ("join", &[(Some(2), "(array<string>, string)")]),
    ("resolved-locale", &[(Some(1), "(collator)")]),
];

/// The typed overloads of `op`, if it is a `CompoundExpression`.
fn compound_overloads(op: &str) -> Option<&'static [Overload]> {
    COMPOUND_EXPRESSIONS
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, overloads)| *overloads)
}

/// A `CompoundExpression` whose argument count matches none of its overloads is
/// reported against the signatures rather than as a count
/// (`compound_expression.ts:154-172`: the count filter empties `overloads`, so
/// `overloads.length === 1` is false and the signature form is printed).
///
/// The argument *types* are the one place this is an approximation. Upstream
/// re-parses each argument and prints `typeToString(parsed.type)`; this crate
/// splits parsing from type inference into two passes (`parse` then
/// `typecheck`), and no type exists yet at the point this error is raised — so
/// types are inferred coarsely, as the raw JSON's JavaScript `typeof`. The two
/// agree for numbers, strings and booleans; they differ for sub-expressions and
/// literal arrays, which read here as `object`.
fn signature_arity_error(op: &str, args: &[Json]) -> Option<ParseError> {
    let overloads = compound_overloads(op)?;
    if overloads
        .iter()
        .any(|(params, _)| params.is_none_or(|n| n == args.len()))
    {
        return None;
    }
    let sigs: Vec<&str> = overloads.iter().map(|(_, sig)| *sig).collect();
    let found: Vec<&str> = args.iter().map(js_typeof).collect();
    Some(ParseError::of(ParseErrorKind::ExpectedArgsOfType {
        sig: sigs.join(" | "),
        found: found.join(", "),
    }))
}

/// Reject unknown operators and calls with the wrong number of arguments at
/// parse time — these are `"result": "error"` cases in the spec fixtures.
///
/// Every name in MapLibre's expression registry is accepted here (so that its
/// arguments parse and error keys line up) even when this crate has no
/// evaluator for it — evaluation reports those as unimplemented. Only names
/// outside the registry are rejected.
fn check_generic_arity(op: &str, argc: usize) -> Result<()> {
    // `case` has an irregular (odd, >= 3) shape (`case.ts:23-28`).
    if op == "case" {
        if argc < 3 {
            return Err(ParseError::of(ParseErrorKind::ExpectedAtLeastArgs {
                min: 3,
                found: argc,
            }));
        }
        if argc.is_multiple_of(2) {
            return Err(ParseError::of(ParseErrorKind::ExpectedOddArgsCase));
        }
        return Ok(());
    }

    let range = arity(op).ok_or_else(|| {
        // The unknown operator name sits at position [0].
        ParseError::of(ParseErrorKind::UnknownExpression(op.to_string())).at(0)
    })?;
    let (min, max) = range;
    if argc < min || max.is_some_and(|m| argc > m) {
        let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
        return Err(ParseError::of(match op {
            // `to-boolean` / `to-string` are fixed single-argument coercions
            // with their own wording (`coercion.ts:46-47`).
            "to-boolean" | "to-string" => ParseErrorKind::ExpectedOneArgument,
            // `image` is strictly two arguments (`image.ts:19-20`).
            "image" => ParseErrorKind::ExpectedTwoArguments,
            // The assertions and the remaining coercions state a floor and
            // nothing else (`assertion.ts:36`, `coercion.ts:41`).
            _ if max.is_none() => ParseErrorKind::ExpectedAtLeastOneArgument,
            _ => ParseErrorKind::WrongArgCount {
                op: op.to_string(),
                expected: match max {
                    Some(m) if m == min => format!("{min} {}", plural(min)),
                    Some(m) if m == min + 1 => format!("{min} or {m} arguments"),
                    Some(m) => format!("{min} to {m} arguments"),
                    None => unreachable!("handled by the arm above"),
                },
                found: argc,
            },
        }));
    }
    Ok(())
}

/// `(min, max)` argument counts for each known operator; `None` max means
/// variadic. Operators absent from this table are unknown names.
///
/// `CompoundExpression`s are not listed: their counts are derived from the
/// typed overloads in [`COMPOUND_EXPRESSIONS`], so the two can never drift
/// apart. What is written out below are the special forms of
/// `src/expression/definitions/index.ts`.
fn arity(op: &str) -> Option<(usize, Option<usize>)> {
    if let Some(overloads) = compound_overloads(op) {
        // A varargs overload accepts any count, so it sets the floor to 0 and
        // removes the ceiling.
        let min = overloads
            .iter()
            .map(|(params, _)| params.unwrap_or(0))
            .min()
            .unwrap_or(0);
        // `Option`'s own `Ord` would sink the varargs overload: `None` sorts
        // *below* every `Some`, so `[Some(2), None].max()` is `Some(2)` and
        // `all`/`any` would cap at two arguments. The varargs case has to win
        // explicitly.
        let max = if overloads.iter().any(|(params, _)| params.is_none()) {
            None
        } else {
            overloads.iter().filter_map(|(params, _)| *params).max()
        };
        return Some((min, max));
    }
    Some(match op {
        // lookups
        "at" => (2, Some(2)),
        "in" => (2, Some(2)),
        "index-of" => (2, Some(3)),
        "slice" => (2, Some(3)),
        "length" => (1, Some(1)),
        "global-state" => (1, Some(1)),

        // decision / boolean
        "coalesce" => (0, None),
        "==" | "!=" | "<" | ">" | "<=" | ">=" => (2, Some(3)),

        // strings & formatting
        "number-format" => (2, Some(2)),
        "format" => (1, None),
        // `image` takes exactly one argument, the image name (`image.ts:19-20`
        // tests `args.length !== 2`, and upstream's `args` includes the
        // operator). It is not a variadic `format`-shaped section list — and
        // note that its message says "two arguments", counting the operator,
        // which is MapLibre's wording and is kept verbatim.
        "image" => (1, Some(1)),

        // type assertions & conversions ("array" takes an optional item type
        // and length prefix, then one or more fallback value candidates)
        "array" => (1, None),
        "boolean" | "number" | "string" | "object" | "to-number" | "to-color" => (1, None),
        "to-boolean" | "to-string" => (1, Some(1)),

        // geometry predicates
        "within" => (1, Some(1)),
        "distance" => (1, Some(1)),

        _ => return None,
    })
}

/// Whether `op` names a built-in expression operator: a special form dispatched
/// by [`parse_array`] before the arity table, or an entry in [`arity`]. Mirrors
/// the operator set the parser recognizes and backs [`crate::is_expression`].
///
/// This is arity-agnostic (a head match only), and considers only built-ins —
/// user macros / expression functions / external functions are `Options`-scoped, not part of the
/// syntactic expression grammar.
pub(crate) fn is_operator(op: &str) -> bool {
    // Special forms recognized outside the `arity` table: the match arms in
    // `parse_array` (`literal`/`let`/`var`/`match`/`step`/`interpolate*`/
    // `collator`) plus `case` (its irregular shape is checked in
    // `check_generic_arity`). The rest — `format`, `number-format`, `within`,
    // `distance`, `global-state`, `array`, … — are covered by `arity` below.
    matches!(
        op,
        "literal"
            | "let"
            | "var"
            | "match"
            | "step"
            | "case"
            | "interpolate"
            | "interpolate-hcl"
            | "interpolate-lab"
            | "collator"
    ) || arity(op).is_some()
}

/// Parse `["let", name, value, …, body]`.
///
/// Upstream (`let.ts:28-49`) gates on a *minimum* count, not on parity: with an
/// even number of arguments the last value doubles as the body, which is
/// accepted. What is rejected is fewer than three arguments — in particular a
/// `let` with no binding at all.
fn parse_let(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    if args.len() < 3 {
        return Err(ParseError::of(ParseErrorKind::LetAtLeast3 {
            found: args.len(),
        }));
    }
    let mut bindings = Vec::new();
    let mut i = 0;
    while i + 1 < args.len() {
        // The binding name is at position [i + 1] in the original array.
        let name = args[i].as_str().ok_or_else(|| {
            ParseError::of(ParseErrorKind::ExpectedString {
                found: js_typeof(&args[i]),
            })
            .at(i + 1)
        })?;
        bindings.push((name.to_string(), parse_expr(&args[i + 1], ctx)?));
        i += 2;
    }
    let body = parse_expr(&args[args.len() - 1], ctx)?;
    Ok(Expr::Let {
        bindings,
        body: Box::new(body),
    })
}

fn parse_match(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    // args = input, (label, output)+, default  =>  even count, >= 4.
    if args.len() < 4 {
        return Err(ParseError::of(ParseErrorKind::ExpectedAtLeastArgs {
            min: 4,
            found: args.len(),
        }));
    }
    if !args.len().is_multiple_of(2) {
        return Err(ParseError::of(ParseErrorKind::ExpectedEvenArgs));
    }
    // Positions: op[0], input[1], label0[2], out0[3], ..., default[len].
    let input = parse_expr(&args[0], ctx).map_err(|e| e.at(1))?;
    let mut arms = Vec::new();
    let mut i = 1;
    while i + 1 < args.len() {
        let labels = parse_match_labels(&args[i]).map_err(|e| e.at(i + 1))?;
        let output = parse_expr(&args[i + 1], ctx).map_err(|e| e.at(i + 2))?;
        arms.push((labels, output));
        i += 2;
    }
    let default = parse_expr(&args[args.len() - 1], ctx).map_err(|e| e.at(args.len()))?;
    Ok(Expr::Match {
        input: Box::new(input),
        arms,
        default: Box::new(default),
    })
}

/// `match` labels are unquoted literals: a single value, or an array of values.
fn parse_match_labels(json: &Json) -> Result<Vec<Value>> {
    match json {
        Json::Array(items) => Ok(items.iter().map(Value::from_json).collect()),
        Json::Number(_) | Json::String(_) => Ok(vec![Value::from_json(json)]),
        _ => Err(ParseError::of(ParseErrorKind::BranchLabelsType)),
    }
}

fn parse_step(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    // `step.ts:31-39` — a floor first, then parity, as two distinct messages.
    if args.len() < 4 {
        return Err(ParseError::of(ParseErrorKind::ExpectedAtLeastArgs {
            min: 4,
            found: args.len(),
        }));
    }
    if !args.len().is_multiple_of(2) {
        return Err(ParseError::of(ParseErrorKind::ExpectedEvenArgs));
    }
    // Positions: op[0], input[1], output0[2], stop[3], output[4], ...
    let input = parse_expr(&args[0], ctx).map_err(|e| e.at(1))?;
    let output0 = parse_expr(&args[1], ctx).map_err(|e| e.at(2))?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        // The offending stop input is at position [i + 1] (`step.ts:56-62`).
        let stop = args[i].as_f64().ok_or_else(|| {
            ParseError::of(ParseErrorKind::StopInputLiteral {
                kind: "step".to_string(),
            })
            .at(i + 1)
        })?;
        stops.push((
            stop,
            parse_expr(&args[i + 1], ctx).map_err(|e| e.at(i + 2))?,
        ));
        i += 2;
    }
    check_ascending("step", &stops)?;
    Ok(Expr::Step {
        input: Box::new(input),
        output0: Box::new(output0),
        stops,
    })
}

fn parse_interpolate(space: InterpSpace, args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    // Positions: op[0], kind[1], input[2], stop[3], output[4], ...
    //
    // Upstream validates the interpolation type *before* the argument count
    // (`interpolate.ts:108-158`), so `["interpolate", "linear", 1]` reports the
    // type, not the count. A missing type slot reads as a non-array.
    let kind = parse_interp_kind(args.first().unwrap_or(&Json::Null)).map_err(|e| e.at(1))?;
    if args.len() < 4 {
        return Err(ParseError::of(ParseErrorKind::ExpectedAtLeastArgs {
            min: 4,
            found: args.len(),
        }));
    }
    if !args.len().is_multiple_of(2) {
        return Err(ParseError::of(ParseErrorKind::ExpectedEvenArgs));
    }
    let input = parse_expr(&args[1], ctx).map_err(|e| e.at(2))?;
    let mut stops = Vec::new();
    let mut i = 2;
    while i + 1 < args.len() {
        // The offending stop input is at position [i + 1]
        // (`interpolate.ts:175-190`, `labelKey = i + 3` over `rest`).
        let stop = args[i].as_f64().ok_or_else(|| {
            ParseError::of(ParseErrorKind::StopInputLiteral {
                kind: "interpolate".to_string(),
            })
            .at(i + 1)
        })?;
        stops.push((
            stop,
            parse_expr(&args[i + 1], ctx).map_err(|e| e.at(i + 2))?,
        ));
        i += 2;
    }
    check_ascending("interpolate", &stops)?;
    Ok(Expr::Interpolate {
        kind,
        space,
        input: Box::new(input),
        stops,
        projection: false,
    })
}

/// Parse `["collator", options]`, where `options` is an object of
/// `case-sensitive`, `diacritic-sensitive` and `locale` sub-expressions.
fn parse_collator(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    if args.len() != 1 {
        return Err(ParseError::of(ParseErrorKind::ExpectedOneArgument));
    }
    let obj = args[0]
        .as_object()
        .ok_or_else(|| ParseError::of(ParseErrorKind::CollatorOptions))?;
    let opt = |key: &str| -> Result<Option<Box<Expr>>> {
        match obj.get(key) {
            Some(v) => Ok(Some(Box::new(parse_expr(v, ctx)?))),
            None => Ok(None),
        }
    };
    Ok(Expr::Collator {
        case_sensitive: opt("case-sensitive")?,
        diacritic_sensitive: opt("diacritic-sensitive")?,
        locale: opt("locale")?,
    })
}

/// Parse `["number-format", value, options]`.
fn parse_number_format(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    if args.len() != 2 {
        return Err(ParseError::of(ParseErrorKind::ExpectedTwoArguments));
    }
    let value = Box::new(parse_expr(&args[0], ctx)?);
    let obj = args[1]
        .as_object()
        .ok_or_else(|| ParseError::of(ParseErrorKind::NumberFormatOptionsObject))?;
    if obj.contains_key("currency") && obj.contains_key("unit") {
        return Err(ParseError::of(ParseErrorKind::NumberFormatExclusive));
    }
    let opt = |key: &str| -> Result<Option<Box<Expr>>> {
        match obj.get(key) {
            Some(v) => Ok(Some(Box::new(parse_expr(v, ctx)?))),
            None => Ok(None),
        }
    };
    Ok(Expr::NumberFormat {
        value,
        locale: opt("locale")?,
        currency: opt("currency")?,
        min_fraction_digits: opt("min-fraction-digits")?,
        max_fraction_digits: opt("max-fraction-digits")?,
        unit: opt("unit")?,
    })
}

/// Parse `["within", geojson]`, extracting polygon rings (as `[lng, lat]`)
/// from a Polygon, MultiPolygon, Feature, or FeatureCollection.
fn parse_within(args: &[Json]) -> Result<Expr> {
    let err = || {
        ParseError::of(ParseErrorKind::GeojsonPolygon {
            op: "within".to_string(),
        })
    };
    if args.len() != 1 {
        return Err(ParseError::of(ParseErrorKind::RequiresExactlyOneArg {
            op: "within".to_string(),
            found: args.len(),
        }));
    }
    let geojson = &args[0];
    let mut polygons: Vec<Vec<Vec<(f64, f64)>>> = Vec::new();
    let mut add_geometry =
        |ty: Option<&str>, coords: Option<&Json>| match (ty, coords.and_then(Json::as_array)) {
            (Some("Polygon"), Some(c)) => {
                if let Some(p) = parse_polygon(c) {
                    polygons.push(p);
                }
            }
            (Some("MultiPolygon"), Some(c)) => {
                for poly in c.iter().filter_map(Json::as_array) {
                    if let Some(p) = parse_polygon(poly) {
                        polygons.push(p);
                    }
                }
            }
            _ => {}
        };
    match geojson.get("type").and_then(Json::as_str) {
        Some("FeatureCollection") => {
            for feat in geojson
                .get("features")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
            {
                let g = feat.get("geometry");
                add_geometry(
                    g.and_then(|g| g.get("type")).and_then(Json::as_str),
                    g.and_then(|g| g.get("coordinates")),
                );
            }
        }
        Some("Feature") => {
            let g = geojson.get("geometry");
            add_geometry(
                g.and_then(|g| g.get("type")).and_then(Json::as_str),
                g.and_then(|g| g.get("coordinates")),
            );
        }
        Some(t @ ("Polygon" | "MultiPolygon")) => {
            add_geometry(Some(t), geojson.get("coordinates"));
        }
        _ => {}
    }
    if polygons.is_empty() {
        return Err(err());
    }
    Ok(Expr::Within(polygons))
}

/// Parse `["distance", geojson]`, extracting the argument geometries (splitting
/// any `Multi*` into simple Point/LineString/Polygon geometries).
fn parse_distance(args: &[Json]) -> Result<Expr> {
    let err = || {
        ParseError::of(ParseErrorKind::GeojsonPolygon {
            op: "distance".to_string(),
        })
    };
    if args.len() != 1 {
        return Err(ParseError::of(ParseErrorKind::RequiresExactlyOneArg {
            op: "distance".to_string(),
            found: args.len(),
        }));
    }
    let mut geoms: Vec<SimpleGeom> = Vec::new();
    match args[0].get("type").and_then(Json::as_str) {
        Some("FeatureCollection") => {
            for feat in args[0]
                .get("features")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(g) = feat.get("geometry") {
                    add_simple_geometry(g, &mut geoms);
                }
            }
        }
        Some("Feature") => {
            if let Some(g) = args[0].get("geometry") {
                add_simple_geometry(g, &mut geoms);
            }
        }
        Some(_) => add_simple_geometry(&args[0], &mut geoms),
        None => {}
    }
    if geoms.is_empty() {
        return Err(err());
    }
    Ok(Expr::Distance(geoms))
}

fn parse_point(c: &Json) -> Option<(f64, f64)> {
    let a = c.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

fn parse_line(c: &Json) -> Vec<(f64, f64)> {
    c.as_array()
        .map(|a| a.iter().filter_map(parse_point).collect())
        .unwrap_or_default()
}

/// Append the simple geometries of a GeoJSON geometry (splitting `Multi*`).
fn add_simple_geometry(geom: &Json, out: &mut Vec<SimpleGeom>) {
    let coords = geom.get("coordinates");
    match geom.get("type").and_then(Json::as_str) {
        Some("Point") => {
            if let Some(p) = coords.and_then(parse_point) {
                out.push(SimpleGeom::Point(p));
            }
        }
        Some("MultiPoint") => {
            for p in coords.and_then(Json::as_array).into_iter().flatten() {
                if let Some(p) = parse_point(p) {
                    out.push(SimpleGeom::Point(p));
                }
            }
        }
        Some("LineString") => {
            if let Some(c) = coords {
                out.push(SimpleGeom::Line(parse_line(c)));
            }
        }
        Some("MultiLineString") => {
            for l in coords.and_then(Json::as_array).into_iter().flatten() {
                out.push(SimpleGeom::Line(parse_line(l)));
            }
        }
        Some("Polygon") => {
            if let Some(c) = coords.and_then(Json::as_array) {
                if let Some(p) = parse_polygon(c) {
                    out.push(SimpleGeom::Polygon(p));
                }
            }
        }
        Some("MultiPolygon") => {
            for poly in coords.and_then(Json::as_array).into_iter().flatten() {
                if let Some(p) = poly.as_array().and_then(|r| parse_polygon(r)) {
                    out.push(SimpleGeom::Polygon(p));
                }
            }
        }
        _ => {}
    }
}

/// Parse a GeoJSON polygon (array of rings of `[lng, lat]`).
fn parse_polygon(rings: &[Json]) -> Option<Vec<Vec<(f64, f64)>>> {
    let mut out = Vec::new();
    for ring in rings.iter().filter_map(Json::as_array) {
        let mut r = Vec::new();
        for pt in ring.iter().filter_map(Json::as_array) {
            let lng = pt.first().and_then(Json::as_f64)?;
            let lat = pt.get(1).and_then(Json::as_f64)?;
            r.push((lng, lat));
        }
        out.push(r);
    }
    Some(out)
}

fn parse_format(args: &[Json], ctx: &Ctx<'_>) -> Result<Expr> {
    if args.is_empty() {
        return Err(ParseError::of(ParseErrorKind::ExpectedAtLeastOneArgument));
    }
    // Upstream's guard is `!Array.isArray(firstArg) && typeof firstArg ===
    // 'object'` (`format.ts:48`), and JS reports `typeof null` as `"object"`,
    // so a leading `null` is rejected here too — not just a bare options
    // object.
    if args[0].is_object() || args[0].is_null() {
        return Err(ParseError::of(ParseErrorKind::FormatFirstSection));
    }
    let mut sections: Vec<FormatArg> = Vec::new();
    let mut next_may_be_object = false;
    // A section's options (text-font, font-scale, ...) are keyed at the
    // section's content position, not the options-object position.
    let mut content_pos = 1;
    for (j, arg) in args.iter().enumerate() {
        let pos = j + 1;
        if next_may_be_object && arg.is_object() {
            next_may_be_object = false;
            let obj = arg.as_object().unwrap();
            let sec = content_pos;
            let section = sections.last_mut().unwrap();
            if let Some(v) = obj.get("font-scale") {
                section.scale = Some(parse_expr(v, ctx).map_err(|e| e.at(sec))?);
            }
            if let Some(v) = obj.get("text-font") {
                section.font = Some(parse_expr(v, ctx).map_err(|e| e.at(sec))?);
            }
            if let Some(v) = obj.get("text-color") {
                section.text_color = Some(parse_expr(v, ctx).map_err(|e| e.at(sec))?);
            }
            if let Some(v) = obj.get("vertical-align") {
                if let Some(s) = v.as_str() {
                    if !VERTICAL_ALIGN.contains(&s) {
                        return Err(ParseError::of(ParseErrorKind::VerticalAlign {
                            found: s.to_string(),
                        }));
                    }
                }
                section.vertical_align = Some(parse_expr(v, ctx).map_err(|e| e.at(sec))?);
            }
        } else {
            content_pos = pos;
            sections.push(FormatArg {
                content: parse_expr(arg, ctx).map_err(|e| e.at(pos))?,
                scale: None,
                font: None,
                text_color: None,
                vertical_align: None,
            });
            next_may_be_object = true;
        }
    }
    Ok(Expr::Format(sections))
}

/// Parse the interpolation-type slot of `interpolate` (`interpolate.ts:106-148`).
///
/// Errors are keyed *within* that slot: the caller prepends `[1]`, so an
/// unknown type name comes out at `[1][0]`, the position of the name itself.
fn parse_interp_kind(json: &Json) -> Result<InterpKind> {
    let items = match json.as_array() {
        // An empty array is rejected the same way a non-array is.
        Some(items) if !items.is_empty() => items,
        _ => return Err(ParseError::of(ParseErrorKind::InterpolationTypeArray)),
    };
    // A non-string head is not a separate error upstream: it falls through to
    // "unknown interpolation type", stringified by JavaScript's `String()`.
    let Some(name) = items[0].as_str() else {
        return Err(ParseError::of(ParseErrorKind::UnknownInterpolationType {
            name: js_string(&items[0]),
        })
        .at(0));
    };
    match name {
        "linear" => Ok(InterpKind::Linear),
        "exponential" => {
            // The base is at index [1] within the interpolation-type array.
            let base = items
                .get(1)
                .and_then(Json::as_f64)
                .ok_or_else(|| ParseError::of(ParseErrorKind::ExponentialBase).at(1))?;
            Ok(InterpKind::Exponential(base))
        }
        "cubic-bezier" => {
            // Reported at the interpolation-type slot itself (no sub-index).
            let cubic_err = || ParseError::of(ParseErrorKind::CubicBezier);
            // Exactly four control points, each in 0..=1.
            if items.len() != 5 {
                return Err(cubic_err());
            }
            let n = |i: usize| items.get(i).and_then(Json::as_f64);
            match (n(1), n(2), n(3), n(4)) {
                (Some(a), Some(b), Some(c), Some(d))
                    if [a, b, c, d].iter().all(|v| (0.0..=1.0).contains(v)) =>
                {
                    Ok(InterpKind::CubicBezier(a, b, c, d))
                }
                _ => Err(cubic_err()),
            }
        }
        other => Err(ParseError::of(ParseErrorKind::UnknownInterpolationType {
            name: other.to_string(),
        })
        .at(0)),
    }
}

/// JavaScript's `String()` for a JSON value, as `interpolate.ts:144-147` uses
/// it to name an unrecognized interpolation type.
fn js_string(v: &Json) -> String {
    match v {
        Json::Null => "null".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.to_string(),
        Json::String(s) => s.clone(),
        // `Array.prototype.toString` joins with "," and renders null/undefined
        // as the empty string; a plain object is "[object Object]".
        Json::Array(items) => items
            .iter()
            .map(|v| match v {
                Json::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Json::Object(_) => "[object Object]".to_string(),
    }
}

/// Validate the (optional) item-type and length arguments of `array` against
/// the raw JSON: they must be a bare type name and a bare non-negative integer,
/// not `["literal", ...]` sub-expressions.
fn validate_array_type_args(args: &[Json]) -> Result<()> {
    if args.len() < 2 {
        return Ok(());
    }
    match args[0].as_str() {
        Some("string" | "number" | "boolean") => {}
        _ => {
            // The item type is the first argument, at position [1].
            return Err(ParseError::of(ParseErrorKind::ArrayItemType).at(1));
        }
    }
    if args.len() >= 3 {
        // The length may be null (unspecified) or a non-negative integer.
        if !args[1].is_null() {
            match args[1].as_f64() {
                Some(n) if n >= 0.0 && n.fract() == 0.0 => {}
                _ => {
                    // The length is the second argument, at position [2].
                    return Err(ParseError::of(ParseErrorKind::ArrayLength).at(2));
                }
            }
        }
    }
    Ok(())
}

fn check_ascending(kind: &str, stops: &[(f64, Expr)]) -> Result<()> {
    for (j, pair) in stops.windows(2).enumerate() {
        if pair[1].0 <= pair[0].0 {
            // The offending stop is `stops[j + 1]`, whose input is at
            // position [3 + 2*(j+1)] in the original array.
            return Err(ParseError::of(ParseErrorKind::AscendingStops {
                kind: kind.to_string(),
            })
            .at(3 + 2 * (j + 1)));
        }
    }
    Ok(())
}

/// Check that `op` — one of the operators that names itself in its arity
/// message (`literal.ts:18-21`) — was given exactly one argument.
fn expect_one_arg(op: &str, args: &[Json]) -> Result<()> {
    if args.len() == 1 {
        Ok(())
    } else {
        Err(ParseError::of(ParseErrorKind::RequiresExactlyOneArg {
            op: op.to_string(),
            found: args.len(),
        }))
    }
}
