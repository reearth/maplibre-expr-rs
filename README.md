# maplibre-expr

[![crates.io](https://img.shields.io/crates/v/maplibre-expr.svg)](https://crates.io/crates/maplibre-expr)
[![docs.rs](https://img.shields.io/docsrs/maplibre-expr)](https://docs.rs/maplibre-expr)

A pure-Rust parser and evaluator for [MapLibre GL style expressions][spec] that
aims to behave **exactly** like the reference implementation — not just the same
results, but the same compile errors, in the same places.

- 🎯 **Exhaustive compatibility.** Passes the **entire** upstream conformance
  suite — **563/563** fixtures, zero skipped. Every operator, legacy
  stop-function, type coercion, and edge case behaves like `maplibre-gl-js`.
- 🧭 **Byte-exact errors.** Compile- and eval-error messages match MapLibre's
  wording **character-for-character**, and each compile error carries the same
  location `key` (e.g. `"[4][0]"`). The test harness enforces this, so the
  parity can't silently regress.
- 🦀 **Pure Rust, tiny surface.** No rendering, no I/O, no C deps — just
  `serde_json` and a pure-Rust ICU for locale-aware collation. Works anywhere
  Rust does.
- 🧱 **Real pipeline.** `parse` → optional static `typecheck` (the same
  type-inference/coercion pass MapLibre runs) → `evaluate` against a
  zoom + feature context. Full coverage: `match`/`step`/`interpolate`, `format`,
  `collator`, `within`/`distance` geometry, `number-format`, images, and more.
- 🔌 **Extensible.** Plug in your own operators as macros, recursive functions,
  or native Rust closures — without forking the language.

It turns a MapLibre expression (JSON such as `["*", ["get", "x"], 2]`) into a
typed tree with `parse`, optionally validates it with `typecheck`, then
evaluates that tree against an `EvaluationContext` (zoom + feature) with
`evaluate`.

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

## Type checking

`typecheck(&expr, expected)` runs a static pass that mirrors the compile-time
validation MapLibre performs while parsing: it infers each node's result type,
checks operator argument types, and reconciles against an optional expected
type (assert/coerce/subtype). It rejects, for example, comparisons between
incompatible types, malformed `match` branches, non-interpolatable
`interpolate` outputs, bad `array` item-type/length arguments, and misuse of
`zoom` outside a single top-level curve.

**Errors are semantic *and* match MapLibre's wording.** `ParseError`/`EvalError`
carry a `kind` ([`ParseErrorKind`]/[`EvalErrorKind`]) you can match on —
`UnknownExpression`, `WrongArgCount`, `TypeMismatch`, `NotComparable`,
`CannotCompare`, `NotInterpolatable`, `UnboundVariable`, `Zoom`, … — with a
`Display` "printer" rendering the message. `ParseError` also carries a `key`, the
location path of the offending sub-expression (e.g. `"[2]"` or `"[4][0]"`),
collected as the error bubbles up. Both the message text and the location key
match the reference implementation **byte-for-byte** across the conformance
suite, and the harness enforces this (see [Conformance testing](#conformance-testing)).
Every intrinsic error has a dedicated variant (`CouldNotParse`,
`ArrayIndexOutOfBounds`, `InvalidRgba`, `BranchLabels*`, `ExpectedEvenArgs`,
`InterpolationTypeArray`, …). The `Other(String)` kind is reserved for
message-only cases with no fixed category: the user-thrown `["error", msg]`
operator, and runtime errors surfaced by compile-time constant folding.

## Extensions: macros and functions

Beyond the standard operators, you can plug your own operators in through
[`Options`], passed to `parse_with` / `evaluate_with`:

- A **macro** expands at parse time into a `let` binding its parameters to the
  call arguments — zero runtime cost, but it cannot recurse (a recursion-depth
  limit rejects cyclic macros).
- A **function** stays a call in the tree and runs at evaluation time, so it
  *may* recurse; a call-depth limit turns runaway recursion into an error
  instead of a stack overflow.
- A **native function** ([`Options::native`]) is a Rust closure invoked with
  the evaluated arguments (and the context), so results can be computed
  dynamically. `Options` is `Send + Sync`, so the registry can be shared across
  threads (native closures must be `Send + Sync`).

```rust
use maplibre_expr::{parse_with, evaluate_with, EvaluationContext, Options, Value};
use serde_json::json;

let mut opts = Options::new();
opts.macro_def("double", vec!["x".into()], json!(["*", ["var", "x"], 2]));
opts.function(
    "sum",
    vec!["n".into()],
    json!(["case", ["<=", ["var", "n"], 0], 0,
           ["+", ["var", "n"], ["sum", ["-", ["var", "n"], 1]]]]),
);

let expr = parse_with(&json!(["sum", ["double", 3]]), &opts).unwrap();
let out = evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap();
assert_eq!(out, Value::Number(21.0)); // sum(6)
```

A **native function** is just a Rust `fn`/closure — it receives the already
evaluated arguments plus the context, so it can compute anything:

```rust
use maplibre_expr::{parse_with, evaluate_with, EvaluationContext, Options, Value};
use serde_json::json;

let mut opts = Options::new();
opts.native("hypot", 2, |args, _ctx| {
    let x = args[0].as_number().unwrap_or(0.0);
    let y = args[1].as_number().unwrap_or(0.0);
    Ok(Value::Number(x.hypot(y)))
});

let expr = parse_with(&json!(["hypot", 3, 4]), &opts).unwrap();
let out = evaluate_with(&expr, &EvaluationContext::new(), &opts).unwrap();
assert_eq!(out, Value::Number(5.0));
```

These are parser/runtime *options*, not a new dialect — a tree without any
custom operators parses and evaluates identically with or without them.

[`Options`]: https://docs.rs/maplibre-expr

## Implementation notes

- **`distance` uses a brute-force pairwise scan** rather than MapLibre's
  bounding-volume hierarchy. The minimum distance is independent of traversal
  order, so the result is identical; the trade-off is scalability — this is
  `O(n·m)` in the vertex counts, where MapLibre's BVH prunes distant pairs.
  For tile-sized geometry the difference is negligible, and the code is far
  simpler. (If you need large-geometry performance, this is the place to add a
  spatial index.)
- Feature coordinates are round-tripped through tile coordinates before
  `distance`/`within`, matching MapLibre's quantization so results agree.
- **`collator` uses CLDR collation via [`icu_collator`]** (pure Rust), so
  locale-aware ordering works for any locale — Intl's `sensitivity` maps to an
  ICU strength plus case level.

[`icu_collator`]: https://crates.io/crates/icu_collator

## Conformance testing

The crate is validated against a **vendored snapshot** of the upstream
[`maplibre-style-spec`][spec] expression fixtures (`tests/fixtures/expression`,
see `tests/fixtures/ATTRIBUTION.md`). The harness in `tests/spec.rs` turns
**each fixture directory into one libtest case** (via `libtest-mimic`), so a
run reads like:

```
cargo test --test spec
# test result: ok. 563 passed; 0 failed; 0 ignored; ...
```

For every fixture it compiles the `expression` (`parse` + `typecheck`, with the
expected type taken from the fixture's `propertySpec`; legacy stop-function
objects are converted first), checking success vs. compile error, then evaluates
it against each `input` and compares to the expected `output`, matching
`{ "error": ... }` outputs against evaluation errors. Numbers are compared with
the same 6-significant-figure `stripPrecision` rule the upstream suite uses;
colors are compared premultiplied, matching MapLibre's internal `Color`.

For error fixtures it also asserts **error parity**: our `ParseError`/`EvalError`
message text and (for compile errors) the location `key` must match the
fixture's `expected.compiled.errors[0]` / `outputs[i].error` exactly. Running
the harness with `PARITY=1` prints a coverage report of message/key agreement
instead of the pass/fail run.

**Scope note:** the harness verifies `compiled.result` (success/error), the
per-input `outputs`, and error message/key parity. It does **not** assert the
other static-analysis fields (`type`, `isFeatureConstant`, `isZoomConstant`).

Refresh the vendored snapshot with `tests/refresh_fixtures.sh [git-ref]`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Vendored test fixtures under `tests/fixtures/expression` are
from `maplibre/maplibre-style-spec` (BSD-3-Clause); see their `ATTRIBUTION.md`.

[spec]: https://maplibre.org/maplibre-style-spec/expressions/
