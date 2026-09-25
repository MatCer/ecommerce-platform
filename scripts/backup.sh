#!/usr/bin/env bash
# Local backup (spec A29, docs/runbook.md §7): `make backup` or scripts/backup.sh [out dir].
# Writes backups/<UTC timestamp>/ with
#   app.dump        pg_dump -Fc of database `app` (all schemas incl. auth, owners, grants, RLS)
#   minio/public    mirror of the public bucket (image variants)
#   minio/private   mirror of the private bucket (originals, theme/checkout artifacts, ...)
#   manifest.txt    timestamp, git commit, compose project, checksums, object counts
# Consistent pair: the writers (api, worker) are stopped while the database and the buckets are
# copied, so no object the dump references can be purged (media.purge jobs) or appear between
# the two copies. Local downtime is a few seconds; prod uses provider PITR + versioned buckets.
# Not included: Meilisearch (rebuilt on restore), Mailpit, the edge artifact cache (refilled).
source "$(dirname "$0")/lib/ops.sh"

ts=$(date -u +%Y%m%dT%H%M%SZ)
dir=${1:-backups/$ts}
[[ -e $dir ]] && die "$dir already exists"
require_running postgres minio
mkdir -p "$dir/minio/public" "$dir/minio/private"
dir=$(cd "$dir" && pwd)
start=$SECONDS

writers=$("${compose[@]}" ps --status running --services | grep -xE 'api|worker' || true)
if [[ -n $writers ]]; then
  # Registered first: a failed or interrupted stop/copy still restarts every writer.
  # shellcheck disable=SC2086 # word splitting intended: one service per word
  trap 'status=$?; "${compose[@]}" start $writers >/dev/null && log "writers restarted"; exit $status' EXIT INT TERM
  log "stopping writers for a consistent copy: $(echo $writers)"
  # shellcheck disable=SC2086
  "${compose[@]}" stop $writers >/dev/null
fi

log "pg_dump app -> $dir/app.dump"
"${compose[@]}" exec -T postgres pg_dump -U postgres -d app -Fc >"$dir/app.dump"
"${compose[@]}" exec -T postgres pg_restore -l <"$dir/app.dump" >/dev/null ||
  die "the dump is not a readable archive"

log "mirror buckets ${BUCKETS[*]} -> $dir/minio"
mc_run "$dir/minio" "for b in ${BUCKETS[*]}; do mc mirror --quiet --overwrite \"local/\$b\" \"/backup/\$b\" >/dev/null; done"

{
  echo "created_at=$ts"
  echo "git_commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compose_project=$COMPOSE_PROJECT_NAME"
  echo "postgres=$(psql_su -c 'SHOW server_version')"
  echo "migrations=$(psql_su -c 'SELECT max(version) FROM _sqlx_migrations')"
  echo "app.dump.sha256=$(sha256sum "$dir/app.dump" | cut -d' ' -f1)"
  echo "app.dump.bytes=$(stat -c %s "$dir/app.dump")"
  for b in "${BUCKETS[@]}"; do
    echo "minio.$b.objects=$(find "$dir/minio/$b" -type f | wc -l)"
  done
} >"$dir/manifest.txt"

log "backup done in $((SECONDS - start)) s: $dir"
cat "$dir/manifest.txt" >&2
echo "$dir"
