#!/usr/bin/env bash
# End-to-end check of the catalog and media through the real stack (`make up` first):
# a `staff`-role member builds a category tree, uploads an image through the presigned flow
# (MinIO private bucket -> worker -> variants in the public bucket), creates a product with
# 2 options / 4 variants / cs+sk+en translations and reads it back; another tenant cannot see
# it. Needs curl and jq. Reads ports from .env.
# shellcheck source=lib/smoke.sh
source "$(dirname "$0")/lib/smoke.sh"

photo=fixtures/images/smoke-photo.jpg

step "tenants A and B, and a clerk added to A with role staff"
a=$(admin create-tenant --slug "cat-a-$run" --name "Catalog A $run" --owner-email "cat-owner-$run@example.test")
b=$(admin create-tenant --slug "cat-b-$run" --name "Catalog B $run" --owner-email "cat-rival-$run@example.test")
admin create-tenant --slug "cat-c-$run" --name "Clerk home $run" --owner-email "cat-clerk-$run@example.test" >/dev/null
tenant_a=$(jq -r .tenant_id <<<"$a")
tenant_b=$(jq -r .tenant_id <<<"$b")
clerk=$(login "cat-clerk-$run@example.test")
clerk_id=$(expect_status 200 -H "authorization: Bearer $clerk" "$api/admin/v1/me" | jq -r .user_id)
# No staff-management API yet (later WP): add the membership directly (superuser bypasses RLS).
"${compose[@]}" exec -T postgres psql -qAt -U postgres -d app -v ON_ERROR_STOP=1 -c \
  "INSERT INTO staff_members (tenant_id, user_id, email, role)
   VALUES ('$tenant_a', '$clerk_id', 'cat-clerk-$run@example.test', 'staff')" >/dev/null
echo "clerk $clerk_id is staff in $tenant_a"

as_clerk=(-H "authorization: Bearer $clerk" -H "x-tenant-id: $tenant_a" -H "content-type: application/json")
post() { expect_status "$1" "${as_clerk[@]}" -X POST -d "$3" "$api/admin/v1/$2"; }
get() { expect_status 200 "${as_clerk[@]}" "$api/admin/v1/$1"; }

step "category tree: Oblečení > (Mikiny, Trička)"
root=$(post 201 categories '{"parent_id":null,"translations":[{"locale":"cs","name":"Oblečení","slug":"obleceni-'$run'"},{"locale":"en","name":"Clothing","slug":"clothing-'$run'"}]}' | jq -r .id)
tees=$(post 201 categories '{"parent_id":"'$root'","translations":[{"locale":"cs","name":"Trička","slug":"tricka-'$run'"}]}' | jq -r .id)
hoodies=$(post 201 categories '{"parent_id":"'$root'","translations":[{"locale":"cs","name":"Mikiny","slug":"mikiny-'$run'"}]}' | jq -r .id)
post 200 "categories/$hoodies/move" '{"parent_id":"'$root'","position":0}' >/dev/null
post 422 "categories/$root/move" '{"parent_id":"'$tees'","position":0}' | jq -c '{status, code}'
get categories | jq -c '[.items[] | {name: .translations[0].name, children: [.children[] | .translations[0].name]}]'

step "image upload: presigned PUT -> complete -> worker variants"
size=$(stat -c %s "$photo")
upload=$(expect_status 201 "${as_clerk[@]}" -H "idempotency-key: img-$run" \
  -d '{"filename":"smoke-photo.jpg","content_type":"image/jpeg","size":'"$size"'}' "$api/admin/v1/assets/uploads")
asset=$(jq -r .asset.id <<<"$upload")
url=$(jq -r .upload.url <<<"$upload")
echo "PUT ${url%%\?*} (presigned, private bucket)"
status=$(curl -s -o /dev/null -w '%{http_code}' -X PUT -H "content-type: image/jpeg" --data-binary "@$photo" "$url")
[[ $status == 200 ]] || fail "presigned PUT returned $status"
public_original="http://s3.localhost:${HTTP_PORT}/public/uploads/$tenant_a/$asset"
[[ $(curl -s -o /dev/null -w '%{http_code}' "$public_original") != 200 ]] || fail "original readable publicly"
[[ $(curl -s -o /dev/null -w '%{http_code}' "${url%%\?*}") == 403 ]] || fail "private object readable without a signature"
post 200 "assets/$asset/complete" 'null' | jq -c '{status, mime, width, height}'
for _ in $(seq 1 90); do
  state=$(get "assets/$asset" | jq -r .status)
  [[ $state == ready || $state == failed ]] && break
  sleep 1
done
[[ $state == ready ]] || fail "asset is $state"
ready=$(get "assets/$asset")
jq -c '[.variants[] | "\(.format) \(.width)x\(.height) \(.bytes)B"]' <<<"$ready"
for v in $(jq -r '.variants[].url' <<<"$ready"); do
  headers=$(curl -s -o /tmp/smoke-variant.$$ -D - "$v")
  grep -q '^HTTP/1.1 200' <<<"$headers" || fail "variant not public: $v"
  grep -qi '^cache-control: public, max-age=31536000, immutable' <<<"$headers" || fail "no immutable cache header: $v"
  ! grep -qa -e SmokeCam -e GPSSECRET /tmp/smoke-variant.$$ || fail "EXIF survived in $v"
done
rm -f /tmp/smoke-variant.$$
echo "$(jq '.variants | length' <<<"$ready") variants public in MinIO, EXIF stripped"

step "product: 2 options x 2 values = 4 variants, cs/sk/en, image, category, SK reduced VAT"
product=$(jq -n --arg run "$run" --arg cat "$tees" --arg asset "$asset" '{
  status: "active", brand: "Smoke",
  gpsr: { manufacturer: { name: "Smoke s.r.o.", address: "Praha 1", email: "info@smoke.test" } },
  translations: [
    { locale: "cs", name: "Tričko Smoke", slug: "tricko-smoke-\($run)", description_html: "<p>Bavlna <script>alert(1)</script></p>" },
    { locale: "sk", name: "Tričko Smoke", slug: "tricko-smoke-\($run)" },
    { locale: "en", name: "Smoke T-shirt", slug: "smoke-t-shirt-\($run)" } ],
  options: [
    { code: "color", name_i18n: { cs: "Barva", sk: "Farba", en: "Color" },
      values: [ { code: "red", name_i18n: { cs: "Červená" } }, { code: "blue", name_i18n: { cs: "Modrá" } } ] },
    { code: "size", name_i18n: { cs: "Velikost" },
      values: [ { code: "s", name_i18n: { cs: "S" } }, { code: "m", name_i18n: { cs: "M" } } ] } ],
  variants: [ ["red","s"], ["red","m"], ["blue","s"], ["blue","m"] ] | map({
    sku: "SMK-\($run)-\(.[0])-\(.[1])", option_values: { color: .[0], size: .[1] }, weight_g: 180 }),
  category_ids: [ $cat ],
  media: [ { asset_id: $asset, alt_i18n: { cs: "Tričko zepředu" } } ],
  tax_categories: { SK: "reduced" } }')
created=$(expect_status 201 "${as_clerk[@]}" -H "idempotency-key: prod-$run" -d "$product" "$api/admin/v1/products")
id=$(jq -r .id <<<"$created")
replay=$(curl -s -D - "${as_clerk[@]}" -H "idempotency-key: prod-$run" -d "$product" "$api/admin/v1/products")
grep -qi '^idempotent-replayed: true' <<<"$replay" || fail "product create was not replayed"

step "GET /admin/v1/products/$id"
got=$(get "products/$id")
jq -c '{status, locales: [.translations[].locale], variants: [.variants[] | .sku + (if .is_default then "*" else "" end)], media: [.media[].asset_id], categories: .category_ids, tax: .tax_categories}' <<<"$got"
[[ $(jq '.variants | length' <<<"$got") == 4 ]] || fail "expected 4 variants"
jq -e '.translations[] | select(.locale == "cs") | .description_html | contains("script") | not' <<<"$got" >/dev/null || fail "script not sanitized"
get "products?q=smk-$run-blue" | jq -c '{found: [.items[].id], next_cursor}'
get "products?category_id=$tees" | jq -e --arg id "$id" '[.items[].id] == [$id]' >/dev/null || fail "category filter"

step "tenant B cannot see tenant A's product"
rival=$(login "cat-rival-$run@example.test")
expect_status 404 -H "authorization: Bearer $rival" -H "x-tenant-id: $tenant_b" "$api/admin/v1/products/$id" | jq -c '{status, code}'
expect_status 200 -H "authorization: Bearer $rival" -H "x-tenant-id: $tenant_b" "$api/admin/v1/products" | jq -c '{items: (.items | length)}'
expect_status 403 -H "authorization: Bearer $rival" -H "x-tenant-id: $tenant_a" "$api/admin/v1/products/$id" | jq -c '{status, code}'

step "audit log (owner of A) lists the clerk's changes"
owner=$(login "cat-owner-$run@example.test")
expect_status 200 -H "authorization: Bearer $owner" -H "x-tenant-id: $tenant_a" "$api/admin/v1/audit-log?limit=20" |
  jq -c --arg clerk "$clerk_id" '[.items[] | select(.actor == $clerk) | .action] | unique'

printf '\ncatalog flow OK\n'
