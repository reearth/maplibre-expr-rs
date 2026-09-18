#!/usr/bin/env bash
# Refresh the pruned style-spec reference embedded for `migrate`.
#
# Usage: scripts/refresh_reference.sh [git-ref]
#
# Fetches src/reference/v8.json from maplibre-style-spec at the given ref
# (default: main) and writes src/reference/v8.json: one flat map from property
# name to the handful of spec fields the legacy-function conversion needs
# (`type`, `tokens`, `default`, `expression`, `values`, `value`, `length`).
# Lookup is by property name only, first match across the spec's `layout`
# then `paint` section order — exactly how the upstream migrate resolves a
# property's reference (src/visit.ts getPropertyReference). Enum `values` are
# stored as an array in source order so `match` labels come out in the same
# order as the reference implementation.
#
# Also updates the pinned commit recorded in src/reference/ATTRIBUTION.md.
set -euo pipefail

REF="${1:-main}"
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$CRATE_DIR/src/reference/v8.json"
ATTRIBUTION="$CRATE_DIR/src/reference/ATTRIBUTION.md"
URL="https://raw.githubusercontent.com/maplibre/maplibre-style-spec/$REF/src/reference/v8.json"

# Rewrite the `**Pinned commit**` line of an ATTRIBUTION.md in place. This
# crate's correctness is defined as matching the upstream at the pinned
# commit, so a stale pin is a real defect, not a bookkeeping nit -- which is
# why this refuses to continue rather than silently leaving the old value.
# tests/refresh_fixtures.sh carries the same helper for its own snapshot; the
# two scripts stay standalone, so the helper is duplicated rather than shared.
update_pinned_commit() {
  local file="$1" commit="$2" tmp
  case "$commit" in
    [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*) ;;
    *)
      echo "error: refusing to pin a commit that is not a hex sha: '$commit'" >&2
      return 1
      ;;
  esac
  if ! grep -q '^- \*\*Pinned commit\*\*:' "$file"; then
    echo "error: no '- **Pinned commit**:' line in $file" >&2
    return 1
  fi
  tmp="$(mktemp)"
  sed "s|^- \*\*Pinned commit\*\*:.*|- **Pinned commit**: \`$commit\`|" \
    "$file" >"$tmp"
  mv "$tmp" "$file"
}

curl -sfL "$URL" | jq -S '
  . as $ref
  | ($ref.layout + $ref.paint) as $sections
  | reduce $sections[] as $section ({};
      reduce ($ref[$section] | to_entries[]) as $e (.;
        if has($e.key) then . else
          . + { ($e.key): ($e.value
                | { type, tokens, default, expression, value, length,
                    values: (if (.values | type) == "object" then (.values | keys_unsorted) else .values end) }
                | with_entries(select(.value != null))) }
        end))
' > "$DEST"

COMMIT="$(curl -sfL "https://api.github.com/repos/maplibre/maplibre-style-spec/commits/$REF" | jq -r .sha)"

update_pinned_commit "$ATTRIBUTION" "$COMMIT"

echo "Wrote $(jq length "$DEST") property specs to $DEST from $REF ($COMMIT)"
echo "Pinned commit in src/reference/ATTRIBUTION.md updated to $COMMIT"
