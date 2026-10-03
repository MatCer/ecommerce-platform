#!/usr/bin/env bash
# Runs the theme gates of spec §12.3 against a theme directory and the running stack:
# contract lint → astro check → astro build → pack → publish (upload + activate for every
# tenant following the default, A30; the edge is purged) → budget/axe → Playwright smoke.
# BASE should be HTTPS/h2 (see docker/caddy/Caddyfile); the smoke runs the checkout handoff,
# which follows the edge's canonical scheme (http locally).
#
#   ARTIFACT_ROOT=.artifacts BASE=https://demo.localhost:8681 SMOKE_BASE=http://demo.localhost:8680 \
#     scripts/theme-gates.sh /path/to/theme-copy
#
# PUBLISH overrides the publish command (default: the api CLI in the compose stack).
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
theme="$(cd "${1:?theme dir}" && pwd)"
: "${ARTIFACT_ROOT:?}" "${BASE:?}"
kit="$repo/packages/theme-kit/src"

step() { printf '\n== %s\n' "$1"; }

step "contract lint"
node "$kit/cli.ts" lint "$theme" --reference "$repo/themes/conversion"
step "astro check"
(cd "$theme" && node_modules/.bin/astro check --minimumSeverity error | tail -3)
step "astro build"
(cd "$theme" && node_modules/.bin/astro build --silent)
step "pack + publish"
id=$(node "$kit/cli.ts" pack --dist "$theme/dist" --kind theme --tokens "$theme/theme.tokens.json" \
  --out "$ARTIFACT_ROOT" | tail -1)
node "$kit/cli.ts" verify --root "$ARTIFACT_ROOT" "$id"
root_abs="$(cd "$ARTIFACT_ROOT" && pwd)"
${PUBLISH:-docker compose run --rm --no-deps -v "$root_abs:/artifacts:ro" api \
  /usr/local/bin/api admin publish-artifacts --root /artifacts --theme} "$id"
step "budget + axe"
node "$kit/measure.ts" --base "$BASE" --pages "${PAGES:-/,/c/trika,/p/tricko-basic}" --runs "${RUNS:-1}" \
  --out "${REPORT:-$theme/.perf/report.json}"
step "playwright smoke"
node "$kit/smoke.ts" --base "${SMOKE_BASE:-$BASE}" --shots "$(dirname "${REPORT:-$theme/.perf/report.json}")/shots"
