# Embedded style-spec reference

`v8.json` is a pruned snapshot of the [MapLibre style specification][repo]
reference (`src/reference/v8.json`), embedded so that `migrate` can resolve
each layout/paint property's spec without any external input. Only the fields
the legacy-function conversion consults are kept; see
`scripts/refresh_reference.sh` for the exact shape.

- **Source**: <https://github.com/maplibre/maplibre-style-spec>
- **Path in source**: `src/reference/v8.json`
- **Pinned commit**: `ef522e45a28e0efafabbebb27197d3440c99fe34`
- **License**: BSD-3-Clause — Copyright (c) 2020, MapLibre contributors

To refresh, re-run `scripts/refresh_reference.sh`.

[repo]: https://github.com/maplibre/maplibre-style-spec
