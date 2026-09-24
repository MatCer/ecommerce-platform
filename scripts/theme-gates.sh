#!/usr/bin/env bash
# Runs the theme gates of spec §12.3 against a theme directory and a running edge:
# contract lint → astro check → astro build → pack + publish (channel) → purge → budget/axe →
# Playwright smoke. BASE should be HTTPS/h2 (see docker/caddy/Caddyfile); the smoke runs the
# checkout handoff, which follows the edge's canonical scheme (http locally).
#
#   ARTIFACT_ROOT=/tmp/artifacts EDGE_ADMIN=http://127.0.0.1:8688 BASE=https://demo.localhost:8691 \
#   SMOKE_BASE=http://demo.localhost:8690 \
#     scripts/theme-gates.sh /path/to/theme-copy
#
# The edge must serve ARTIFACT_ROOT with sites pointing at "@default-theme".
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
theme="$(cd "${1:?theme dir}" && pwd)"
: "${ARTIFACT_ROOT:?}" "${EDGE_ADMIN:?}" "${BASE:?}"
kit="$repo/packages/theme-kit/src"

step() { printf '\n== %s\n' "$1"; }

step "contract lint"
node "$kit/cli.ts" lint "$theme" --reference "$repo/themes/default/package.json"
step "astro check"
(cd "$theme" && node_modules/.bin/astro check --minimumSeverity error | tail -3)
step "astro build"
(cd "$theme" && node_modules/.bin/astro build --silent)
step "pack + publish"
node "$kit/cli.ts" pack --dist "$theme/dist" --kind theme --tokens "$theme/theme.tokens.json" \
  --out "$ARTIFACT_ROOT" --channel default-theme
curl -fsS -X POST "$EDGE_ADMIN/_edge/purge" -H "authorization: Bearer ${EDGE_PURGE_TOKEN:-local-edge-purge-token-0123456789}" \
  -H 'content-type: application/json' -d '{"all":true}'
step "budget + axe"
node "$kit/measure.ts" --base "$BASE" --pages "${PAGES:-/,/c/trika,/p/tricko-basic}" --runs "${RUNS:-1}" \
  --out "${REPORT:-$theme/.perf/report.json}"
step "playwright smoke"
node "$kit/smoke.ts" --base "${SMOKE_BASE:-$BASE}" --shots "$(dirname "${REPORT:-$theme/.perf/report.json}")/shots"
