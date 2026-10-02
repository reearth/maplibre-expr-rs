# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.4]

### Fixed

- Transcendental functions now come from the pure-Rust `libm` crate instead of
  `f64`'s methods, so an expression evaluates to the same bits on every host,
  native and WebAssembly alike. The std methods call the platform's libm
  natively and a different implementation on wasm, and the two can disagree in
  the last bit. This covers the `sin`, `cos`, `tan`, `asin`, `acos`, `atan`,
  `ln`, `log2`, `log10` and `^` operators, `exponential` interpolation, the
  Lab/HCL colour-space conversions behind `interpolate-lab` and
  `interpolate-hcl`, and the projection and ruler math in `within` and
  `distance`. Results may differ from earlier releases in the last bit.
  A `clippy.toml` now forbids the std methods.

## [0.5.3]

### Added

- Variadic external functions. `Options::external` now takes
  `impl Into<Option<usize>>` for its arity: pass a count (`2`) as before for
  an exact, parse-time-checked arity, or `None` to accept any number of
  arguments, which the closure receives as-is. A variadic closure that needs a
  minimum checks `args.len()` itself and returns an `EvalError`. Existing calls
  such as `opts.external("f", 2, …)` compile unchanged.

## [0.5.2]

### Fixed

- `["all", …]` and `["any", …]` rejected more than two arguments with
  `Expected 0 to 2 arguments, but found N instead.`, a regression introduced
  in 0.5.0. Both carry two overloads upstream — `(boolean, boolean)` and
  varargs — and deriving the argument ceiling from that table used
  `Option`'s own ordering, where `None` sorts *below* every `Some`, so the
  varargs overload lost to the two-argument one. Four-way `all` is ordinary in
  real styles (`["all", ["==", …], ["==", …], ["!=", …], ["!=", …]]`), and
  every vendored fixture happens to use exactly two arguments, so nothing in
  the conformance suite covered it. It is covered now.

## [0.5.1]

Finishes the 0.5.0 collation. Four behaviours had been left explicitly
unverified against upstream because the conformance fixtures do not reach
them; checking them found two real divergences, both in `number-format`, and
confirmed the other two were already correct.

### Fixed

- `number-format` rendered non-finite numbers with Rust's spelling: `"inf"`,
  `"-inf"`. ECMA-402 resolves these before any digit handling, so
  `Intl.NumberFormat` gives `"∞"`, `"-∞"` and `"NaN"` — and `"$∞"` under
  `style: "currency"`. Reachable from a style with
  `["number-format", ["/", 1, 0], {}]`.
- `number-format` placed a minus sign inside the currency symbol, so a
  negative amount formatted as `"$-1.00"` instead of `"-$1.00"`. Negative
  money is an ordinary input.
- `number-format` of `-0` produced `"0"` rather than `"-0"`.
- `number-format` silently clamped `min-fraction-digits` up to
  `max-fraction-digits` when the first exceeded the second. ECMA-402
  §16.1.2 throws a `RangeError` for that combination, and §9.2.13 also
  rejects a fraction-digit option outside `0..=100`. Both now produce an
  error whose text matches upstream's, as
  `EvalErrorKind::NumberFormatDigits`; for a constant expression it surfaces
  at compile time, as it does upstream. This also closes an unbounded
  allocation: `{"min-fraction-digits": 1000000}` used to build a one-megabyte
  string from a single style value.

### Added

- `EvalErrorKind::NumberFormatDigits { option, value }`. The enum is
  `#[non_exhaustive]`, so this is additive.

### Changed

- Comments asserting upstream behaviour in `is-supported-script` and in the
  collator's sensitivity mapping were checked and are correct; they now cite
  the upstream file and line, the ECMA-402 section, and the `icu_collator`
  documentation rather than stating it unsourced. The README gains an
  implementation note recording what the collator cannot reproduce —
  ICU4X ships no search collations, so upstream's `usage: "search"` is
  approximated (`de` via `de-u-co-phonebk`, which agrees with Node's search
  collation across all 84 German comparisons tried, with two Japanese
  kana-width pairs remaining) — and that `number-format` always formats as
  `en-US`.

## [0.5.0]

A correctness pass that collated the crate against maplibre-style-spec at the
pinned commit `ef522e45`. Most of it is invisible — the conformance suite
already passed and still does (563/563) — but it changes a lot of error text,
tightens `Color::parse`, and adds and removes `ParseErrorKind` variants, so
read *Changed* before upgrading.

### Added

- `convert::try_convert_function` — the `Result`-returning sibling of
  `convert_function`, with a new `convert::ConvertError` type, for callers that
  want an unknown legacy function `type` reported rather than embedded in the
  output.
- The 17 `filter-*` operators (`filter-has`, `filter-has-id`, `filter-type-in`,
  `filter-id-in`, `filter-in-small`, `filter-in-large`, and the `==`/`!=`-style
  comparison forms) now parse. These are the internal forms
  `convert_legacy_filter` emits, and upstream has them in its registry; this
  crate accepted the output of its own converter only by accident before.
- `ParseErrorKind::NestingTooDeep { max }`, reported when a JSON expression
  nests deeper than 100 levels.
- `FormatSection`, `FormatArg` and `SimpleGeom` are exported. They are the
  payloads of the public variants `Value::Formatted`, `Expr::Format` and
  `Expr::Distance`, so until now a downstream crate could pattern-match
  through those variants but could not name their contents or build a
  non-empty one.
- `is_subtype` is exported alongside `Type`, whose canonical subtype predicate
  it is, and `format_number` alongside `Value`, so callers can reproduce the
  exact number rendering that `to-string`, `concat` and the error messages
  use.

### Changed

- **Breaking:** `ParseErrorKind`, `EvalErrorKind`, `FilterError`,
  `ParseFilterError` and `MigrateError` are now `#[non_exhaustive]`, so a
  `match` on any of them needs a `_` arm. In exchange, adding a variant stops
  being a breaking change — which is why this release, the one that has to
  break them anyway, is where it happens.
- **Breaking:** `ParseErrorKind` gained and lost variants, so an exhaustive
  `match` on it will no longer compile.
  Removed: `MatchAtLeast4`, `ExpectedOddArgsLet`, `FormatAtLeastOne`,
  `CollatorOneArg`, `NumberFormatTwoArgs`, `ExpectedNArgs`, `StepStopNumber`,
  `InterpolationStopNumber`, `InterpolationTypeName`, `LetBindingNameString`,
  `VarBindingName`. Added: `NestingTooDeep`, `ExpectedTwoArguments`,
  `ExpectedAtLeastOneArgument`, `ExpectedAtLeastArgs`, `LetAtLeast3`,
  `ExpectedString`, `VarOneStringLiteral`, `StopInputLiteral`. Several
  hand-written, single-operator variants were replaced by general ones, which
  is why the count drops. `ExpectedEvenArgs` also lost its `op` field.
- **Breaking:** `EvalErrorKind::ArrayIndexOutOfBounds`'s `max` field changes
  from `usize` to `i64`. It is the last valid index, which is `-1` for an empty
  array — the old type could not represent that and saturated at `0`, so an
  out-of-range `at` on an empty array reported `0 > 0`.
- **Breaking:** many error *messages* changed, to match upstream word for word.
  The conformance fixtures only pin the wording they happen to reach; outside
  that corpus the port had grown its own phrasing. `let`, `var`, `step`,
  `interpolate`, `match`, `case`, `format`, `collator`, `number-format`, `at`
  and every compound-expression signature error are affected, and some now
  report at the key of the offending stop instead of `""`. Code that matches on
  error strings will break; match on `ParseErrorKind` / `EvalErrorKind` instead.
- **Breaking:** `Color::parse` is now a port of upstream's `parse_css_color.ts`
  and follows CSS Color 4, so some inputs it used to accept are rejected —
  notably mixed argument formats such as `rgb(50%, 0, 0)` (percentages mixed
  with plain numbers) and `rgb(0, 0 255)` (commas mixed with spaces). Channels
  and alpha that are merely out of range are clamped rather than rejected, so
  `rgb(300, 0, 0)` is opaque red. In the other direction the named-color table
  grows from 18 names to the full CSS list of 148, so many colors that used to
  fail now parse.
- **Breaking:** `["format", null]` is rejected. Upstream guards its first
  argument with `!Array.isArray(a) && typeof a === 'object'`, and JavaScript
  reports `typeof null` as `"object"`, so a leading `null` fails there just
  like a bare options object does.
- **Breaking:** `config`, `measure-light`, `raster-value` and
  `sky-radial-progress` are not in upstream's registry and are no longer
  accepted.
- **Breaking:** `convert_function` now returns `["error", msg]` for an unknown
  function `type`. It previously fell through to exponential interpolation, so
  a typo in a style silently changed the rendering instead of failing. Use
  `try_convert_function` for a `Result`.
- **Breaking:** `ExprFn`, `Macro` and `ExternalFn` are no longer exported. No
  public function ever took or returned one — `Options::macro_def`,
  `Options::expr_fn` and `Options::external` take their parts directly, and
  `Options`'s fields are private — so the three were reachable only through
  `Options`'s `Debug` output. Register extensions through `Options` as before.
- **Breaking:** `to-number` now skips an argument that converts to `NaN` and
  tries the next, as upstream does, instead of returning the `NaN`. String
  conversion follows ECMA-262 `Number(string)` rather than Rust's
  `str::parse`, so the Rust-only spellings `nan`, `inf` and `infinity` are no
  longer accepted while `0x1f`, `0b101` and `0o17` now are. Arrays convert
  through `Array.prototype.toString`, so `["to-number", ["literal", [7]]]`
  yields `7` where it used to fail.
- `format_number` and `Display for Value` now produce exactly JavaScript's
  `String(n)` — a port of ECMA-262 6.1.6.1.20. This changes the output for
  `-0`, for values near the exponent-notation thresholds, and for infinities.
- `interpolate` validates its interpolation type before its argument count, as
  upstream does, so a call that is wrong in both ways now reports the type.
- `serde` and `thiserror` are no longer dependencies. Nothing referenced
  either — the error types are hand-written `Display` + `std::error::Error`
  impls, and no type here derives `Serialize` or `Deserialize` — but the
  `serde` dependency's `derive` feature pulled `serde_derive`, `syn` and
  `quote` into every build. `serde_json` is unaffected and remains the crate's
  only non-optional dependency, which is what the README has claimed all
  along. The dependency graph drops from 42 crates to 38 with `collator`, and
  from 14 to 6 without it. No API change.
- `tests/refresh_fixtures.sh` and `scripts/refresh_reference.sh` now rewrite
  the pinned commit in the matching `ATTRIBUTION.md` themselves, instead of
  printing a reminder for someone to do it by hand. This crate's correctness
  is defined as agreeing with upstream *at the pinned commit*, so a pin that
  drifts away from the vendored snapshot undermines every later comparison.

### Fixed

- Three panics on ordinary input:
  - An empty legacy filter array (`"filter": []`) panicked in
    `convert_legacy_filter`, reachable from `migrate` on any style that has
    one. It now converts to `true`, as upstream does.
  - An empty `LineString` or polygon ring panicked in `distance`. It now
    yields a non-finite result instead, matching upstream.
  - A multi-byte character in a hex color (`#aaaaa✗`) panicked in
    `Color::parse` by slicing on a non-char boundary. It is now rejected.
- `Options` is documented as shareable across threads, but the macro-expansion
  depth counter lived in the shared registry as a non-atomic
  read-modify-write, so two threads parsing through one `&Options` corrupted
  each other's budget. A non-recursive one-level macro failed roughly 1 parse
  in 6, and a genuinely recursive one overran the 64-level guard into a stack
  overflow. Per-parse state is now threaded through the recursion.
- Deeply nested JSON aborted the process with a stack overflow instead of
  returning an error; `parse` now has a nesting limit of 100 and returns
  `Err(NestingTooDeep)`.
- `Feature::geometry`'s documentation said "global tile coordinates". It is
  lng/lat degrees — both `within` and `distance` project it themselves — and
  feeding it tile coordinates returns `false` or `inf` rather than erroring, so
  the wrong doc produced silently wrong results. The doc also mentioned only
  `within`.
- `["coalesce", ["get", "c"], "red"]` was rejected on a color property. The
  assert/coerce table existed in two copies that had drifted, and one of them
  tolerated a type mismatch only when the actual type was `value`; this idiom
  is accepted upstream.
- `to-string` printed every integer in `[2^63, 1e21)` as
  `9223372036854775807`, because the conversion went through `i64`.
- `to-string` escaped strings with Rust's `Debug`, emitting Rust-syntax escapes
  where JSON requires its own.
- `to-color` and the implicit color coercion never range-checked their
  channels, although `rgb` a few hundred lines away did.
- `index-of` measured its `from` argument in code points; upstream indexes in
  UTF-16.
- An ordered comparison with a `collator` returned a value where upstream
  throws, and array operands were rendered with their item type and length
  where upstream prints the bare kind.
- A legacy filter leaf under `!` or `case` was neither converted nor rejected,
  so the filter compared the property *name* against the value.
- `convert_literal` missed `typeof null === 'object'`; `convert_token_string`
  now matches upstream's `/{([^{}]+)}/g`; `in` deduplicates numbers by value,
  so `1` and `1.0` no longer produce duplicate `match` labels; `resolvedImage`
  properties regained upstream's empty-string fallback; and `distance` seeds
  its running minimum as upstream does, so a one-point line is finite.

## [0.4.0]

### Added

- `migrate`: a port of maplibre-style-spec's `migrate` for `version: 8`
  styles. It walks every layer and converts legacy filters, function objects
  and `{token}` strings to expressions, and normalises non-standard `hsl()`
  colors, using a pruned snapshot of the style-spec reference embedded in the
  crate (`src/reference/v8.json`, refreshed with
  `scripts/refresh_reference.sh`). With the property spec in hand the
  conversion matches the reference implementation exactly, where the former
  spec-less conversion in `parse` had to guess (numeric stops came out as
  `step` instead of `interpolate`, tokens were never expanded). The MapLibre demo
  style `globe.json` is vendored and its migration asserted equal to
  `gl-style-migrate`'s output. Also `migrate::migrate_property` for a single
  value, `migrate_with` / `migrate_property_with` for a custom reference,
  `migrate::migrate_colors`, `migrate::reference` / `property_spec`, and
  `convert::convert_token_string` is now public.
- `parse_property(name, value)` / `parse_property_with`: parse one
  layout/paint value the way MapLibre's `createPropertyExpression` reads it —
  legacy function objects and `{token}` strings are converted with the
  property's spec from the embedded reference, then parsed.
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

### Removed

- **Breaking:** `parse` no longer converts a bare legacy function object on
  the fly, and `Options::convert_legacy` is gone. MapLibre never reads a
  function object without its property spec (`createExpression` rejects
  objects outright), and the spec-less guess this crate made could differ
  from MapLibre's reading. A bare object is now the same `Bare objects
  invalid` parse error as upstream; use `parse_property` (one value) or
  `migrate` (a whole style) to read legacy forms with their spec.

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
