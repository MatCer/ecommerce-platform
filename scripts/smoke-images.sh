#!/usr/bin/env bash
# Smoke-tests built images without the rest of the stack (used by CI, runnable locally):
#   scripts/smoke-images.sh <rust-image> <mocks-image>
# api: /healthz 200, `api healthcheck` exits 0, /readyz fails closed (503) with no dependencies.
# worker: stays up without a database, exits 0 on SIGTERM. mocks: /healthz 200.
set -euo pipefail
rust_image=$1
mocks_image=$2
suffix=$$
api=smoke-api-$suffix
worker=smoke-worker-$suffix
mocks=smoke-mocks-$suffix

cleanup() { docker rm -f "$api" "$worker" "$mocks" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Nothing listens on port 1: dependencies are down, which the processes must tolerate.
dead_deps=(
  -e DATABASE_URL=postgres://nobody:nothing@127.0.0.1:1/none
  -e MEILI_URL=http://127.0.0.1:1 -e MEILI_SEARCH_KEY=smoke -e MEILI_ADMIN_KEY=smoke
  -e S3_ENDPOINT=http://127.0.0.1:1
  -e S3_ACCESS_KEY_ID=smoke -e S3_SECRET_ACCESS_KEY=smoke
  -e S3_BUCKET_PUBLIC=public -e S3_BUCKET_PRIVATE=private
  -e AUTH_JWKS_URL=http://127.0.0.1:1/jwks -e ADMIN_ORIGIN=http://admin.localhost:8080
  -e INTERNAL_API_TOKEN=smoke-internal-api-token-0123456789abcdef
)

docker run -d --name "$api" -p 127.0.0.1::8000 "${dead_deps[@]}" "$rust_image" /usr/local/bin/api >/dev/null
docker run -d --name "$worker" "${dead_deps[@]}" "$rust_image" /usr/local/bin/worker >/dev/null
docker run -d --name "$mocks" -p 127.0.0.1::4010 "$mocks_image" >/dev/null

api_url="http://$(docker port "$api" 8000/tcp)"
mocks_url="http://$(docker port "$mocks" 4010/tcp)"

fail() {
  echo "smoke test failed: $*" >&2
  docker logs "$api" 2>&1 | tail -20 >&2 || true
  docker logs "$worker" 2>&1 | tail -20 >&2 || true
  docker logs "$mocks" 2>&1 | tail -20 >&2 || true
  exit 1
}

curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$api_url/healthz" >/dev/null || fail "api /healthz"
docker exec "$api" /usr/local/bin/api healthcheck || fail "api healthcheck subcommand"
status=$(curl -s -o /dev/null -w '%{http_code}' "$api_url/readyz")
[[ $status == 503 ]] || fail "api /readyz expected 503 without dependencies, got $status"

curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$mocks_url/healthz" >/dev/null || fail "mocks /healthz"

[[ $(docker inspect -f '{{.State.Running}}' "$worker") == true ]] || fail "worker is not running"
docker stop -t 10 "$worker" >/dev/null
[[ $(docker inspect -f '{{.State.ExitCode}}' "$worker") == 0 ]] || fail "worker did not exit cleanly"
docker stop -t 10 "$api" >/dev/null
[[ $(docker inspect -f '{{.State.ExitCode}}' "$api") == 0 ]] || fail "api did not exit cleanly"

echo "images OK: api healthz/healthcheck/readyz, worker lifecycle, mocks healthz"
