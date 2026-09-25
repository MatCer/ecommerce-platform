import { createHash, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { type Context, Hono } from "hono";
import {
  CATEGORIES,
  MARKETS,
  type Market,
  PRODUCTS,
  type ProductData,
  productBySlug,
  variantById,
} from "./catalog.ts";

/**
 * Stub Storefront API (`/storefront/v1/*`, spec §8.2) with fixture data, so the edge, the theme
 * and the performance probe have a realistic upstream until WP6 builds the real one in Rust.
 * Tenant/market come from the `X-Tenant`/`X-Market` headers the edge injects.
 */
export const storefront = new Hono();

const WIDTHS = [360, 480, 720, 1080];
const img = (ref: { key: string; alt: string }) => ({
  ...ref,
  width: 1080,
  height: 1350,
  widths: WIDTHS,
});

const market = (c: Context): Market =>
  MARKETS[c.req.header("x-market") ?? ""] ?? (MARKETS["m-cz"] as Market);

function money(czkMinor: number, m: Market) {
  const amount_minor = m.currency === "CZK" ? czkMinor : Math.round((czkMinor * m.rate) / 10) * 10;
  const formatted = new Intl.NumberFormat(m.locale, {
    style: "currency",
    currency: m.currency,
  }).format(amount_minor / 100);
  return { amount_minor, currency: m.currency, formatted };
}

function price(p: ProductData, m: Market) {
  return {
    price: money(p.price, m),
    reference_price: p.reference ? money(p.reference, m) : null,
    discount_percent: p.reference
      ? Math.round(((p.reference - p.price) / p.reference) * 100)
      : null,
    unit_price: null,
  };
}

// ponytail: the real API derives canonical hosts from market domains (WP6).
const base = (c: Context) =>
  c.req.header("x-market") === "m-sk" ? "https://demo-sk.localhost" : "https://demo.localhost";

function card(p: ProductData, m: Market) {
  const stock = p.variants.every((v) => v.stock === "out_of_stock") ? "out_of_stock" : "in_stock";
  return {
    id: p.id,
    slug: p.slug,
    name: p.name,
    brand: p.brand,
    image: img(p.images[0] ?? { key: "p/hero", alt: p.name }),
    stock,
    badges: p.reference ? ["Sleva"] : p.created > 26 ? ["Novinka"] : [],
    ...price(p, m),
  };
}

const seo = (
  title: string,
  description: string,
  canonical: string,
  extra: Record<string, unknown>[] = [],
  robots?: string,
) => ({
  title,
  description,
  canonical,
  alternates: [],
  json_ld: extra,
  ...(robots ? { robots } : {}),
});

const cache = (tags: string[], max_age = 60) => ({ public: true, max_age, tags });

storefront.get("/shop", (c) => {
  const m = market(c);
  return c.json({
    name: "Lnen & Co.",
    locale: m.locale.slice(0, 2),
    currency: m.currency,
    locales: [],
    menus: {
      main: CATEGORIES.map((k) => ({ label: k.name, href: `/c/${k.slug}` })),
      footer: [
        { label: "Doprava a platba", href: "/pages/doprava" },
        { label: "Reklamace a vrácení", href: "/pages/vraceni" },
        { label: "Kontakt", href: "/pages/kontakt" },
      ],
    },
    legal_pages: [
      { label: "Obchodní podmínky", href: "/pages/obchodni-podminky" },
      { label: "Ochrana osobních údajů", href: "/pages/ochrana-udaju" },
    ],
    consent: { purposes: ["analytics", "ads", "personalization"], policy_url: "/pages/cookies" },
    free_shipping_threshold: money(150000, m),
    tracking: { rum_sample_rate: 0.1 },
    trust: {
      delivery: "Doručení do 2 pracovních dnů",
      returns: "Vrácení do 30 dnů zdarma",
      payments: ["Karta", "Apple Pay", "Převodem", "Dobírka"],
    },
    seo: seo("Lnen & Co.", "Oblečení z organické bavlny.", `${base(c)}/`),
    cache: cache(["shop"], 300),
  });
});

storefront.get("/pages/home", (c) => {
  const m = market(c);
  return c.json({
    hero: {
      title: "Podzimní kolekce",
      subtitle: "Mikiny a trička z organické bavlny, šitá v Česku.",
      image: img({ key: "p/hero", alt: "Mikina z podzimní kolekce" }),
      cta: { label: "Prohlédnout mikiny", href: "/c/mikiny" },
    },
    categories: CATEGORIES.map((k) => ({
      label: k.name,
      href: `/c/${k.slug}`,
      image: img({
        key: `p/${k.slug === "trika" ? "tee-forest" : k.slug === "mikiny" ? "hoodie-clay" : "bag-natural"}`,
        alt: k.name,
      }),
    })),
    featured: PRODUCTS.slice(0, 8).map((p) => card(p, m)),
    seo: seo(
      "Lnen & Co. – oblečení z organické bavlny",
      "Trička, mikiny a doplňky.",
      `${base(c)}/`,
    ),
    cache: cache(["home", "products"]),
  });
});

const PAGE_SIZE = 24;

function listing(
  c: Context,
  products: ProductData[],
  title: string,
  basePath: string,
  breadcrumbs: { label: string; href: string }[],
  extra: Record<string, unknown>,
) {
  const m = market(c);
  const q = new URL(c.req.url).searchParams;
  const sizes = q.getAll("velikost");
  const colors = q.getAll("barva");
  const sort = q.get("sort") ?? "doporucene";
  const page = Math.max(1, Number.parseInt(q.get("page") ?? "1", 10) || 1);

  const matches = (p: ProductData, s: string[], col: string[]) =>
    p.variants.some(
      (v) =>
        v.stock !== "out_of_stock" &&
        (!s.length || s.includes(v.options.Velikost ?? "")) &&
        (!col.length || col.includes(v.options.Barva ?? "")),
    );
  let list = products.filter((p) => matches(p, sizes, colors));
  if (sort === "nejlevnejsi") list = [...list].sort((a, b) => a.price - b.price);
  if (sort === "nejdrazsi") list = [...list].sort((a, b) => b.price - a.price);
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));

  const href = (mut: (p: URLSearchParams) => void) => {
    const p = new URLSearchParams(q);
    p.delete("page");
    mut(p);
    const s = p.toString();
    return `${basePath}${s ? `?${s}` : ""}`;
  };
  const toggle = (key: string, value: string) =>
    href((p) => {
      const cur = p.getAll(key);
      p.delete(key);
      for (const v of cur.includes(value) ? cur.filter((x) => x !== value) : [...cur, value])
        p.append(key, v);
    });
  const facet = (
    key: string,
    label: string,
    values: string[],
    selected: string[],
    test: (v: string) => boolean,
  ) => ({
    key,
    label,
    values: values.map((v) => ({
      value: v,
      label: v,
      selected: selected.includes(v),
      disabled: !test(v),
      href: toggle(key, v),
    })),
  });
  const allSizes = [...new Set(products.flatMap((p) => p.sizes))];
  const allColors = [...new Set(products.flatMap((p) => p.colors))];
  const filtered = sizes.length + colors.length > 0;

  return c.json({
    ...extra,
    breadcrumbs,
    facets: [
      facet("velikost", "Velikost", allSizes, sizes, (v) =>
        products.some((p) => matches(p, [v], colors)),
      ),
      facet("barva", "Barva", allColors, colors, (v) =>
        products.some((p) => matches(p, sizes, [v])),
      ),
    ],
    sort: [
      ["doporucene", "Doporučené"],
      ["nejlevnejsi", "Nejlevnější"],
      ["nejdrazsi", "Nejdražší"],
    ].map(([value, label]) => ({
      value,
      label,
      selected: sort === value,
      href: href((p) => p.set("sort", value ?? "")),
    })),
    products: list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE).map((p) => card(p, m)),
    pagination: {
      page,
      pages,
      prev: page > 1 ? href((p) => p.set("page", String(page - 1))) : null,
      next: page < pages ? href((p) => p.set("page", String(page + 1))) : null,
    },
    // Filtered URLs are noindex with a canonical to the base category (spec §9.5).
    seo: seo(
      title,
      `${title} – ${list.length} produktů`,
      `${base(c)}${basePath}`,
      [],
      filtered ? "noindex,follow" : undefined,
    ),
    cache: cache(["products", ...list.slice(0, PAGE_SIZE).map((p) => `product:${p.id}`)]),
  });
}

storefront.get("/pages/category/:slug{.+}", (c) => {
  const cat = CATEGORIES.find((k) => k.slug === c.req.param("slug"));
  if (!cat) return c.json({ code: "not_found" }, 404);
  return listing(
    c,
    PRODUCTS.filter((p) => p.category === cat.slug),
    cat.name,
    `/c/${cat.slug}`,
    [
      { label: "Domů", href: "/" },
      { label: cat.name, href: `/c/${cat.slug}` },
    ],
    {
      category: cat,
    },
  );
});

const fold = (s: string) => s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

storefront.get("/pages/search", (c) => {
  const q = fold(c.req.query("q") ?? "");
  const found = PRODUCTS.filter((p) => q && fold(`${p.name} ${p.colors.join(" ")}`).includes(q));
  return listing(
    c,
    found,
    `Hledání: ${c.req.query("q") ?? ""}`,
    "/search",
    [{ label: "Domů", href: "/" }],
    {
      category: {
        id: "search",
        slug: "search",
        name: `Výsledky pro „${c.req.query("q") ?? ""}“`,
        description: "",
      },
    },
  );
});

storefront.get("/search/suggest", (c) => {
  const m = market(c);
  const raw = (c.req.query("q") ?? "").slice(0, 100);
  const q = fold(raw);
  const products =
    q.length < 2
      ? []
      : PRODUCTS.filter((p) => fold(`${p.name} ${p.colors.join(" ")}`).includes(q)).slice(0, 5);
  return c.json({
    query: raw,
    products: products.map((p) => ({
      slug: p.slug,
      name: p.name,
      image: img(p.images[0] ?? { key: "p/hero", alt: p.name }),
      price: money(p.price, m),
    })),
    categories:
      q.length < 2
        ? []
        : CATEGORIES.filter((k) => fold(k.name).includes(q)).map((k) => ({
            label: k.name,
            href: `/c/${k.slug}`,
          })),
  });
});

storefront.get("/pages/product/:slug", (c) => {
  const m = market(c);
  const p = productBySlug.get(c.req.param("slug"));
  if (!p) return c.json({ code: "not_found" }, 404);
  const cat = CATEGORIES.find((k) => k.slug === p.category);
  const url = `${base(c)}/p/${p.slug}`;
  const pr = price(p, m);
  return c.json({
    product: {
      id: p.id,
      slug: p.slug,
      name: p.name,
      brand: p.brand,
      description_html:
        "<p>Pohodlný střih z česané organické bavlny o gramáži 180 g/m². Šito v malé dílně v Brně, barveno bez azobarviv.</p><ul><li>100% organická bavlna (GOTS)</li><li>Předsrážená látka, po vyprání nesrazí</li><li>Praní na 30 °C naruby</li></ul>",
      images: p.images.map(img),
      options: [
        { name: "Barva", values: p.colors },
        { name: "Velikost", values: p.sizes },
      ],
      variants: p.variants.map((v) => ({ ...v, ...pr })),
      parameters: [
        { name: "Materiál", value: "100% organická bavlna" },
        { name: "Gramáž", value: "180 g/m²" },
        { name: "Střih", value: "Regular" },
        { name: "Země výroby", value: "Česko" },
      ],
      gpsr: {
        manufacturer: "Lnen & Co. s.r.o., Údolní 12, 602 00 Brno",
        contact: "bezpecnost@lnen.example",
      },
    },
    breadcrumbs: [
      { label: "Domů", href: "/" },
      { label: cat?.name ?? "", href: `/c/${p.category}` },
      { label: p.name, href: `/p/${p.slug}` },
    ],
    delivery_estimate: { from: "2026-09-29", to: "2026-09-30" },
    seo: seo(`${p.name} | Lnen & Co.`, `${p.name} z organické bavlny.`, url, [
      {
        "@context": "https://schema.org",
        "@type": "Product",
        name: p.name,
        sku: p.variants[0]?.sku,
        brand: { "@type": "Brand", name: p.brand },
        image: `${base(c)}/media/${p.images[0]?.key}/1080.avif`,
        offers: {
          "@type": "Offer",
          price: (pr.price.amount_minor / 100).toFixed(2),
          priceCurrency: m.currency,
          availability: "https://schema.org/InStock",
          url,
        },
      },
    ]),
    cache: cache([`product:${p.id}`]),
  });
});

storefront.get("/redirects/resolve", (c) => c.json({ code: "not_found" }, 404));
storefront.get("/recommendations", (c) =>
  c.json({
    products: PRODUCTS.slice(8, 12).map((p) => card(p, market(c))),
    cache: cache(["products"]),
  }),
);

// --- carts (in memory; the real API stores only token hashes too) ------------------------------

interface CartState {
  id: string;
  tenant: string;
  lines: { id: string; variant_id: string; quantity: number }[];
}
/**
 * Capabilities (token hash → cart + scope). A4: the shop capability may edit lines; the
 * checkout capability minted at handoff only reads here (checkout mutations arrive in WP10),
 * and minting it revokes the shop capability, so pre-handoff tokens stop working.
 */
type Scope = "shop" | "checkout";
const carts = new Map<string, { cart: CartState; scope: Scope }>();
const hash = (t: string) => createHash("sha256").update(t).digest("hex");
const newToken = () => randomBytes(32).toString("base64url");

function cartOf(c: Context, scope?: Scope): CartState | null {
  const token = c.req.header("x-cart-token");
  const cap = token ? carts.get(hash(token)) : undefined;
  if (!cap || cap.cart.tenant !== c.req.header("x-tenant")) return null;
  return scope && cap.scope !== scope ? null : cap.cart;
}

function cartJson(c: Context, cart: CartState) {
  const m = market(c);
  const lines = cart.lines.flatMap((l) => {
    const hit = variantById.get(l.variant_id);
    if (!hit) return [];
    const unit = hit.product.price;
    return [
      {
        id: l.id,
        variant_id: l.variant_id,
        product_name: hit.product.name,
        variant_label: Object.values(hit.variant.options).join(" / "),
        image: img(
          hit.product.images[hit.variant.image_index] ??
            hit.product.images[0] ?? { key: "p/hero", alt: "" },
        ),
        quantity: l.quantity,
        unit_price: money(unit, m),
        total: money(unit * l.quantity, m),
      },
    ];
  });
  const subtotalCzk = cart.lines.reduce(
    (s, l) => s + (variantById.get(l.variant_id)?.product.price ?? 0) * l.quantity,
    0,
  );
  return c.json({
    id: cart.id,
    lines,
    item_count: cart.lines.reduce((s, l) => s + l.quantity, 0),
    subtotal: money(subtotalCzk, m),
    free_shipping_remaining: subtotalCzk >= 150000 ? null : money(150000 - subtotalCzk, m),
  });
}

storefront.post("/cart", (c) => {
  const token = newToken();
  carts.set(hash(token), {
    cart: {
      id: `cart-${randomBytes(6).toString("hex")}`,
      tenant: c.req.header("x-tenant") ?? "",
      lines: [],
    },
    scope: "shop",
  });
  return c.body(null, 201, { "x-cart-token": token });
});

storefront.get("/cart", (c) => {
  const cart = cartOf(c);
  return cart ? cartJson(c, cart) : c.json({ code: "cart_not_found" }, 404);
});

storefront.post("/cart/lines", async (c) => {
  const cart = cartOf(c, "shop");
  if (!cart) return c.json({ code: "cart_not_found" }, 404);
  const body = (await c.req.json().catch(() => null)) as {
    variant_id?: unknown;
    quantity?: unknown;
  } | null;
  const qty = Number(body?.quantity ?? 1);
  const hit = typeof body?.variant_id === "string" ? variantById.get(body.variant_id) : undefined;
  if (!hit || !Number.isInteger(qty) || qty < 1 || qty > 99)
    return c.json({ code: "invalid_line" }, 422);
  if (hit.variant.stock === "out_of_stock") return c.json({ code: "out_of_stock" }, 409);
  const existing = cart.lines.find((l) => l.variant_id === hit.variant.id);
  if (existing) existing.quantity = Math.min(99, existing.quantity + qty);
  else
    cart.lines.push({
      id: `line-${randomBytes(4).toString("hex")}`,
      variant_id: hit.variant.id,
      quantity: qty,
    });
  return cartJson(c, cart);
});

storefront.patch("/cart/lines/:id", async (c) => {
  const cart = cartOf(c, "shop");
  if (!cart) return c.json({ code: "cart_not_found" }, 404);
  const qty = Number(
    ((await c.req.json().catch(() => null)) as { quantity?: unknown } | null)?.quantity,
  );
  if (!Number.isInteger(qty) || qty < 0 || qty > 99)
    return c.json({ code: "invalid_quantity" }, 422);
  cart.lines = cart.lines.flatMap((l) =>
    l.id !== c.req.param("id") ? [l] : qty === 0 ? [] : [{ ...l, quantity: qty }],
  );
  return cartJson(c, cart);
});

storefront.delete("/cart/lines/:id", (c) => {
  const cart = cartOf(c, "shop");
  if (!cart) return c.json({ code: "cart_not_found" }, 404);
  cart.lines = cart.lines.filter((l) => l.id !== c.req.param("id"));
  return cartJson(c, cart);
});

/** Handoff (A1): a new checkout-scoped capability for the same cart. */
storefront.post("/cart/checkout-token", (c) => {
  const cart = cartOf(c, "shop");
  if (!cart) return c.json({ code: "cart_not_found" }, 404);
  const token = newToken();
  carts.delete(hash(c.req.header("x-cart-token") ?? "")); // rotate: the shop capability dies
  carts.set(hash(token), { cart, scope: "checkout" });
  return c.json({ token });
});

storefront.post("/newsletter/subscribe", async (c) => {
  const body = (await c.req.json().catch(() => null)) as { email?: unknown } | null;
  const email = typeof body?.email === "string" ? body.email.trim() : "";
  if (!/^[^@\s]{1,64}@[^@\s]{1,190}\.[^@\s]{2,}$/.test(email))
    return c.json({ code: "invalid_email" }, 422);
  return c.json({ status: "confirmation_sent" }, 202);
});

let events = 0;
storefront.post("/events", async (c) => {
  events += 1;
  await c.req.arrayBuffer();
  return c.body(null, 202);
});
storefront.get("/_stats", (c) => c.json({ events, carts: carts.size }));

storefront.get("/files/robots.txt", (c) =>
  c.text(`User-agent: *\nAllow: /\nDisallow: /_p/\nSitemap: ${base(c)}/sitemap.xml\n`),
);
storefront.get("/files/llms.txt", (c) =>
  c.text(
    "# Lnen & Co.\n\n> Oblečení z organické bavlny.\n\n- [Trička](/c/trika)\n- [Mikiny](/c/mikiny)\n",
  ),
);

// --- media (the real platform serves re-encoded variants from the public bucket) ---------------

export const media = new Hono();
const MEDIA_DIR = path.resolve(
  process.env.MEDIA_DIR ?? path.join(import.meta.dirname, "../../../../fixtures/media"),
);

media.get("/media/:path{.+}", async (c) => {
  const rel = c.req.param("path");
  if (!/^[a-z0-9/_-]+\.avif$/.test(rel) || rel.includes("..")) return c.body(null, 404);
  const buf = await readFile(path.join(MEDIA_DIR, rel)).catch(() => null);
  return buf
    ? c.body(buf, 200, {
        "content-type": "image/avif",
        "cache-control": "public, max-age=31536000, immutable",
      })
    : c.body(null, 404);
});
