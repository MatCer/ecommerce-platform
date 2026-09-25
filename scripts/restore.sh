#!/usr/bin/env bash
# Local restore (spec A29, docs/runbook.md §7): `make restore BACKUP=backups/<ts>` or
#   scripts/restore.sh [--no-start] backups/<ts>
# Meant for a fresh stack (`docker compose down -v && make up`), whose init-roles.sh has created
# the roles; works on a used stack too (database `app` is replaced as a whole, extra bucket
# objects stay).
#
# Database: pg_restore --create --clean as the postgres superuser drops and recreates database
# `app` from the archive, which carries the database ACL and `ALTER ROLE ... IN DATABASE`
# settings too. Object-by-object --clean into the migrated database fails on partitioned tables
# (a partition's inherited primary key cannot be dropped). Without --no-owner, owners (app_owner;
# auth_service for schema `auth`), grants to app_runtime, RLS policies, SECURITY DEFINER
# functions and _sqlx_migrations come back exactly as dumped. Roles are not in a pg_dump:
# init-roles.sh creates them. DROP DATABASE cannot run in a transaction, so a failed restore
# leaves `app` partial: fix the cause and run the restore again.
#
# --no-start leaves auth/api/worker/edge stopped (to inspect or count the restored data);
# otherwise the stack is started, waited for, and a search rebuild is enqueued for every tenant
# (Meilisearch is not backed up).
source "$(dirname "$0")/lib/ops.sh"

start_services=1
if [[ ${1-} == --no-start ]]; then
  start_services=0
  shift
fi
[[ $# == 1 ]] || die "usage: scripts/restore.sh [--no-start] backups/<timestamp>"
dir=$1
[[ -f $dir/app.dump ]] || die "$dir/app.dump not found"
for b in "${BUCKETS[@]}"; do [[ -d $dir/minio/$b ]] || die "$dir/minio/$b not found"; done
dir=$(cd "$dir" && pwd)
require_running postgres minio
start=$SECONDS

# Everything that holds database connections (auth runs its own migrations on start).
log "stop auth api worker edge"
"${compose[@]}" stop auth api worker edge

log "pg_restore $dir/app.dump -> app"
# Copied in rather than piped: pg_restore can seek in a file.
"${compose[@]}" cp "$dir/app.dump" postgres:/tmp/restore.dump
trap '"${compose[@]}" exec -T postgres rm -f /tmp/restore.dump || true' EXIT
psql_su -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
  WHERE datname = 'app' AND pid <> pg_backend_pid()" >/dev/null
"${compose[@]}" exec -T postgres pg_restore -U postgres -d postgres \
  --create --clean --if-exists --exit-on-error /tmp/restore.dump
psql_su -c 'ANALYZE' # planner statistics are not part of a dump
# Meilisearch is not in the backup: forget the restored index state so the worker recreates
# every index with its settings before the rebuild (otherwise the first incremental job would
# create a bare index that the restored state already counts as configured).
psql_su -c 'TRUNCATE search_indexes, search_product_state' >/dev/null

log "mirror buckets back: ${BUCKETS[*]}"
mc_run "$dir/minio" "
mc mirror --quiet --overwrite --attr 'Cache-Control=$PUBLIC_CACHE_CONTROL' /backup/public local/public >/dev/null
mc mirror --quiet --overwrite /backup/private local/private >/dev/null"
log "data restored in $((SECONDS - start)) s"

if ((start_services == 0)); then
  log "services left stopped; start with: COMPOSE_PROFILES=full docker compose up -d --wait && make admin args=reindex"
  exit 0
fi

log "start the stack and wait until healthy"
"${compose[@]}" up -d --wait
log "enqueue a search rebuild for every tenant"
"${compose[@]}" exec -T api /usr/local/bin/api admin reindex
log "restore done in $((SECONDS - start)) s (search catches up in the background)"
