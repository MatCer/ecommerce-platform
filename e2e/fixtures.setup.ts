import { expect, test } from "@playwright/test";
import { signInOwner, sql } from "./admin/support";
import { rateHeaders } from "./rate-client";

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
