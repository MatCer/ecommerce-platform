#!/usr/bin/env bash
set -euo pipefail

die() { echo "stack: $*" >&2; exit 1; }
usage() { die "usage: stack.sh up|sync [service...]|env|e2e <playwright args>|doctor|down <slot 3|4>"; }

cmd=${1:-}; slot=${2:-}
[[ $cmd =~ ^(up|sync|env|e2e|doctor|down)$ && $slot =~ ^[34]$ ]] || usage
shift 2
src=$(git rev-parse --show-toplevel 2>/dev/null) || die "run it from the worktree under test"
project=ecommerce-s$slot
dir=${XDG_CACHE_HOME:-$HOME/.cache}/ecommerce-stack/$project
log=$dir.log
http=${slot}8080 https=${slot}8443

while read -r v; do unset "$v"; done < <(compgen -e | grep -E \
  '^(COMPOSE_|(TEST_|OWNER_)?DATABASE_URL$|APP_|PG_|MEILI_|MINIO_|MAILPIT_|STRIPE_|S3_|HTTPS?_PORT$|HOST_BIND$|E2E_|PLAYWRIGHT_|CI$)')

owner_check() {
  [[ ! -f $dir.owner || $(<"$dir.owner") == "$src" ]] ||
    die "slot $slot belongs to $(<"$dir.owner"); use the other slot or ask its owner"
}

copy() {
  mkdir -p "$dir"
  rsync -a --delete --chmod=go+rX \
    --exclude .git --exclude node_modules --exclude target --exclude .artifacts --exclude dist \
    --exclude .astro --exclude .wrangler --exclude .env --exclude .env.worktree.local \
    --exclude .cargo/worktree.toml --exclude test-results --exclude .verify --exclude .claude \
    "$src/" "$dir/"
  sed -e "s/^COMPOSE_PROJECT_NAME=.*/COMPOSE_PROJECT_NAME=$project/" \
    -e "s/^HTTP_PORT=.*/HTTP_PORT=$http/" -e "s/^HTTPS_PORT=.*/HTTPS_PORT=$https/" \
    -e "s/^PG_PORT=.*/PG_PORT=${slot}5432/" -e "s/^MEILI_PORT=.*/MEILI_PORT=${slot}7700/" \
    -e "s/^MINIO_PORT=.*/MINIO_PORT=${slot}9000/" -e "s/^MINIO_CONSOLE_PORT=.*/MINIO_CONSOLE_PORT=${slot}9001/" \
    -e "s/^MAILPIT_SMTP_PORT=.*/MAILPIT_SMTP_PORT=${slot}1025/" -e "s/^MAILPIT_UI_PORT=.*/MAILPIT_UI_PORT=${slot}8025/" \
    -e "s/^STRIPE_MOCK_PORT=.*/STRIPE_MOCK_PORT=${slot}2121/" "$dir/.env.example" >"$dir/.env"
  chmod 600 "$dir/.env"
  echo "$(git -C "$src" rev-parse --short HEAD)$([[ -z $(git -C "$src" status --porcelain) ]] || echo ' + uncommitted changes')" >"$dir.rev"
}

compose() { (cd "$dir" && COMPOSE_PROFILES=full docker compose -p "$project" --env-file "$dir/.env" "$@"); }

case $cmd in
  up)
    owner_check
    for p in $http $https ${slot}5432 ${slot}7700 ${slot}9000 ${slot}9001 ${slot}1025 ${slot}8025 ${slot}2121; do
      ! (: <"/dev/tcp/127.0.0.1/$p") 2>/dev/null || [[ -f $dir.owner ]] || die "port $p is taken"
    done
    mkdir -p "$(dirname "$dir")"
    echo "$src" >"$dir.owner"
    copy
    cd "$dir"
    echo "stack: building $project, log $log (cold: 10+ min)"
    pnpm install --frozen-lockfile --silent
    make up >>"$log" 2>&1 || die "make up failed, see $log"
    make seed >>"$log" 2>&1 || die "make seed failed, see $log"
    bash scripts/seed-m2-perf-review.sh >>"$log" 2>&1 || die "review seed failed, see $log"
    make e2e args="--project chromium-setup --no-deps" >>"$log" 2>&1 || die "e2e fixture setup failed, see $log"
    echo "stack: up, shop http://demo.localhost:$http https://demo.localhost:$https"
    ;;
  sync)
    owner_check
    copy
    cd "$dir"
    if (($#)); then
      compose up -d --build --wait "$@" >>"$log" 2>&1 || die "rebuild of $* failed, see $log"
      echo "stack: rebuilt $*"
    else
      make theme-build >>"$log" 2>&1 || die "theme-build failed, see $log"
      echo "stack: theme and checkout artifacts rebuilt and published"
    fi
    ;;
  env)
    grep -E '^(COMPOSE_PROJECT_NAME|HTTPS?_PORT|PG_PORT|MAILPIT_UI_PORT|E2E_RATE_SECRET)=' "$dir/.env" | sed 's/^/export /'
    ;;
  e2e)
    [[ -f $dir/.env ]] || die "no stack in slot $slot; run up first"
    set -a
    # shellcheck disable=SC1091
    source "$dir/.env"
    set +a
    cd "$src"
    exec pnpm --filter @platform/e2e exec playwright test "$@"
    ;;
  doctor)
    [[ -f $dir.owner ]] || die "slot $slot is free"
    echo "owner:   $(<"$dir.owner")$([[ $(<"$dir.owner") == "$src" ]] || echo '  (NOT this worktree: do not drive it)')"
    echo "copied:  $(<"$dir.rev") at the last up/sync; worktree HEAD is now $(git -C "$src" rev-parse --short HEAD)"
    compose ps --format '{{.Service}}\t{{.State}}\t{{.Health}}' | sort
    for u in "http://api.localhost:$http/readyz" "http://demo.localhost:$http/" "https://demo.localhost:$https/"; do
      printf '%s %s\n' "$(curl -sk -o /dev/null -w '%{http_code}' --max-time 10 "$u" || true)" "$u"
    done
    ;;
  down)
    owner_check
    if [[ -f $dir/.env ]]; then
      compose stop --timeout 5 theme-builder theme-sandbox-proxy >/dev/null 2>&1 || true
      docker ps -aq --filter "label=platform.theme-sandbox=$project" | xargs -r docker rm -f >/dev/null 2>&1 || true
      compose down -v --remove-orphans --timeout 5 >>"$log" 2>&1 || true
      docker images --format '{{.Repository}}:{{.Tag}}' --filter "reference=$project-*:local" |
        xargs -r docker rmi >/dev/null 2>&1 || true
    fi
    rm -rf "$dir" "$dir.owner" "$dir.rev"
    echo "stack: $project down (log kept at $log)"
    ;;
esac
