# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
