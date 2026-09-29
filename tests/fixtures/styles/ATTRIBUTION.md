# Vendored demo styles

- `globe.json` is the MapLibre demo style from
  <https://demotiles.maplibre.org/globe.json> (repository:
  <https://github.com/maplibre/demotiles>, BSD-3-Clause, MapLibre
  contributors), fetched on 2026-09-14. It is a real-world v8 style that still
  uses legacy stop functions, `{token}` strings and legacy filters, which makes
  it a good end-to-end check for `migrate`.
- `globe.migrated.json` is the output of the reference implementation's
  `gl-style-migrate` (`@maplibre/maplibre-gl-style-spec` 26.4.2) on that
  file. `tests/migrate.rs` asserts that our `migrate` produces the same style.
