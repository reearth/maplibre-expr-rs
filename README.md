# maplibre-expr-rs

[![crates.io](https://img.shields.io/crates/v/maplibre-expr.svg)](https://crates.io/crates/maplibre-expr)
[![docs.rs](https://img.shields.io/docsrs/maplibre-expr)](https://docs.rs/maplibre-expr)

A pure-Rust parser and evaluator for [MapLibre GL style expressions][spec] that
aims to behave **exactly** like the reference implementation — not just the same
results, but the same compile errors, in the same places.

- 🎯 **Exhaustive compatibility.** Passes the **entire** upstream conformance
  suite — **563/563** fixtures, zero skipped — including legacy stop functions,
  type coercion, and every edge case.
- 🧭 **Byte-exact errors.** Compile- and eval-error messages match MapLibre's
  wording **character-for-character**, with the same location `key`
  (e.g. `"[4][0]"`). The test harness enforces this.
- 🦀 **Pure Rust, tiny surface.** No rendering, no I/O, no C deps — just
  `serde_json`, plus a pure-Rust ICU for `collator` (optional, see
  [Feature flags](#feature-flags)). Works anywhere Rust does, including wasm.
- 🧱 **Real pipeline.** `parse` → static `typecheck` (the same inference and
  coercion pass MapLibre runs) → `evaluate` against a zoom + feature context.
- 🗺️ **Real styles, as they are.** `migrate` ports MapLibre's style migration,
  with the style-spec reference embedded: hand it a style still using stop
  functions, `{token}` strings and legacy filters (the official demo tiles do)
  and get the same expressions the reference implementation produces.
- 🔌 **Extensible.** Plug in your own operators as macros, expression
  functions, or external Rust closures — without forking the language.

## Quick start

```rust
use maplibre_expr::{parse, evaluate, EvaluationContext, Feature, Value};
use std::collections::BTreeMap;

let expr = parse(&serde_json::json!(["*", ["get", "x"], 2])).unwrap();

let mut props = BTreeMap::new();
props.insert("x".to_string(), Value::Number(21.0));
let ctx = EvaluationContext::new().with_feature(Feature {
    properties: props,
    ..Default::default()
});

assert_eq!(evaluate(&expr, &ctx).unwrap(), Value::Number(42.0));
```

## Usage

### Pipeline: parse, typecheck, evaluate

`parse` turns expression JSON into an `Expr` tree. `typecheck(&expr, expected)`
then runs the static pass MapLibre performs at compile time: it infers each
node's result type, checks operator argument types, and reconciles against an
optional expected type (assert / coerce / subtype). It rejects, for example,
comparisons between incompatible types, malformed `match` branches,
non-interpolatable `interpolate` outputs, and `zoom` outside a single top-level
curve. `evaluate` finally runs the tree against an `EvaluationContext` — zoom,
feature properties, geometry, and so on.

### Errors

`ParseError` / `EvalError` carry a `kind` you can match on (`UnknownExpression`,
`WrongArgCount`, `TypeMismatch`, `NotComparable`, `UnboundVariable`, …), and
`Display` renders MapLibre's exact message. `ParseError` also carries a `key`:
the location of the offending sub-expression, such as `"[2]"` or `"[4][0]"`,
collected as the error bubbles up. Both the message and the key match the
reference implementation byte-for-byte across the conformance suite.

Every intrinsic error has a dedicated variant. `Other(String)` is reserved for
message-only cases: the user-thrown `["error", msg]` operator, runtime errors
surfaced by compile-time constant folding, and expression-function bodies that
fail to parse.

### Legacy inputs

Real-world styles still use three pre-expression forms: *function objects*
such as `{"type": "exponential", "property": "x", "stops": [...]}`, `{token}`
strings in `text-field` / `icon-image`, and *legacy filters* with bare property
names like `["==", "class", "primary"]`. MapLibre converts all of them to
expressions before evaluating, and so can this crate.

**Migrate a whole style** with `migrate`, the port of maplibre-style-spec's
`migrate`. It walks every layer and rewrites filters, function objects and
token strings using the embedded style-spec reference — the property's type,
whether it interpolates, whether it takes tokens — so the output is exactly
what the reference implementation produces. The MapLibre demo style is
checked against `gl-style-migrate`'s output in the test suite.

```rust
use maplibre_expr::{migrate, parse, evaluate, EvaluationContext};
use serde_json::json;

let style = json!({
    "version": 8,
    "layers": [{
        "id": "label", "type": "symbol", "source": "s",
        "layout": {
            "text-field": "{name}",
            "text-size": {"stops": [[2, 10], [6, 16]]},
        },
        "filter": ["!=", "name", ""],
    }],
});
let style = migrate(&style).unwrap();
let layer = &style["layers"][0];
assert_eq!(layer["layout"]["text-field"], json!(["to-string", ["get", "name"]]));
assert_eq!(
    layer["layout"]["text-size"],
    json!(["interpolate", ["linear"], ["zoom"], 2, 10, 6, 16])
);
assert_eq!(layer["filter"], json!(["!=", ["get", "name"], ""]));

let size = parse(&layer["layout"]["text-size"]).unwrap();
let ctx = EvaluationContext::new().with_zoom(4.0);
assert_eq!(evaluate(&size, &ctx).unwrap(), maplibre_expr::Value::Number(13.0));
```

`migrate::migrate_property(name, value)` does the same for one layout/paint
value, and `migrate_with` / `migrate_property_with` accept a custom reference
(a fork of the spec, say). Only `version: 8` styles are handled; the ancient
v7 → v8 rewrite is not ported.

**Parse a single property value** with `parse_property(name, value)`, the
counterpart of MapLibre's `createPropertyExpression(value, propertySpec)`: it
converts a function object or token string with the property's spec from the
embedded reference, then parses, so one value comes out exactly as it would
from a migrated style.

```rust
use maplibre_expr::{parse_property, evaluate, EvaluationContext, Value};
use serde_json::json;

let width = parse_property("line-width", &json!({"stops": [[2, 10], [6, 16]]})).unwrap();
let ctx = EvaluationContext::new().with_zoom(4.0);
assert_eq!(evaluate(&width, &ctx).unwrap(), Value::Number(13.0)); // interpolated
```

`parse` itself, like MapLibre's `createExpression`, accepts only expressions
and rejects a bare object with the same `Bare objects invalid` error — there
is no spec-less guessing. `filter::parse_filter` converts a legacy filter
before parsing exactly as MapLibre's `createFilter` does (no spec is
involved). The lower-level pieces are public too: `convert::convert_function`,
`convert::convert_token_string`, and `filter::convert_legacy_filter`, which
reproduces legacy filter semantics faithfully (strictly-typed comparisons that
yield `false` on a type mismatch, the `$type` / `$id` keys, and the `typeof`
guards that keep one `any` term from erroring out its siblings).

### Extensions

You can plug your own operators in through `Options`, passed to `parse_with` /
`evaluate_with`. A tree that uses none of them parses and evaluates identically
with or without the options — this is not a new dialect.

| Kind | Registered with | Body | Runs | Recursion | Result tree |
| --- | --- | --- | --- | --- | --- |
| **Macro** | `Options::macro_def` | expression JSON | expands at parse time into a `let` | no (depth-limited) | plain MapLibre expression |
| **Expression function** | `Options::expr_fn` | expression JSON | at evaluation time | yes (depth-limited) | contains the custom call |
| **External function** | `Options::external` | Rust closure | at evaluation time | n/a | contains the custom call |

**Prefer macros.** Because a macro disappears into a standard expression, the
result needs no options to evaluate, is fully type-checked and constant-folded,
and could be handed to any other MapLibre implementation. Reach for an
expression function only when you need recursion, and for an external function
when the logic can't be written as an expression at all. Calls to either are
opaque to `typecheck` (typed as `value`). Expression-function bodies are parsed
once per `Options`, on first use, and cached until the next registration.

```rust
use maplibre_expr::{parse_with, evaluate_with, EvaluationContext, Options, Value};
use serde_json::json;

let mut opts = Options::new();
// Macro: expands to ["let", "x", <arg>, ["*", ["var", "x"], 2]].
opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));
// Expression function: recursive, so it can't be a macro.
opts.expr_fn(
    "sum",
    vec!["n".into()],
    json!(["case", ["<=", ["var", "n"], 0], 0,
           ["+", ["var", "n"], ["sum", ["-", ["var", "n"], 1]]]]),
);
// External function: a Rust closure over the evaluated arguments and context.
opts.external("hypot", 2, |args, _ctx| {
    let x = args[0].as_number().unwrap_or(0.0);
    let y = args[1].as_number().unwrap_or(0.0);
    Ok(Value::Number(x.hypot(y)))
});

let expr = parse_with(&json!(["hypot", ["sum", ["double", 3]], 28]), &opts).unwrap();
let out = evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap();
assert_eq!(out, Value::Number(35.0)); // hypot(sum(6) = 21, 28)
```

`Options` is `Send + Sync` (external closures must be too), so one registry can
be shared across threads.

## Feature flags

| Feature | Default | Effect |
| --- | --- | --- |
| `collator` | ✅ | Locale-aware `collator` comparisons via [`icu_collator`]'s embedded CLDR data. |

The CLDR tables are the crate's only heavyweight dependency — roughly 1.1 MB of
static data and ~28 extra crates. If your styles don't use `collator` (most
don't), turn the feature off, especially for wasm:

```toml
maplibre-expr = { version = "0.5", default-features = false }
```

This does **not** change what the crate accepts: `["collator", …]` still parses
and type-checks identically, and `resolved-locale` still works. Only the
comparison changes — the locale and the `case-sensitive` / `diacritic-sensitive`
options are ignored and operands compare in code-point order. The 15 fixtures
that depend on CLDR tailoring are reported as *ignored* in that configuration.

[`icu_collator`]: https://crates.io/crates/icu_collator

## Development

### Conformance testing

The crate is validated against a vendored snapshot of the upstream
[`maplibre-style-spec`][spec] expression fixtures (`tests/fixtures/expression`;
see `ATTRIBUTION.md` there). `tests/spec.rs` turns each fixture directory into
one libtest case:

```
cargo test --test spec
# test result: ok. 563 passed; 0 failed; 0 ignored; ...
```

For every fixture it compiles the expression (`parse` + `typecheck`, with the
expected type from the fixture's `propertySpec`), checks success vs. compile
error, then evaluates each `input` and compares to the expected `output`.
Numbers use the upstream 6-significant-figure rule; colors compare
premultiplied. For error fixtures it also asserts **parity**: our message text
and location `key` must equal the fixture's exactly. `PARITY=1` prints a
coverage report instead of the pass/fail run.

The harness verifies `compiled.result`, the per-input `outputs`, and error
parity. It does not assert the other static-analysis fields (`type`,
`isFeatureConstant`, `isZoomConstant`). Refresh the snapshot with
`tests/refresh_fixtures.sh [git-ref]`.

### Implementation notes

- **`distance` is a brute-force pairwise scan**, not MapLibre's bounding-volume
  hierarchy. The minimum distance doesn't depend on traversal order, so results
  are identical; the cost is `O(n·m)` in vertex counts, which is negligible for
  tile-sized geometry. Add a spatial index here if you need more.
- Feature coordinates round-trip through tile coordinates before `distance` /
  `within`, matching MapLibre's quantization.
- **`collator` uses CLDR collation via [`icu_collator`]**; Intl's `sensitivity`
  maps to an ICU strength plus case level.

## Community

`maplibre-expr-rs` is part of the [MapLibre](https://maplibre.org) ecosystem.
Discussion happens in the `#maplibre` channel on the OSM-US Slack — join with
the [OSM-US Slack invite](https://slack.openstreetmap.us).

Please also see our [Code of Conduct](CODE_OF_CONDUCT.md), the
[Contributing guide](CONTRIBUTING.md), and the
[Security policy](SECURITY.md).

## License

Copyright (c) 2026 MapLibre contributors

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Vendored test fixtures under `tests/fixtures/expression` are
from `maplibre/maplibre-style-spec` (BSD-3-Clause); see their `ATTRIBUTION.md`.

[spec]: https://maplibre.org/maplibre-style-spec/expressions/
