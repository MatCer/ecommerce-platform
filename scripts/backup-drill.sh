#!/usr/bin/env bash
# The scripted A29 restore drill (`make backup-drill`, docs/runbook.md §7). DESTRUCTIVE: wipes
# the compose project's volumes. Against the running, seeded stack (`make up seed theme-build`):
#   1. quiesce writers, record row counts of key tables, pick an active product + a ready asset
#   2. scripts/backup.sh
#   3. docker compose down -v            (all data gone)
#   4. make up                           (fresh stack: roles, migrations, buckets)
#   5. scripts/restore.sh                (database + buckets, then start + search rebuild)
#   6. verify: identical row counts, the product page and the image variant are served
# Prints PASS/FAIL per check and the timings (the local RTO); exits non-zero on any failure.
# Guards: DRILL_CONFIRM=destroy is required; the default project `ecommerce` (your everyday
# stack) additionally needs DRILL_ALLOW_DEFAULT=1.
source "$(dirname "$0")/lib/ops.sh"

[[ ${DRILL_CONFIRM-} == destroy ]] ||
  die "this wipes the volumes of compose project '$COMPOSE_PROJECT_NAME'; rerun with DRILL_CONFIRM=destroy"
[[ $COMPOSE_PROJECT_NAME != ecommerce || ${DRILL_ALLOW_DEFAULT-} == 1 ]] ||
  die "refusing to wipe the default project 'ecommerce' without DRILL_ALLOW_DEFAULT=1"
require_running postgres minio api worker edge caddy

TABLES=(platform.tenants products variants assets orders customers consent_records audit_log
  events webhook_subscriptions)
shop=http://demo.localhost:$HTTP_PORT
api=http://api.localhost:$HTTP_PORT
counts() { # "<table> <rows>" per line; fails if a table is missing
  local t sql=""
  for t in "${TABLES[@]}"; do sql+="${sql:+ UNION ALL }SELECT '$t', count(*) FROM $t"; done
  psql_su -F ' ' -c "$sql"
}

failures=0
check() { # check <name> <command...>
  if "${@:2}"; then echo "PASS  $1"; else
    echo "FAIL  $1"
    failures=$((failures + 1))
  fi
}
get_ok() { # get_ok <url> [text]: 200 and, if given, the body contains text
  local body rc=0 hostport=${1#http://}
  hostport=${hostport%%/*}
  body=$(mktemp)
  [[ $(curl -sS --resolve "$hostport:127.0.0.1" -o "$body" -w '%{http_code}' "$1") == 200 ]] || rc=1
  [[ $rc == 1 || -z ${2-} ]] || grep -qF -- "$2" "$body" || rc=1
  rm -f "$body"
  return $rc
}

log "1/6 pick samples (demo tenant) and check they are served before the drill"
read -r slug name < <(psql_su -F ' ' -c "
  SELECT pt.slug, pt.name FROM product_translations pt
  JOIN products p ON p.tenant_id = pt.tenant_id AND p.id = pt.product_id
  JOIN platform.tenants t ON t.id = p.tenant_id
  WHERE t.slug = 'demo' AND p.status = 'active' AND pt.locale = 'cs' AND pt.name !~ '[&<>\"'']'
  ORDER BY pt.slug LIMIT 1") || true
asset=$(psql_su -c "
  SELECT a.variants -> 0 ->> 'key' FROM assets a JOIN platform.tenants t ON t.id = a.tenant_id
  WHERE t.slug = 'demo' AND a.status = 'ready' AND jsonb_array_length(a.variants) > 0
  ORDER BY a.id LIMIT 1")
[[ -n ${slug-} && -n $asset ]] || die "no active demo product or ready asset: run make seed theme-build first"
get_ok "$shop/p/$slug" "$name" || die "$shop/p/$slug is not served before the drill (make theme-build?)"
get_ok "$shop/$asset" || die "$shop/$asset is not served before the drill"
echo "product /p/$slug ($name), asset /$asset"

# Quiesce writers so the counts equal the dump (the drill destroys the stack anyway).
"${compose[@]}" stop api worker edge
before=$(counts)
echo "$before"

log "2/6 backup"
t0=$SECONDS
dir=$(scripts/backup.sh)
t_backup=$((SECONDS - t0))

log "3/6 destroy: docker compose down -v"
"${compose[@]}" down -v

log "4/6 fresh stack: make up"
t0=$SECONDS
make up
t_up=$((SECONDS - t0))

log "5/6 restore $dir"
t0=$SECONDS
scripts/restore.sh --no-start "$dir"
t_restore=$((SECONDS - t0))
after=$(counts)
t0=$SECONDS
"${compose[@]}" up -d --wait
"${compose[@]}" exec -T api /usr/local/bin/api admin reindex
t_start=$((SECONDS - t0))

log "6/6 verify"
check "row counts identical" diff <(echo "$before") <(echo "$after")
check "product page /p/$slug contains '$name'" get_ok "$shop/p/$slug" "$name"
check "image variant /$asset served" get_ok "$shop/$asset"
check "api /readyz 200" get_ok "$api/readyz"
echo
echo "backup:          ${t_backup} s  ($dir)"
echo "fresh stack:     ${t_up} s  (make up, build cache)"
echo "restore data:    ${t_restore} s"
echo "start + reindex: ${t_start} s"
echo "RTO (restore + start, excluding provisioning): $((t_restore + t_start)) s"
((failures == 0)) || die "$failures check(s) failed"
echo "DRILL PASSED"
