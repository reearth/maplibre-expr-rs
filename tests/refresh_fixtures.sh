#!/usr/bin/env bash
# Refresh the vendored maplibre-style-spec expression fixtures.
#
# Usage: tests/refresh_fixtures.sh [git-ref]
#
# Clones the spec at the given ref (default: main), copies the expression test
# suite into tests/fixtures/expression, and updates the pinned commit recorded
# in tests/fixtures/ATTRIBUTION.md.
set -euo pipefail

REF="${1:-main}"
CRATE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$CRATE_DIR/tests/fixtures/expression"
ATTRIBUTION="$CRATE_DIR/tests/fixtures/ATTRIBUTION.md"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Rewrite the `**Pinned commit**` line of an ATTRIBUTION.md in place. This
# crate's correctness is defined as matching the upstream at the pinned
# commit, so a stale pin is a real defect, not a bookkeeping nit -- which is
# why this refuses to continue rather than silently leaving the old value.
# scripts/refresh_reference.sh carries the same helper for its own snapshot;
# the two scripts stay standalone, so the helper is duplicated rather than
# shared.
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

git clone --depth 1 --branch "$REF" \
  https://github.com/maplibre/maplibre-style-spec.git "$TMP/spec" 2>/dev/null \
  || git clone --depth 1 https://github.com/maplibre/maplibre-style-spec.git "$TMP/spec"

COMMIT="$(git -C "$TMP/spec" rev-parse HEAD)"

rm -rf "$DEST"
cp -R "$TMP/spec/test/integration/expression/tests" "$DEST"

update_pinned_commit "$ATTRIBUTION" "$COMMIT"

echo "Vendored $(find "$DEST" -name test.json | wc -l | tr -d ' ') fixtures at commit $COMMIT"
echo "Pinned commit in tests/fixtures/ATTRIBUTION.md updated to $COMMIT"
