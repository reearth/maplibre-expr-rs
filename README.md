# maplibre-expr

A pure-Rust parser and evaluator for [MapLibre GL style expressions][spec].
It passes the **entire** upstream expression conformance suite (563/563).

It turns a MapLibre expression (JSON such as `["*", ["get", "x"], 2]`) into a
typed tree with `parse`, optionally validates it with `typecheck`, then
evaluates that tree against an `EvaluationContext` (zoom + feature) with
`evaluate`. No rendering, no I/O — just the expression language.

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

## Layout

| Module        | Responsibility                                              |
| ------------- | ----------------------------------------------------------- |
| `value.rs`    | `Value` — the expression type system (+ number formatting)  |
| `color.rs`    | `Color` and a CSS color parser (hex, `rgb()/hsl()`, named)  |
| `context.rs`  | `EvaluationContext` (zoom + `Feature`)                      |
| `ast.rs`      | `Expr` — the parsed tree; special forms for let/match/step/interpolate |
| `parse.rs`    | JSON → `Expr`, with operator/arity validation               |
| `typ.rs`      | `Type` and the subtyping relation                           |
| `typecheck.rs`| Static type inference & validation (compile-time errors)    |
| `eval.rs`     | Evaluating an `Expr` against a context                       |

## Type checking

`typecheck(&expr, expected)` runs a static pass that mirrors the compile-time
validation MapLibre performs while parsing: it infers each node's result type,
checks operator argument types, and reconciles against an optional expected
type (assert/coerce/subtype). It rejects, for example, comparisons between
incompatible types, malformed `match` branches, non-interpolatable
`interpolate` outputs, bad `array` item-type/length arguments, and misuse of
`zoom` outside a single top-level curve.

**Error messages are not reproduced (yet).** The pass detects the *same error
conditions* as the reference implementation — logically, the same expressions
are rejected — but the returned `ParseError` text is our own and does not match
MapLibre's wording. Message-for-message parity is future work; today the
conformance suite only checks *whether* an expression compiles, not the error
string.

## Extensions: macros and functions

Beyond the standard operators, you can plug your own operators in through
[`Options`], passed to `parse_with` / `evaluate_with`:

- A **macro** expands at parse time into a `let` binding its parameters to the
  call arguments — zero runtime cost, but it cannot recurse (a recursion-depth
  limit rejects cyclic macros).
- A **function** stays a call in the tree and runs at evaluation time, so it
  *may* recurse; a call-depth limit turns runaway recursion into an error
  instead of a stack overflow.

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

**Scope note:** the harness verifies `compiled.result` (success/error) and the
per-input `outputs`. It does **not** compare compile-error *messages* (only that
an error is raised — see [Type checking](#type-checking)), nor assert the other
static-analysis fields (`type`, `isFeatureConstant`, `isZoomConstant`).

### The skip-list

`tests/known_failures.txt` lists any fixtures to report as **ignored** rather
than failing the build; it is currently empty (the whole suite passes). It is
the running to-do list should the vendored fixtures be refreshed to a newer
spec — add a failing fixture's name to keep the build green while you catch up.

To make progress: implement a behaviour, delete the corresponding line(s) from
`known_failures.txt`, and the fixtures graduate to enforced tests.

### Refreshing the fixtures

```sh
tests/refresh_fixtures.sh [git-ref]   # re-vendors and prints the new commit
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Vendored test fixtures under `tests/fixtures/expression` are
from `maplibre/maplibre-style-spec` (BSD-3-Clause); see their `ATTRIBUTION.md`.

[spec]: https://maplibre.org/maplibre-style-spec/expressions/
