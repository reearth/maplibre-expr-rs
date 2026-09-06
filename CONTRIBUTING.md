# Contributing to maplibre-expr-rs

Thanks for your interest in contributing! This crate is a pure-Rust parser and
evaluator for the [MapLibre GL style expression language][spec]. Its goal is to
be **byte-exact compatible** with the reference implementation in
[`maplibre-gl-js`][gl-js], including error messages and error locations.

## Ways to contribute

- **Report a bug.** Open an issue with a minimal expression JSON, the observed
  behavior, and the behavior you get from `maplibre-gl-js` for the same input.
- **Request a feature.** Open an issue describing the use case. Note that any
  new operator or behavior must ultimately be justifiable by the upstream
  [style spec][spec] — this crate does not add non-standard operators on its own
  (users can register their own via `Options::macro_def` / `Options::function`
  / `Options::native`).
- **Send a pull request.** See below.

## Development setup

Requirements:

- Rust (see `rust-version` in `Cargo.toml` for the current MSRV)

Common commands:

```bash
cargo build
cargo test              # runs the full 563-fixture conformance suite plus unit tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

The conformance suite lives under `tests/fixtures/` and is derived from the
upstream MapLibre expression test data. It is the primary correctness gate:
**any change that breaks a fixture must include a clear justification.**

## Pull request guidelines

- Keep PRs focused — one logical change per PR.
- Add tests for new behavior. If the change affects expression semantics, add
  or update a fixture under `tests/fixtures/` and make sure the fixture matches
  the behavior of `maplibre-gl-js` for the same input.
- Update `CHANGELOG.md` under the `## main` (or unreleased) section.
- If you touch public API, update the doc comments and the README examples.
- Run `cargo fmt` and `cargo clippy` before pushing.

## AI-assisted contributions

This project follows the [MapLibre AI policy][ai-policy]. Fully AI-generated
issues or pull requests are not accepted; AI-assisted contributions are fine
provided you review, understand, and take responsibility for the change.

## License

By contributing, you agree that your contributions will be dual-licensed under
the MIT license and the Apache License, Version 2.0, matching this repository's
license.

[spec]: https://maplibre.org/maplibre-style-spec/expressions/
[gl-js]: https://github.com/maplibre/maplibre-gl-js
[ai-policy]: https://github.com/maplibre/maplibre/blob/main/AI_POLICY.md
