#!/usr/bin/env bash
# Builds the default theme and the checkout app and packs them as content-addressed artifacts
# (spec A22) into $ARTIFACT_ROOT (default .artifacts), updating the `default-theme` and
# `checkout` channel pointers. Re-running with unchanged sources is a no-op (same ids): the
# Astro key is fixed (DEFAULT_THEME_ASTRO_KEY), so builds are reproducible.
# Also writes the default theme's source archive (`sources/default-theme.tar.gz`, WP23), which
# tenants fork and reset to. It is deterministic too (sorted, mtime 0, no owner, gzip -n).
set -euo pipefail
cd "$(dirname "$0")/.."
root="${ARTIFACT_ROOT:-.artifacts}"
mkdir -p "$root/sources"
# Astro encrypts server-island props with this key and embeds it in the bundle; a fixed key
# makes the shared default artifact reproducible. Tenant builds get their own key (WP23).
export ASTRO_KEY="${DEFAULT_THEME_ASTRO_KEY:-ZGVmYXVsdC10aGVtZS1sb2NhbC1hc3Ryby1rZXktMDE=}"

pnpm --dir themes/default exec astro build --silent
pnpm --dir apps/checkout exec astro build --silent

node packages/theme-kit/src/cli.ts pack --dist themes/default/dist --kind theme \
  --tokens themes/default/theme.tokens.json --out "$root" --channel default-theme
node packages/theme-kit/src/cli.ts pack --dist apps/checkout/dist --kind checkout \
  --out "$root" --channel checkout

(cd themes/default && tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  --format=gnu -cf - src public theme.tokens.json package.json astro.config.mjs tsconfig.json \
  README.md) | gzip -n -9 > "$root/sources/default-theme.tar.gz.tmp"
mv "$root/sources/default-theme.tar.gz.tmp" "$root/sources/default-theme.tar.gz"
