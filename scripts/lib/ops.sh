# Shared helpers for backup.sh / restore.sh / backup-drill.sh (source it).
# Runs from the repo root against the compose project of `.env` (docker compose reads .env
# itself; a COMPOSE_PROJECT_NAME/HTTP_PORT exported in the shell wins, as for compose).
# Secrets stay in .env and inside the containers: nothing here reads or prints them.
# shellcheck shell=bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

dotenv() { [[ -f .env ]] && sed -n "s/^$1=//p" .env | tail -1; }
: "${COMPOSE_PROJECT_NAME:=$(dotenv COMPOSE_PROJECT_NAME || true)}"
: "${COMPOSE_PROJECT_NAME:=ecommerce}"
: "${HTTP_PORT:=$(dotenv HTTP_PORT || true)}"
: "${HTTP_PORT:=8080}"
export COMPOSE_PROJECT_NAME HTTP_PORT
export COMPOSE_PROFILES=full

compose=(docker compose)
# shellcheck disable=SC2034 # used by the sourcing scripts
BUCKETS=(public private)
# Public objects are content-addressed image variants (crates/commerce/src/media, IMMUTABLE).
# A filesystem mirror drops object metadata: Content-Type comes back from the extension,
# Cache-Control is re-applied on restore.
# shellcheck disable=SC2034
PUBLIC_CACHE_CONTROL="public, max-age=31536000, immutable"

log() { printf '== %s\n' "$*" >&2; }
die() {
  echo "ERROR: $*" >&2
  exit 1
}

# psql as the postgres superuser over the container's local socket (trust auth, no password).
psql_su() { "${compose[@]}" exec -T postgres psql -v ON_ERROR_STOP=1 -X -qAt -U postgres -d app "$@"; }

# mc_run <host dir> <script>: runs <script> in the MinIO image with alias `local` (root
# credentials from the minio-init service env) and <host dir> mounted at /backup, as the
# calling user so the files on the host stay deletable.
mc_run() {
  "${compose[@]}" run --rm --no-deps -T --user "$(id -u):$(id -g)" \
    -e MC_CONFIG_DIR=/tmp/.mc -v "$1:/backup" --entrypoint /bin/sh minio-init -euc \
    "mc alias set local http://minio:9000 \"\$MINIO_ROOT_USER\" \"\$MINIO_ROOT_PASSWORD\" >/dev/null
$2"
}

require_running() { # require_running <service>...
  local s
  for s in "$@"; do
    [[ -n $("${compose[@]}" ps --status running -q "$s") ]] ||
      die "service '$s' of compose project '$COMPOSE_PROJECT_NAME' is not running (make up)"
  done
}
