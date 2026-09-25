#!/usr/bin/env bash
# Builds the default theme and the checkout app and packs them as content-addressed artifacts
# (spec A22) into $ARTIFACT_ROOT (default .artifacts), updating the `default-theme` and
# `checkout` channel pointers. Re-running with unchanged sources is a no-op (same ids).
set -euo pipefail
cd "$(dirname "$0")/.."
root="${ARTIFACT_ROOT:-.artifacts}"
mkdir -p "$root"

pnpm --dir themes/default exec astro build --silent
pnpm --dir apps/checkout exec astro build --silent

node packages/theme-kit/src/cli.ts pack --dist themes/default/dist --kind theme \
  --tokens themes/default/theme.tokens.json --out "$root" --channel default-theme
node packages/theme-kit/src/cli.ts pack --dist apps/checkout/dist --kind checkout \
  --out "$root" --channel checkout
