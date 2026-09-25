# Shared helpers for the smoke scripts (source it; needs curl and jq; reads ports from .env).
# shellcheck shell=bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
set -a
# shellcheck disable=SC1091
source .env
set +a

http="http://%s.localhost:${HTTP_PORT:-8080}"
api=$(printf "$http" api)
# Better Auth is served on the admin origin (first-party session cookie, WP5).
auth=$(printf "$http" admin)
mailpit="http://127.0.0.1:${MAILPIT_UI_PORT:-58025}"
compose=(docker compose)
run=$RANDOM$RANDOM

step() { printf '\n== %s\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
expect_status() { # expect_status <want> <curl args...>
  local want=$1
  shift
  local got
  got=$(curl -s -o /tmp/smoke-body.$$ -w '%{http_code}' "$@")
  [[ $got == "$want" ]] || fail "expected $want, got $got: $(cat /tmp/smoke-body.$$)"
  cat /tmp/smoke-body.$$
  rm -f /tmp/smoke-body.$$
}
admin() { "${compose[@]}" exec -T api /usr/local/bin/api admin "$@"; }

# magic_link <email>: the sign-in link from the newest Mailpit message to <email>.
magic_link() {
  local id
  for _ in $(seq 1 30); do
    id=$(curl -s "$mailpit/api/v1/search?query=to:$1" | jq -r '.messages[0].ID // empty')
    if [[ -n $id ]]; then
      curl -s "$mailpit/api/v1/message/$id" | jq -r .Text | grep -o 'http://admin\.localhost[^[:space:]]*/api/auth/magic-link/verify[^[:space:]]*' | head -1
      return
    fi
    sleep 1
  done
  fail "no magic link email for $1"
}

# login <email>: follows the magic link (session cookie), prints a staff JWT.
login() {
  local jar status link
  jar=$(mktemp)
  link=$(magic_link "$1")
  status=$(curl -s -o /dev/null -w '%{http_code}' -c "$jar" "$link")
  [[ $status == 302 ]] || fail "magic link verify for $1 returned $status"
  expect_status 200 -b "$jar" "$auth/api/auth/token" | jq -r .token
  rm -f "$jar"
}
