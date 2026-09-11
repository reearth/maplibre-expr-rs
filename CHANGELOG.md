# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `filter::parse_filter` (and `parse_filter_with` for user `Options`) — a
  one-call convenience that combines `convert_legacy_filter` and `parse`, so a
  legacy layer filter (`["all", ["!=", "name", "International Date Line"]]`,
  `["==", "class", "primary"]`, …) parses correctly without the caller
  remembering to convert first. Handing such a filter straight to `parse`
  parses it as a comparison of two literals, which then fails type-check
  (`Cannot compare types 'string' and 'number'.` for the reduced
  `["!=", "No", 2]` case from MapLibre's demotiles style) or silently
  evaluates the wrong thing. Mirrors MapLibre's `createFilter`. A new
  `ParseFilterError` wraps the two ways it can fail (`Convert` /
  `Parse`).

- `collator` feature (on by default) gating the ICU4X-backed, locale-aware
  `collator` comparisons. With `default-features = false` the crate carries no
  CLDR data: `["collator", …]` still parses and type-checks identically and
  `resolved-locale` still works, but comparisons ignore the locale and the
  `case-sensitive` / `diacritic-sensitive` options and fall back to code-point
  order. The 15 conformance fixtures that depend on CLDR tailoring are reported
  as *ignored* in that configuration rather than silently dropped.

### Changed

- **Breaking:** the user-extension API is renamed to keep clear of MapLibre's
  own terminology, where a *function* is the legacy `{ "stops": … }` object
  (zoom / property function) that `convert` translates. Eval-time bodies
  written in the expression language are now *expression functions*:
  `Function` → `ExprFn`, `Options::function` → `Options::expr_fn`. Rust
  closures are *external functions*, after the upstream
  [external-functions proposal](https://github.com/maplibre/maplibre-style-spec/issues/516):
  `NativeFn` → `ExternalFn`, `Options::native` → `Options::external`.
  `Macro` / `Options::macro_def` are unchanged. The `ExtArgCount` parse error
  now reports the kind as `Expression function` / `External function`, and the
  call-depth error message says `calling expression function`.
- Expression-function bodies are now parsed once per `Options` (lazily, on
  the first `evaluate_with`) and cached until the next registration, instead
  of being re-parsed on every `evaluate_with` call. A body that fails to parse
  still surfaces as an `EvalErrorKind::Other` at evaluation time.
- README restructured: a short quick start, then *Usage* (pipeline, errors,
  legacy inputs, extensions with a macro / expression-function /
  external-function comparison), *Feature flags*, and *Development*.
- Depend on `icu_collator` and `icu_locale_core` directly instead of the `icu`
  meta-crate, which also built the datetime, segmenter, calendar, list and
  plurals data crates that nothing here uses. No behaviour change; the
  dependency graph drops from 68 crates to 42 (and to 14 without `collator`).

## [0.3.2]

### Added

- `is_expression` — whether a JSON value is a MapLibre expression (an array
  whose first element names a built-in operator) rather than a literal value
  such as a bare `["Font A", "Font B"]` array or a legacy function object. A
  syntactic head check analogous to MapLibre's `isExpression`; it does not
  validate arity or arguments. Lets callers tell a data-driven property
  expression apart from a plain array literal.

## [0.3.1]

### Fixed

- `convert_legacy_filter` now rewrites the legacy-only leaves of a *mixed*
  `all`/`any`/`none` combiner instead of passing it through untouched. When a
  combiner is classified as an expression (because at least one child is a
  genuine expression) yet still carries a legacy-only leaf — e.g. a three-arg
  `["==", "prop", value]` or an `["!has", …]` — the legacy leaves are converted
  in place while genuine expression children pass through unchanged. Previously
  such a filter was returned verbatim, leaving a raw legacy operator (like
  `!has`) that no expression evaluator can parse. Real-world styles hit this —
  e.g. the Protomaps basemap `roads_bridges_*` layers use
  `["all", ["has", …], ["==", "kind", …], ["!has", …]]`. `is_expression_filter`
  (the classifier) is unchanged and still mirrors upstream MapLibre.
