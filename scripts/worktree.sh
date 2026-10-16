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
[build]
$wrapper

[env]
SCCACHE_CACHE_SIZE = "30G"
TOML
}

in_checkout() {
  cd "$(git rev-parse --show-toplevel)"
  ROOT=$(pwd)
  MAIN=$(git worktree list --porcelain | sed -n '1s/^worktree //p')
  MAIN_ENV=$MAIN/.env
}

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
  remove_target "$ROOT" "teardown"
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

gb() { awk -v b="$1" 'BEGIN { printf "%.1f GB", b / 1e9 }'; }

size_bytes() { du -sk "$1" | awk '{ print $1 * 1024 }'; }

remove_target() (
  local target=$1/target bytes cleanup gitdir trash
  shopt -s nullglob
  { gitdir=$(git -C "$1" rev-parse --absolute-git-dir) && exec {cleanup}>"$gitdir/target-cleanup.lock"; } ||
    { echo "worktree: cannot take the cleanup lock of $1, skipped" >&2; exit 0; }
  flock -n "$cleanup" || { echo "worktree: another cleanup is running in $1, skipped"; exit 0; }
  trash=("$1"/.target-trash.*)
  if ((${#trash[@]})); then
    echo "worktree: ${3:+would }remove leftover trash of an interrupted cleanup in $1"
    [[ -n ${3:-} ]] || rm -rf "${trash[@]}"
  fi
  [[ -n $2 && -d $target ]] || exit 0
  bytes=$(size_bytes "$target") || { echo "worktree: cannot measure $target, left as is" >&2; exit 0; }
  (
    for lock in "$target"/*/.cargo-lock; do
      [[ -e $lock ]] || continue
      exec {fd}<"$lock"
      flock -n "$fd" || { echo "worktree: a cargo build is running in $1, $target left as is"; exit 0; }
    done
    echo "worktree: $2, ${3:+would }remove $target ($(gb "$bytes"))"
    [[ -n ${3:-} ]] || mv "$target" "$(mktemp -d "$1/.target-trash.XXXXXX")"
  )
  [[ -n ${3:-} ]] || rm -rf -- "$1"/.target-trash.*
)

sweep() {
  local dir recent
  while read -r dir; do
    [[ $dir != "$MAIN" && -d $dir ]] || continue
    remove_target "$dir" "" "$1"
    [[ -d $dir/target ]] || continue
    recent=$(find "$dir/target" -newermt '14 days ago' -print -quit) ||
      { echo "worktree: cannot read all of $dir/target, kept" >&2; continue; }
    [[ -z $recent ]] || continue
    remove_target "$dir" "nothing built for 14 days" "$1"
  done < <(worktrees)
}

tidy() {
  [[ $ROOT != "$MAIN" ]] || die "this is the main checkout; tidy is for linked worktrees"
  remove_target "$ROOT" "" "$1"
  [[ -d target ]] || return 0
  local head pr='' bytes
  head=$(git rev-parse HEAD)
  if command -v gh >/dev/null; then
    # A worktree on a main commit is linked to the PR squash-merged into it, so match the PR head.
    pr=$(timeout 10 gh api "repos/{owner}/{repo}/commits/$head/pulls" \
      --jq "[.[] | select(.merged_at != null and .head.sha == \"$head\")][0].number // empty" 2>/dev/null) ||
      { pr=''; echo "worktree: GitHub did not answer, cannot tell whether HEAD is merged" >&2; }
  fi
  if [[ -n $pr ]]; then
    remove_target "$ROOT" "HEAD is the merged head of PR #$pr" "$1"
  elif ! bytes=$(size_bytes target); then
    echo "worktree: cannot measure target/, kept" >&2
  elif ((bytes > 10 * 10 ** 9)); then
    remove_target "$ROOT" "target/ is over the 10 GB cap" "$1"
  else
    echo "worktree: target/ is $(gb "$bytes"), under the 10 GB cap, kept"
  fi
}

case ${1:-} in
  setup | teardown)
    [[ $# -eq 1 ]] || die "usage: scripts/worktree.sh $1"
    in_checkout
    "$1"
    ;;
  prune | sweep | tidy)
    [[ -z ${2:-} || $2 == --dry-run ]] || die "usage: scripts/worktree.sh $1 [--dry-run]"
    in_checkout
    "$1" "${2:-}"
    ;;
  cargo-config) [[ $# -le 2 ]] || die "usage: scripts/worktree.sh cargo-config [dir]"; cargo_config "${2:-.}" ;;
  *) die "usage: scripts/worktree.sh setup|teardown|prune [--dry-run]|sweep [--dry-run]|tidy [--dry-run]|cargo-config [dir]" ;;
esac
