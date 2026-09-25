#!/usr/bin/env bash
# Fails when the committed OpenAPI snapshot or generated TS clients differ from what the
# current Rust API produces (spec §4: generated clients are committed, CI fails if stale).
set -euo pipefail
cd "$(dirname "$0")/.."

make --no-print-directory openapi

generated=(openapi.json openapi.storefront.json packages/admin-client/src/schema.d.ts packages/storefront-sdk/src/schema.d.ts)
if [[ -n "$(git status --porcelain -- "${generated[@]}")" ]]; then
  echo "OpenAPI clients are stale. Run 'make openapi' and commit the result:" >&2
  git status --short -- "${generated[@]}" >&2
  git --no-pager diff --stat -- "${generated[@]}" >&2
  exit 1
fi
echo "OpenAPI clients are up to date."
