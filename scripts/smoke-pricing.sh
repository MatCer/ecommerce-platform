#!/usr/bin/env bash
# End-to-end check of WP4 through the real stack (`make up` first): the owner sets the tax
# profile (fresh login), creates a CZK price list for the default market, prices a product,
# schedules a sale, publishes a coupon and adjusts stock; the price history shows the scheduled
# sale and the Omnibus reference, and the outbox holds price.changed / inventory.changed with
# before/after. Needs curl and jq. Reads ports from .env.
# shellcheck source=lib/smoke.sh
source "$(dirname "$0")/lib/smoke.sh"

step "tenant with owner (fresh login)"
t=$(admin create-tenant --slug "price-$run" --name "Pricing $run" --owner-email "price-owner-$run@example.test")
tenant=$(jq -r .tenant_id <<<"$t")
market=$(jq -r .market_id <<<"$t")
token=$(login "price-owner-$run@example.test")
as_owner=(-H "authorization: Bearer $token" -H "x-tenant-id: $tenant" -H "content-type: application/json")
call() { expect_status "$1" "${as_owner[@]}" -X "$2" -d "${4:-}" "$api/admin/v1/$3"; }
get() { expect_status 200 "${as_owner[@]}" "$api/admin/v1/$1"; }

step "tax profile: CZ VAT payer, OSS (destination)"
call 200 PUT tax-profile '{"establishment_country":"CZ","vat_payer":true,"vat_id":"CZ12345678","distance_sales_mode":"destination"}' |
  jq -c '{establishment_country, vat_payer, distance_sales_mode}'

step "product with one variant"
product=$(call 201 POST products '{"status":"active","translations":[{"locale":"cs","name":"Hrnek","slug":"hrnek-'$run'"}],
  "variants":[{"sku":"MUG-'$run'"}]}')
product_id=$(jq -r .id <<<"$product")
variant=$(jq -r '.variants[0].id' <<<"$product")

step "price list for the default market + price 1 290,00 Kč"
list=$(call 201 POST price-lists '{"code":"cz","name":"Česko","currency":"CZK","market_ids":["'$market'"]}')
list_id=$(jq -r .id <<<"$list")
jq -c '{code, currency, market_ids}' <<<"$list"
call 200 PUT "price-lists/$list_id/prices" '{"items":[{"variant_id":"'$variant'","amount_minor":129000}]}' | jq -c '.items'

step "sale -20 % scheduled in 2 days for 7 days"
starts=$(date -u -d '+2 days' +%Y-%m-%dT%H:%M:%SZ)
ends=$(date -u -d '+9 days' +%Y-%m-%dT%H:%M:%SZ)
call 201 POST sales '{"name":"Podzim","discount":{"type":"percent","basis_points":2000},
  "starts_at":"'$starts'","ends_at":"'$ends'","targets":{"product_ids":["'$product_id'"]}}' |
  jq -c '{name, starts_at, ends_at}'

step "published coupon"
call 201 POST coupons '{"code":"podzim10","discount":{"type":"percent","basis_points":1000},"published":true}' |
  jq -c '{code, discount, published}'

step "stock: count 12, then -2 (retry with the same key is not applied twice)"
expect_status 201 "${as_owner[@]}" -H "idempotency-key: count-$run" -d '{"on_hand":12,"note":"inventura"}' \
  "$api/admin/v1/inventory/$variant/adjustments" | jq -c '{applied, on_hand: .level.on_hand}'
for _ in 1 2; do
  expect_status 201 "${as_owner[@]}" -H "idempotency-key: minus-$run" -d '{"delta":-2}' \
    "$api/admin/v1/inventory/$variant/adjustments" | jq -c '{applied, on_hand: .level.on_hand}'
done
[[ $(get "inventory?product_id=$product_id" | jq '.items[0].on_hand') == 10 ]] || fail "stock is not 10"

step "price history now and at the sale start"
get "products/$product_id/price-history" |
  jq -c '.items[0] | {intervals: [.intervals[] | {cause, amount_minor, valid_from, valid_to}], omnibus}'
at=$(date -u -d '+2 days +1 hour' +%Y-%m-%dT%H:%M:%SZ)
omnibus=$(get "products/$product_id/price-history?at=$at" | jq -c '.items[0].omnibus')
echo "$omnibus"
[[ $(jq .current_minor <<<"$omnibus") == 103200 && $(jq .claim <<<"$omnibus") == true ]] ||
  fail "unexpected Omnibus figures at the sale"

step "outbox events"
"${compose[@]}" exec -T postgres psql -qAt -U postgres -d app -v ON_ERROR_STOP=1 -c \
  "SELECT type || ' ' || payload::text FROM queue.outbox
   WHERE tenant_id = '$tenant' AND type IN ('price.changed', 'inventory.changed') ORDER BY id"
"${compose[@]}" exec -T postgres psql -qAt -U postgres -d app -v ON_ERROR_STOP=1 -c \
  "SELECT kind || ' at ' || run_at FROM queue.jobs WHERE tenant_id = '$tenant' AND kind = 'pricing.transition' ORDER BY run_at"

echo
echo "OK: pricing smoke passed"
