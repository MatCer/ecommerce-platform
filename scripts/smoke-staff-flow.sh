#!/usr/bin/env bash
# End-to-end check of staff identity and tenancy through the real stack (`make up` first):
# CLI tenant creation -> magic link from Mailpit -> Better Auth session -> EdDSA JWT ->
# Admin API through Caddy (me, markets, idempotency, audit log, cross-tenant 403) ->
# internal host resolution -> custom domain verification via the DNS stub.
# Needs curl and jq. Reads ports from .env.
# shellcheck source=lib/smoke.sh
source "$(dirname "$0")/lib/smoke.sh"

step "create two tenants with the superadmin CLI"
a=$(admin create-tenant --slug "acme-$run" --name "Acme $run" --owner-email "owner-$run@example.test")
b=$(admin create-tenant --slug "beta-$run" --name "Beta $run" --owner-email "other-$run@example.test")
tenant_a=$(jq -r .tenant_id <<<"$a")
tenant_b=$(jq -r .tenant_id <<<"$b")
echo "tenant A $tenant_a, tenant B $tenant_b"

step "sign in: magic link from Mailpit -> session cookie -> JWT"
jwt=$(login "owner-$run@example.test")
jq -R 'split(".")[1] | @base64d | fromjson | {iss, aud, email, email_verified, lifetime: (.exp - .iat)}' <<<"$jwt"

step "GET /admin/v1/me"
expect_status 200 -H "authorization: Bearer $jwt" "$api/admin/v1/me" | jq -c '{user_id, memberships: [.memberships[] | {slug, role}]}'

step "POST /admin/v1/markets with Idempotency-Key (twice)"
market='{"code":"sk","name":"Slovensko","country_codes":["SK"],"currency":"EUR","default_locale":"sk","locales":["sk"]}'
hdrs=(-H "authorization: Bearer $jwt" -H "x-tenant-id: $tenant_a" -H "content-type: application/json" -H "idempotency-key: sk-$run")
first=$(expect_status 201 "${hdrs[@]}" -d "$market" "$api/admin/v1/markets")
second=$(curl -s -D - "${hdrs[@]}" -d "$market" "$api/admin/v1/markets")
grep -qi '^idempotent-replayed: true' <<<"$second" || fail "second call was not a replay"
[[ $(jq -r .id <<<"$first") == $(tail -1 <<<"$second" | jq -r .id) ]] || fail "replay returned a different market"
echo "created $(jq -c '{id, code, currency}' <<<"$first"), replayed OK"
expect_status 409 "${hdrs[@]}" -d "${market/Slovensko/Slovakia}" "$api/admin/v1/markets" | jq -c '{status, code}'

step "GET /admin/v1/markets and /audit-log in tenant A"
expect_status 200 -H "authorization: Bearer $jwt" -H "x-tenant-id: $tenant_a" "$api/admin/v1/markets" | jq -c '[.items[].code]'
expect_status 200 -H "authorization: Bearer $jwt" -H "x-tenant-id: $tenant_a" "$api/admin/v1/audit-log" | jq -c '[.items[] | {action, actor}]'

step "tenant A's owner is refused in tenant B"
expect_status 403 -H "authorization: Bearer $jwt" -H "x-tenant-id: $tenant_b" "$api/admin/v1/markets" | jq -c '{status, code}'
expect_status 401 -H "authorization: Bearer ${jwt}x" "$api/admin/v1/me" | jq -c '{status, code}'

step "internal host resolution (compose network only; Caddy answers 404)"
expect_status 404 "$api/internal/v1/resolve?host=acme-$run.localhost" >/dev/null
"${compose[@]}" exec -T caddy wget -qO- --header "Authorization: Bearer $INTERNAL_API_TOKEN" \
  "http://api:8000/internal/v1/resolve?host=acme-$run.localhost:8080" | jq -c '{tenant_slug, market_code, currency}'

step "custom domain: unverified until the TXT record exists"
host="shop-$run.example.cz"
added=$(admin add-domain --tenant "acme-$run" --host "$host")
txt_name=$(jq -r .txt_record.name <<<"$added")
txt_value=$(jq -r .txt_record.value <<<"$added")
if admin verify-domain --host "$host" >/dev/null 2>&1; then fail "verified without a TXT record"; fi
"${compose[@]}" exec -T mocks node -e "fetch('http://127.0.0.1:4010/dns/txt',{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({name:process.argv[1],records:[process.argv[2]]})}).then(r=>{if(!r.ok)process.exit(1)})" "$txt_name" "$txt_value"
admin verify-domain --host "$host" | jq -c .
"${compose[@]}" exec -T caddy wget -qO- --header "Authorization: Bearer $INTERNAL_API_TOKEN" \
  "http://api:8000/internal/v1/resolve?host=$host" | jq -c '{hostname, tenant_slug}'

printf '\nstaff flow OK\n'
