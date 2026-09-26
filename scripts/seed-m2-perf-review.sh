#!/usr/bin/env bash
# Add one real delivered-order review to the seeded demo PDP so `make perf` measures the
# visible M2 review section, not only its empty state. Idempotent across repeated checks.
set -euo pipefail
cd "$(dirname "$0")/.."

docker compose exec -T postgres psql -v ON_ERROR_STOP=1 -U postgres -d app <<'SQL'
INSERT INTO reviews (tenant_id, product_id, order_line_id, customer_name, rating, title,
                     body, status, verified, locale, published_at)
SELECT o.tenant_id, ol.product_id, ol.id, 'M2 perf fixture', 5, 'Great fit',
       'Delivered-order review for the M2 performance gate.', 'published', true, 'cs', now()
FROM order_lines ol
JOIN orders o ON o.id = ol.order_id AND o.tenant_id = ol.tenant_id
JOIN product_translations pt ON pt.product_id = ol.product_id AND pt.tenant_id = ol.tenant_id
JOIN platform.tenants t ON t.id = o.tenant_id
WHERE t.slug = 'demo' AND pt.slug = 'tricko-basic' AND pt.locale = 'cs'
  AND o.status = 'delivered'
  AND NOT EXISTS (SELECT 1 FROM reviews r WHERE r.tenant_id = o.tenant_id
                  AND r.customer_name = 'M2 perf fixture')
  AND NOT EXISTS (SELECT 1 FROM reviews r WHERE r.tenant_id = o.tenant_id
                  AND r.order_line_id = ol.id)
ORDER BY ol.id
LIMIT 1;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM reviews r JOIN platform.tenants t ON t.id = r.tenant_id
                 WHERE t.slug = 'demo' AND r.customer_name = 'M2 perf fixture'
                   AND r.status = 'published' AND r.verified) THEN
    RAISE EXCEPTION 'M2 performance review fixture was not seeded';
  END IF;
END $$;
SQL
