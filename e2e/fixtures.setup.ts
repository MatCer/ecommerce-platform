import { expect, test } from "@playwright/test";
import { signInOwner, sql } from "./admin/support";
import { FRESH } from "./checkout/support";
import { rateHeaders } from "./rate-client";

test("seed a same-origin test origin of the demo shop", () => {
  const { hostname, port } = new URL(FRESH);
  sql(`INSERT INTO platform.hostname_claims (hostname, tenant_id, platform_issued)
         SELECT '${hostname}', id, true FROM platform.tenants WHERE slug = 'demo'
         ON CONFLICT (hostname) DO NOTHING;
       INSERT INTO platform.domains (hostname, tenant_id, market_id, verified_at)
         SELECT '${hostname}', m.tenant_id, m.id, now()
         FROM markets m JOIN platform.tenants t ON t.id = m.tenant_id
         WHERE t.slug = 'demo' AND m.is_default
         ON CONFLICT (hostname) DO NOTHING;
       INSERT INTO platform.checkout_origins
           (scheme, hostname, port, tenant_id, market_id, history, mode)
         SELECT 'http', hostname, ${Number(port || 80)}, tenant_id, market_id,
                'platform_never_reassigned', 'same_origin'
         FROM platform.domains WHERE hostname = '${hostname}'
         ON CONFLICT (origin) DO UPDATE SET history = EXCLUDED.history, mode = EXCLUDED.mode;`);
  expect(
    sql(`SELECT string_agg(origin || ' ' || mode_generation, ',')
         FROM platform.checkout_origins WHERE mode = 'same_origin'`),
  ).toBe(`${FRESH} 1`);
});

test("provision stock for concurrent checkout fixtures", async ({ browser }) => {
  // Carts do not reserve stock. Parallel specs can all add the last seeded unit,
  // then lose it before placement. Replenish only their five dedicated products
  // before workers start, including on repeated runs against the same database.
  // 100 units exceeds this suite's combined demand; stock tracking stays enabled.
  const page = await signInOwner(browser);
  try {
    const auth = await page.request.get(new URL("/api/auth/token", page.url()).toString(), {
      headers: rateHeaders(page),
    });
    expect(auth.ok()).toBe(true);
    const { token } = (await auth.json()) as { token: string };
    const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
    const rows = sql(`SELECT DISTINCT ON (p.id) v.id, i.reserved, i.on_hand
      FROM products p JOIN product_translations pt ON pt.product_id=p.id
      JOIN variants v ON v.product_id=p.id
      JOIN inventory_levels i ON i.variant_id=v.id AND i.tenant_id=p.tenant_id
      WHERE p.tenant_id='${tenant}' AND pt.locale='cs'
        AND pt.slug IN ('tricko-henley','tricko-basic','tricko-oversize',
                        'mikina-crew','mikina-oversize')
      ORDER BY p.id, v.position, v.id`).split("\n");
    expect(rows).toHaveLength(5);
    const api = new URL(page.url());
    api.hostname = api.hostname.replace(/^admin\./, "api.");
    for (const row of rows) {
      const [variant, reserved, onHand] = row.split("|");
      const stock = Number(reserved) + 100;
      if (Number(onHand) >= stock) continue;
      api.pathname = `/admin/v1/inventory/${variant}/adjustments`;
      const adjusted = await page.request.post(api.toString(), {
        headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
        data: { on_hand: stock, note: "Acceptance checkout fixture stock" },
      });
      expect(adjusted.status()).toBe(201);
      expect(
        Number(
          sql(`SELECT on_hand-reserved FROM inventory_levels
        WHERE tenant_id='${tenant}' AND variant_id='${variant}'`),
        ),
      ).toBeGreaterThanOrEqual(100);
    }
  } finally {
    await page.context().close();
  }
});
