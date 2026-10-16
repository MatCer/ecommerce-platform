#!/usr/bin/env bash
set -euo pipefail

die() { echo "worktree: $*" >&2; exit 1; }

cargo_config() {
  [[ -d $1 ]] || die "no directory $1"
  local wrapper='rustc-wrapper = "sccache"'
  if ! command -v sccache >/dev/null; then
    echo "worktree: sccache not found, building without a compile cache (README, Prerequisites)" >&2
    wrapper=''
  fi
  mkdir -p "$1/.cargo"
  cat >"$1/.cargo/worktree.toml" <<TOML
# Written by scripts/worktree.sh; .cargo/config.toml includes it. sccache cannot cache
# incremental builds, and incremental data is the bulk of a worktree's target/.
[build]
incremental = false
$wrapper

[env]
SCCACHE_CACHE_SIZE = "30G"
TOML
}

# Before the git checks: it also prepares non-git copies (git archive, scripts/bench-rust.sh).
if [[ ${1:-} == cargo-config ]]; then
  [[ $# -le 2 ]] || die "usage: scripts/worktree.sh cargo-config [dir]"
  cargo_config "${2:-.}"
  exit
fi

cd "$(git rev-parse --show-toplevel)"

ROOT=$(pwd)
MAIN=$(git worktree list --porcelain | sed -n '1s/^worktree //p')
MAIN_ENV=$MAIN/.env

env_value() { [[ ! -f $2 ]] || sed -n "s/^$1=//p" "$2" | tail -1; }
worktrees() { git worktree list --porcelain | sed -n 's/^worktree //p'; }

db_name() {
  local slug
  slug=$(basename "$1" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9' '_' | cut -c1-30)
  echo "app_wt_${slug%_}_$(printf %s "$1" | cksum | cut -d' ' -f1)"
}

worktree_db() {
  local db
  db=$(env_value APP_DB "$1/.env.worktree.local")
  db=${db:-$(db_name "$1")}
  [[ $db =~ ^app_wt_[a-z0-9_]+$ ]] || die "unexpected database name '$db' for $1"
  echo "$db"
}

psql_owner() {
  local pw port
  pw=$(env_value APP_OWNER_PASSWORD "$MAIN_ENV")
  port=$(env_value PG_PORT "$MAIN_ENV")
  psql "postgres://app_owner:${pw:-app-owner-local}@localhost:${port:-55432}/postgres" \
    -v ON_ERROR_STOP=1 -v owner_checkout="$MAIN" -X -qAt "$@"
}

drop() {
  [[ $1 =~ ^app_wt_[a-z0-9_]+$ ]] || die "refusing to drop '$1'"
  psql_owner -c "DROP DATABASE IF EXISTS \"$1\" WITH (FORCE)"
}

pick_port() {
  local taken=" " dir port
  while read -r dir; do taken+="$(env_value API_PORT "$dir/.env.worktree.local") "; done < <(worktrees)
  for ((port = 28000; port < 28200; port++)); do
    [[ $taken == *" $port "* ]] && continue
    (: <"/dev/tcp/127.0.0.1/$port") 2>/dev/null || { echo "$port"; return; }
  done
  die "no free port in 28000-28199"
}

setup() {
  [[ $ROOT != "$MAIN" ]] || die "this is the main checkout; it runs the dev stack itself"
  [[ -f $MAIN_ENV ]] || die "no $MAIN_ENV: run 'make dev-infra' (or 'make up') in $MAIN first"
  command -v sqlx >/dev/null || die "sqlx-cli not found (README, Prerequisites)"
  local own main_project
  own=$(env_value COMPOSE_PROJECT_NAME .env)
  main_project=$(env_value COMPOSE_PROJECT_NAME "$MAIN_ENV")
  [[ ! -e .env || ${own:-ecommerce} == "${main_project:-ecommerce}" ]] ||
    die "this worktree runs its own stack (COMPOSE_PROJECT_NAME=$own); setup is for worktrees that share $MAIN's"
  [[ -e .env ]] || ln -s "$MAIN_ENV" .env

  local db port
  db=$(worktree_db "$ROOT")
  if [[ -z $(psql_owner -c "SELECT 1 FROM pg_database WHERE datname = '$db'") ]]; then
    psql_owner <<SQL
CREATE DATABASE "$db" OWNER app_owner;
REVOKE ALL ON DATABASE "$db" FROM PUBLIC;
GRANT CONNECT, TEMPORARY ON DATABASE "$db" TO app_runtime;
COMMENT ON DATABASE "$db" IS :'owner_checkout';
SQL
  fi

  exec 9>"$(git rev-parse --path-format=absolute --git-common-dir)/worktree-port.lock"
  flock 9
  port=$(env_value API_PORT .env.worktree.local)
  if command -v wtdev >/dev/null; then port=$(wtdev port); elif [[ -z $port ]]; then port=$(pick_port); fi
  printf '# Written by scripts/worktree.sh; the Makefile reads it after .env.\nAPP_DB=%s\nAPI_PORT=%s\n' \
    "$db" "$port" >.env.worktree.local
  exec 9>&-

  cargo_config "$ROOT"
  pnpm install --frozen-lockfile --prefer-offline
  make migrate
  echo "worktree: ready, database $db, make api-native serves http://127.0.0.1:$port"
}

teardown() {
  [[ $ROOT != "$MAIN" ]] || die "this is the main checkout; it has no worktree database"
  drop "$(worktree_db "$ROOT")"
  rm -f .env.worktree.local .cargo/worktree.toml
  rm -rf target
}

this_clone_databases() {
  psql_owner <<'SQL'
SELECT datname FROM pg_database
WHERE datname LIKE 'app\_wt\_%' AND shobj_description(oid, 'pg_database') = :'owner_checkout';
SQL
}

prune() {
  local keep=" " dir db
  while read -r dir; do keep+="$(worktree_db "$dir") "; done < <(worktrees)
  this_clone_databases | while read -r db; do
    [[ $keep == *" $db "* ]] && continue
    echo "worktree: drop $db"
    [[ ${1:-} == --dry-run ]] || drop "$db"
  done
}

sweep() {
  command -v cargo-sweep >/dev/null || die "cargo-sweep not found (README, Prerequisites)"
  local dir
  while read -r dir; do
    [[ $dir != "$MAIN" && -d $dir/target ]] || continue
    env -u CARGO_TARGET_DIR cargo sweep ${1:+"$1"} --time 14 "$dir"
  done < <(worktrees)
}

case ${1:-} in
  setup) setup ;;
  teardown) teardown ;;
  prune) [[ -z ${2:-} || $2 == --dry-run ]] || die "usage: scripts/worktree.sh prune [--dry-run]"; prune "${2:-}" ;;
  sweep) [[ -z ${2:-} || $2 == --dry-run ]] || die "usage: scripts/worktree.sh sweep [--dry-run]"; sweep "${2:-}" ;;
  *) die "usage: scripts/worktree.sh setup|teardown|prune [--dry-run]|sweep [--dry-run]|cargo-config [dir]" ;;
esac
