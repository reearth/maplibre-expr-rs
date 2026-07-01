# maplibre-expr

A pure-Rust parser and evaluator for [MapLibre GL style expressions][spec].

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

## Conformance testing

The crate is validated against a **vendored snapshot** of the upstream
[`maplibre-style-spec`][spec] expression fixtures (`tests/fixtures/expression`,
see `tests/fixtures/ATTRIBUTION.md`). The harness in `tests/spec.rs` turns
**each fixture directory into one libtest case** (via `libtest-mimic`), so a
run reads like:

```
cargo test --test spec
# test result: ok. 386 passed; 0 failed; 177 ignored; ...
```

For every fixture it compiles the `expression` (`parse` + `typecheck`, with the
expected type taken from the fixture's `propertySpec`), checking success vs.
compile error, then evaluates it against each `input` and compares to the
expected `output`, matching `{ "error": ... }` outputs against evaluation
errors. Numbers are compared with the same 6-significant-figure `stripPrecision`
rule the upstream suite uses; colors are compared premultiplied, matching
MapLibre's internal `Color`.

**Scope note:** the harness verifies `compiled.result` (success/error) and the
per-input `outputs`. It does **not** compare compile-error *messages* (only that
an error is raised — see [Type checking](#type-checking)), nor assert the other
static-analysis fields (`type`, `isFeatureConstant`, `isZoomConstant`).

### The skip-list is the roadmap

Fixtures that don't pass yet are listed in `tests/known_failures.txt` and
reported as **ignored** rather than failing the build. That file is grouped by
*reason* (unimplemented operators, HCL/LAB color spaces, compile-time type
validation, type-context coercion, legacy function syntax), so it doubles as
the to-do list. Nothing is skipped silently.

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
