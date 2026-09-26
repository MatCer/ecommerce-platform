#!/usr/bin/env bash
# pnpm verify-merge <PR#> [--full]
#
# The only place tests run (CI does lint + typecheck). Checks out the PR head in a temporary
# worktree, runs the unit tests and e2e specs for the areas the PR touches (everything with
# --full or when shared code changed) against a throwaway compose stack, posts the result as
# the `local-verify` commit status on the PR head and, on success, squash-merges the PR.
#
# The stack is its own compose project (`ecommerce-verify`, own ports and volumes); it never
# touches the dev stack (`ecommerce`) and is removed with its volumes on any exit.
# Spec mapping: scripts/verify-areas.txt. Runbook: docs/runbook.md ("Verify and merge").
set -euo pipefail

usage() { echo "usage: pnpm verify-merge <PR#> [--full]" >&2; exit 2; }
die() { printf '\033[31mverify-merge: %s\033[0m\n' "$*" >&2; exit 1; }
say() { printf '\n\033[1m== %s\033[0m (%ss)\n' "$*" "$SECONDS" >&2; }

pr='' full=0
for arg in "$@"; do
  case "$arg" in
    --full) full=1 ;;
    [0-9]*) pr="$arg" ;;
    *) usage ;;
  esac
done
[[ "$pr" =~ ^[0-9]+$ ]] || usage
for tool in gh jq docker flock make pnpm cargo; do
  command -v "$tool" >/dev/null || die "$tool not found"
done

cd "$(git rev-parse --show-toplevel)"
caller="$PWD"
repo="$(gh repo view --json nameWithOwner -q .nameWithOwner)"
# The caller's environment must not steer compose, make or the e2e helpers at the dev stack
# (COMPOSE_ENV_FILES=.env, TEST_DATABASE_URL, HTTP_PORT, ...). Everything comes from the
# throwaway worktree's .env below.
while read -r v; do unset "$v"; done < <(compgen -e | grep -E \
  '^(COMPOSE_|(TEST_|OWNER_)?DATABASE_URL$|APP_|PG_|MEILI_|MINIO_|MAILPIT_|STRIPE_|S3_|HTTPS?_PORT$|HOST_BIND$|E2E_|PLAYWRIGHT_|CI$)')

# --- PR preconditions --------------------------------------------------------------------
for _ in 1 2 3 4 5; do  # GitHub computes `mergeable` lazily; UNKNOWN settles in seconds.
  info="$(gh pr view "$pr" --json state,isDraft,mergeable,headRefOid,headRefName,baseRefName,url)"
  [ "$(jq -r .mergeable <<<"$info")" = UNKNOWN ] || break
  sleep 3
done
sha="$(jq -r .headRefOid <<<"$info")"
head_ref="$(jq -r .headRefName <<<"$info")"
base_ref="$(jq -r .baseRefName <<<"$info")"
[ "$(jq -r .state <<<"$info")" = OPEN ] || die "PR #$pr is not open"
[ "$(jq -r .isDraft <<<"$info")" = false ] || die "PR #$pr is a draft"
[ "$(jq -r .mergeable <<<"$info")" = MERGEABLE ] || die "PR #$pr is not mergeable ($(jq -r .mergeable <<<"$info")); rebase it first"
# Verifying something other than what is on your screen is a trap: on the PR branch, the
# local tree must be exactly the pushed head.
if [ "$(git branch --show-current)" = "$head_ref" ]; then
  [ -z "$(git status --porcelain)" ] || die "uncommitted changes on $head_ref; commit and push them first"
  [ "$(git rev-parse HEAD)" = "$sha" ] || die "local $head_ref ($(git rev-parse --short HEAD)) differs from the PR head (${sha:0:7}); push or pull first"
fi

# One run at a time: the stack's ports are fixed and a second heavy suite would swamp the box.
cache="${XDG_CACHE_HOME:-$HOME/.cache}/ecommerce-verify-merge"
mkdir -p "$cache"
exec 9>"$cache/lock"
flock -n 9 || die "another verify-merge is running"

git fetch -q origin "$base_ref" "+refs/pull/$pr/head:refs/verify-merge/pr-$pr"
[ "$(git rev-parse "refs/verify-merge/pr-$pr")" = "$sha" ] || die "fetched head differs from the PR head; retry"
base="$(git merge-base "origin/$base_ref" "$sha")"
# The squash result must be what gets tested: the PR has to contain the current base tip.
[ "$base" = "$(git rev-parse "origin/$base_ref")" ] ||
  die "PR #$pr is behind $base_ref; update it first (gh pr update-branch $pr --rebase)"

# --- throwaway worktree + stack, removed on success, failure and Ctrl-C -------------------
project=ecommerce-verify
wt="$cache/pr-$pr"
reported=0 status_posted=0
compose() { (cd "$wt" && COMPOSE_PROFILES=full docker compose -p "$project" --env-file "$wt/.env" "$@"); }
cleanup() {
  local code=$?
  trap - EXIT INT TERM
  # Install errors, Ctrl-C, ...: never leave the status pending.
  if ((code != 0 && status_posted && !reported)); then status error "aborted (exit $code)" || true; fi
  if [ -f "$wt/.env" ]; then
    echo "verify-merge: removing stack $project" >&2
    # Sandbox containers (started by the builder, outside compose) hold the sandbox networks.
    compose stop --timeout 5 theme-builder theme-sandbox-proxy >/dev/null 2>&1 || true
    docker ps -aq --filter "label=platform.theme-sandbox=$project" | xargs -r docker rm -f >/dev/null 2>&1 || true
    compose down -v --remove-orphans --timeout 5 >/dev/null 2>&1 || true
    # Exact tags only: an unchanged build shares image IDs with the dev stack's images.
    docker images --format '{{.Repository}}:{{.Tag}}' --filter "reference=$project-*:local" |
      xargs -r docker rmi >/dev/null 2>&1 || true
    if [ -n "$(docker ps -aq --filter "label=com.docker.compose.project=$project")$(docker volume ls -q --filter "label=com.docker.compose.project=$project")" ]; then
      echo "verify-merge: WARNING: $project containers/volumes left; the next run removes them" >&2
    fi
  fi
  git -C "$caller" worktree remove --force "$wt" 2>/dev/null || rm -rf "$wt"
  git -C "$caller" update-ref -d "refs/verify-merge/pr-$pr" 2>/dev/null || true
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# A leftover from a killed run (kill -9, reboot) would hold the ports.
docker ps -aq --filter "label=com.docker.compose.project=$project" | xargs -r docker rm -f >/dev/null
docker ps -aq --filter "label=platform.theme-sandbox=$project" | xargs -r docker rm -f >/dev/null
docker volume ls -q --filter "label=com.docker.compose.project=$project" | xargs -r docker volume rm >/dev/null
docker network ls -q --filter "label=com.docker.compose.project=$project" | xargs -r docker network rm >/dev/null
git worktree remove --force "$wt" 2>/dev/null || rm -rf "$wt"
git worktree prune
git worktree add -q --detach "$wt" "$sha"

status() {  # state, description (GitHub caps it at 140 chars)
  gh api -X POST "repos/$repo/statuses/$sha" -f state="$1" -f context=local-verify \
    -f description="${2:0:140}" >/dev/null
  status_posted=1
  [ "$1" = pending ] || reported=1
}
fail() {
  status failure "$1" || true
  die "$1"
}
status pending "verify-merge running on $(hostname -s)"

# --- scope: changed files -> areas -> specs ------------------------------------------------
mapfile -t changed < <(git diff --name-only "$base" "$sha")
map="$wt/scripts/verify-areas.txt"
[ -f "$map" ] || { echo "no scripts/verify-areas.txt at ${sha:0:7}: full run" >&2; full=1; map=/dev/null; }
declare -A areas=() specs=()
unmapped=()
for f in "${changed[@]}"; do
  hit=0
  if [[ "$f" == e2e/*.spec.ts ]]; then specs["${f#e2e/}"]=1; hit=1; fi
  while read -r area globs list; do
    [[ -z "$area" || "$area" == \#* ]] && continue
    IFS=, read -ra gs <<<"$globs"
    for g in "${gs[@]}"; do
      # shellcheck disable=SC2053 # unquoted: $g is a glob
      [[ "$f" == $g ]] || continue
      hit=1 areas["$area"]=1
      case "$list" in
        ALL) full=1 ;;
        -) ;;
        *) IFS=, read -ra ss <<<"$list"; for s in "${ss[@]}"; do specs["$s"]=1; done ;;
      esac
      break
    done
  done <"$map"
  [ "$hit" = 1 ] || unmapped+=("$f")
done
# When unsure, run more: an unmapped file means the full suite.
if ((${#unmapped[@]})); then full=1; fi
rust=0
for f in "${changed[@]}"; do
  if [[ "$f" == @(crates/*|migrations/*|fixtures/*|.sqlx/*|.cargo/*|Cargo.*|rust-toolchain.toml|docker/postgres/*) ]]; then rust=1; fi
done
if ((full)); then rust=1; fi
e2e=$((full || ${#specs[@]} > 0))
spec_list=("${!specs[@]}")
area_list="${!areas[*]}"

echo "PR #$pr ${sha:0:7} (base ${base:0:7}): ${#changed[@]} files changed" >&2
echo "areas:    ${area_list:-none}" >&2
if ((${#unmapped[@]})); then printf 'unmapped: %s\n' "${unmapped[@]:0:10}" >&2; fi
if ((full)); then echo "scope:    FULL (cargo, vitest, all e2e, image smoke, perf)" >&2
else echo "scope:    rust=$rust vitest=changed e2e=${spec_list[*]:-none}" >&2; fi

# --- run ------------------------------------------------------------------------------------
cd "$wt"
export CARGO_TARGET_DIR="$cache/target"  # persistent: a cold workspace build is many minutes
export CARGO_BUILD_JOBS=6
# Own project name and host ports; everything else is the .env.example default.
sed -e "s/^COMPOSE_PROJECT_NAME=.*/COMPOSE_PROJECT_NAME=$project/" \
  -e 's/^HTTP_PORT=.*/HTTP_PORT=18080/' -e 's/^HTTPS_PORT=.*/HTTPS_PORT=18443/' \
  -e 's/^PG_PORT=.*/PG_PORT=15432/' -e 's/^MEILI_PORT=.*/MEILI_PORT=17700/' \
  -e 's/^MINIO_PORT=.*/MINIO_PORT=19000/' -e 's/^MINIO_CONSOLE_PORT=.*/MINIO_CONSOLE_PORT=19001/' \
  -e 's/^MAILPIT_SMTP_PORT=.*/MAILPIT_SMTP_PORT=11025/' -e 's/^MAILPIT_UI_PORT=.*/MAILPIT_UI_PORT=18025/' \
  -e 's/^STRIPE_MOCK_PORT=.*/STRIPE_MOCK_PORT=12121/' .env.example >.env

log="$cache/pr-$pr-stack.log"
say "pnpm install"
pnpm install --frozen-lockfile --silent

say "vitest"
# --allowOnly=false: a committed .only must fail, not silently shrink the suite.
if ((full)); then pnpm exec vitest run --allowOnly=false || fail "vitest failed"
else pnpm exec vitest run --allowOnly=false --changed "$base" --passWithNoTests || fail "vitest failed (changed since ${base:0:7})"; fi

if ((e2e)); then
  say "stack: build + start (full profile)"
  make up >"$log" 2>&1 || fail "stack failed to start (log: $log)"
elif ((rust)); then
  say "stack: infra only (cargo tests)"
  make dev-infra >"$log" 2>&1 || fail "infra failed to start (log: $log)"
fi

if ((rust)); then
  say "cargo test"
  make test-rust || fail "cargo test failed"
  say "cargo test -- --ignored (Meilisearch)"
  make test-search || fail "cargo search tests failed"
fi

failed=()
perf_ok=1
e2e_rc=0
if ((e2e)); then
  say "seed"
  make seed >>"$log" 2>&1 || fail "seed failed (log: $log)"
  bash scripts/seed-m2-perf-review.sh >>"$log" 2>&1 || fail "review fixture seed failed (log: $log)"
  pnpm --filter @platform/e2e exec playwright install chromium firefox >/dev/null || fail "playwright install failed"
  results="$wt/.verify"
  mkdir -p "$results"
  # Specs run per project with --no-deps: a file filter would otherwise pull in every spec of
  # the dependency projects. Order and per-project workers match the config's dependency chain.
  e2e_rc=0
  for p in chromium-setup chromium firefox chromium-shared-state chromium-themes; do
    say "e2e: $p"
    # Own output dir per project (each run clears its own); CI=1 makes the config forbid .only.
    args=(--project "$p" --no-deps --pass-with-no-tests --reporter=list,json --output "$results/$p")
    [ "$p" = chromium-setup ] || ((full)) || args+=("${spec_list[@]}")
    if ! CI=1 PLAYWRIGHT_JSON_OUTPUT_NAME="$results/$p.json" make e2e args="${args[*]}"; then
      e2e_rc=1
      if [ "$p" = chromium-setup ]; then fail "e2e fixture setup failed"; fi
    fi
  done
  # Names for the report only; the exit codes above decide (load errors fail no single spec).
  mapfile -t failed < <(jq -r '.. | objects | select(has("specs")) | .specs[] | select(.ok == false) | .file' \
    "$results"/*.json 2>/dev/null | sort -u || true)
  if ((full)); then
    say "image smoke (degraded readiness)"
    i="$project-%s:local"
    # shellcheck disable=SC2059
    scripts/smoke-images.sh "$(printf "$i" rust)" "$(printf "$i" mocks)" "$(printf "$i" theme-builder)" \
      "$(printf "$i" auth)" || fail "image smoke test failed"
    say "perf (lab budget + axe)"
    make perf || perf_ok=0
  fi
fi

if ((e2e_rc)) || ! ((perf_ok)); then
  rm -rf "$cache/pr-$pr-test-results"
  cp -r "$results" "$cache/pr-$pr-test-results" 2>/dev/null || true
  what="${failed[*]:-e2e run errored (see output)}"; ((perf_ok)) || what="perf ${what}"
  status failure "failed: $what" || true
  printf '\n\033[31mverify-merge: FAILED\033[0m %s\n' "$what" >&2
  ((${#failed[@]} == 0)) || printf 'Rerun just those against your dev stack:\n  make e2e args="%s"\n' "${failed[*]}" >&2
  echo "Traces: $cache/pr-$pr-test-results (npx playwright show-trace <trace.zip>)" >&2
  exit 1
fi

# --- record + merge ---------------------------------------------------------------------
if ((full)); then summary="full: cargo, vitest, all e2e + axe, image smoke, perf"
else
  summary="rust=$rust vitest=changed e2e=${#spec_list[@]}: ${spec_list[*]}"
  ((e2e)) || summary="rust=$rust vitest=changed, no e2e (${area_list:-no areas})"
fi
cd "$caller"
# main moved while the tests ran: the squash would contain untested code.
git fetch -q origin "$base_ref"
[ "$(git rev-parse "origin/$base_ref")" = "$base" ] ||
  fail "$base_ref moved during verification; update the PR and rerun"
say "passed in ${SECONDS}s; recording local-verify and merging"
status success "$summary (${SECONDS}s)"
# --repo: gh leaves the local checkout alone (no base checkout, no local branch delete).
gh pr merge "$pr" --repo "$repo" --squash --delete-branch --match-head-commit "$sha"
