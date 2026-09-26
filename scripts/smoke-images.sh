#!/usr/bin/env bash
# Smoke-tests built images without the rest of the stack (used by CI, runnable locally):
#   scripts/smoke-images.sh <rust-image> <mocks-image> [<theme-builder-image>] [<auth-image>]
# api: /healthz 200, `api healthcheck` exits 0, /readyz fails closed (503) with no dependencies.
# worker: stays up without a database, exits 0 on SIGTERM. mocks: /healthz 200.
# theme-builder (WP23): /healthz 200 and /readyz 503 without the sandbox proxy; the sandbox
# proxy answers 502 without a Docker socket and 403 for anything outside its policy.
set -euo pipefail
rust_image=$1
mocks_image=$2
builder_image=${3:-}
auth_image=${4:-}
suffix=$$
api=smoke-api-$suffix
worker=smoke-worker-$suffix
mocks=smoke-mocks-$suffix
builder=smoke-builder-$suffix
proxy=smoke-proxy-$suffix
postgres=smoke-postgres-$suffix
auth=smoke-auth-$suffix

cleanup() { docker rm -f "$api" "$worker" "$mocks" "$builder" "$proxy" "$auth" "$postgres" >/dev/null 2>&1 || true; }
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
  docker logs "$auth" 2>&1 | tail -20 >&2 || true
  docker logs "$postgres" 2>&1 | tail -20 >&2 || true
  exit 1
}

curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$api_url/healthz" >/dev/null || fail "api /healthz"
docker exec "$api" /usr/local/bin/api healthcheck || fail "api healthcheck subcommand"
status=$(curl -s -o /dev/null -w '%{http_code}' "$api_url/readyz")
[[ $status == 503 ]] || fail "api /readyz expected 503 without dependencies, got $status"

curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$mocks_url/healthz" >/dev/null || fail "mocks /healthz"

[[ $(docker inspect -f '{{.State.Running}}' "$worker") == true ]] || fail "worker is not running"
# WP12: the worker renders PDFs with the bundled Typst CLI.
docker exec "$worker" /usr/local/bin/typst --version >/dev/null || fail "typst missing in the image"
docker stop -t 10 "$worker" >/dev/null
[[ $(docker inspect -f '{{.State.ExitCode}}' "$worker") == 0 ]] || fail "worker did not exit cleanly"
docker stop -t 10 "$api" >/dev/null
[[ $(docker inspect -f '{{.State.ExitCode}}' "$api") == 0 ]] || fail "api did not exit cleanly"

if [[ -n $auth_image ]]; then
  # Better Auth migrates on boot; a real disposable Postgres proves the image starts and serves
  # HTTP. They share one temporary network namespace, so no Compose project is touched.
  docker run -d --name "$postgres" -p 127.0.0.1::3000 \
    -e POSTGRES_PASSWORD=smoke-local-password postgres:17.11 >/dev/null
  for _ in $(seq 30); do
    docker exec "$postgres" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 1
  done
  docker exec "$postgres" pg_isready -U postgres >/dev/null 2>&1 || fail "Postgres for auth"
  docker run -d --name "$auth" --network "container:$postgres" \
    -e DATABASE_URL=postgres://postgres:smoke-local-password@127.0.0.1:5432/postgres \
    -e BETTER_AUTH_SECRET=smoke-better-auth-secret-0123456789abcdef \
    -e BETTER_AUTH_URL=http://admin.localhost:8080 \
    -e ADMIN_ORIGIN=http://admin.localhost:8080 \
    -e SMTP_URL=smtp://127.0.0.1:1 \
    -e AUTH_INTERNAL_TOKEN=smoke-auth-internal-token-0123456789abcdef \
    "$auth_image" >/dev/null
  auth_url="http://$(docker port "$postgres" 3000/tcp)"
  curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$auth_url/healthz" >/dev/null \
    || fail "auth /healthz with Postgres"
  docker stop -t 10 "$auth" >/dev/null
  [[ $(docker inspect -f '{{.State.ExitCode}}' "$auth") == 0 ]] \
    || fail "auth did not exit cleanly"
  echo "auth image OK: migrated and served /healthz against disposable Postgres"
fi

if [[ -n $builder_image ]]; then
  sandbox_env=(-e SANDBOX_IMAGE="$builder_image" -e SANDBOX_LABEL=smoke -e SANDBOX_VOLUME=smoke_theme-work
    -e SANDBOX_NETWORKS=none,smoke_theme-check,smoke_theme-functional
    -e CHECK_NETWORK=smoke_theme-check -e FUNCTIONAL_NETWORK=smoke_theme-functional
    -e FUNCTIONAL_PROXY_HOST=preview-only)
  docker run -d --name "$builder" -p 127.0.0.1::4020 "${sandbox_env[@]}" \
    -e API_ORIGIN=http://127.0.0.1:1 -e THEME_BUILDER_TOKEN=smoke-builder-token-0123456789abcdef0123 \
    -e DOCKER_PROXY_URL=http://127.0.0.1:1 "$builder_image" >/dev/null
  docker run -d --name "$proxy" -p 127.0.0.1::2375 "${sandbox_env[@]}" \
    -e PORT=2375 -e DOCKER_SOCKET=/nonexistent.sock "$builder_image" node apps/theme-builder/src/proxy.ts >/dev/null
  builder_url="http://$(docker port "$builder" 4020/tcp)"
  proxy_url="http://$(docker port "$proxy" 2375/tcp)"
  curl -fsS --retry 30 --retry-delay 1 --retry-all-errors "$builder_url/healthz" >/dev/null || fail "builder /healthz"
  status=$(curl -s -o /dev/null -w '%{http_code}' "$builder_url/readyz")
  [[ $status == 503 ]] || fail "builder /readyz expected 503 without the sandbox proxy, got $status"
  # 502 is the expected answer (no socket), so wait for any HTTP answer rather than a 2xx.
  for _ in $(seq 30); do
    status=$(curl -s -o /dev/null -w '%{http_code}' "$proxy_url/_ping" || true)
    [[ $status != 000 ]] && break
    sleep 1
  done
  [[ $status == 502 ]] || fail "proxy /_ping expected 502 without a socket, got $status"
  status=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$proxy_url/containers/x/exec")
  [[ $status == 403 ]] || fail "proxy exec expected 403, got $status"
  echo "theme-builder image OK: builder healthz/readyz, sandbox proxy policy"
fi

echo "images OK: api healthz/healthcheck/readyz, worker lifecycle + typst, mocks healthz"
