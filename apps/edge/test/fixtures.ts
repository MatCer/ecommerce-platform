import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { type ArtifactKind, packArtifact } from "@platform/theme-kit";
import type { Site } from "../src/sites.ts";

// Resolves the pinned Astro/Solid for CSP hashes, exactly like a real theme build.
const THEME_PROJECT = fileURLToPath(new URL("../../../themes/default", import.meta.url));

/**
 * A hostile "theme" used to prove the trust boundary (spec A7): besides rendering pages it
 * tries every escape we can think of and reports what happened at `/probe`.
 */
export const hostileTheme = (version: string) => `
import { connect } from "cloudflare:sockets";
const status = async (p) => { try { return (await p).status; } catch (e) { return "threw:" + e.message.slice(0, 60); } };
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const ctx = request.headers.get("x-platform-ctx");
    const sf = (p, init = {}) => env.STOREFRONT.fetch("https://storefront" + p, { ...init, headers: { "x-platform-ctx": ctx, ...(init.headers || {}) } });
    const html = (body, headers = {}) => new Response("<!doctype html>" + body, { headers: { "content-type": "text/html", ...headers } });
    switch (url.pathname) {
      case "/": {
        const shop = await (await sf("/shop")).json();
        return html("<h1>" + shop.name + "</h1><script src=\\"/_astro/app.${version}.js\\"></script>");
      }
      case "/p/private": { await sf("/pages/product/private"); return html("private"); }
      case "/p/short": { await sf("/pages/product/short"); return html("short"); }
      case "/p/theme-no-store": return html("x", { "cache-control": "no-store" });
      case "/p/sets-cookie": return html("x", { "set-cookie": "evil=1; Domain=.demo.localhost; Path=/" });
      case "/c/slow": { await new Promise((r) => setTimeout(r, 5000)); return html("slow"); }
      case "/c/endless": return new Response(new ReadableStream({ pull: (c) => new Promise((r) => setTimeout(() => { c.enqueue(new TextEncoder().encode("<p>")); r(); }, 100)) }), { headers: { "content-type": "text/html" } });
      case "/c/huge": { const chunk = new Uint8Array(1024 * 1024); let n = 0; return new Response(new ReadableStream({ pull(c) { if (n++ < 8) c.enqueue(chunk); else c.close(); } })); }
      case "/c/fanout": {
        const statuses = [];
        for (let i = 0; i < 52; i++) statuses.push((await sf("/shop")).status);
        return Response.json({ ok: statuses.filter((s) => s === 200).length, last: statuses.at(-1) });
      }
      case "/pages/headers": return Response.json({ headers: [...request.headers], url: request.url });
      case "/probe": {
        return Response.json({
          envKeys: Object.keys(env).sort(),
          externalFetch: await status(fetch("https://example.com/")),
          apiAdminDirect: await status(fetch("http://api.localhost/admin/v1/products")),
          internalDirect: await status(fetch("http://api:8000/internal/v1/resolve?host=demo.localhost")),
          tcpConnect: await status((async () => {
            const s = connect("example.com:80");
            await s.opened;
            const w = s.writable.getWriter();
            await w.write(new TextEncoder().encode("GET / HTTP/1.0\\r\\nHost: example.com\\r\\n\\r\\n"));
            const { value } = await s.readable.getReader().read();
            return { status: "read:" + new TextDecoder().decode(value ?? new Uint8Array()).slice(0, 40) };
          })()),
          adminViaBinding: await status(sf("/admin/v1/products")),
          internalViaBinding: await status(sf("/internal/v1/resolve?host=other.localhost")),
          dotDot: await status(sf("/pages/../../admin/v1/products")),
          encodedDotDot: await status(sf("/pages/category/%2e%2e%2f%2e%2e%2fadmin")),
          forgedTenantHeader: await status(sf("/shop", { headers: { "x-tenant": "t-other" } })),
          forgedStorefrontToken: await status(sf("/shop", { headers: { "x-storefront-token": "sf_other" } })),
          authorization: await status(sf("/shop", { headers: { authorization: "Bearer stolen" } })),
          cookie: await status(sf("/shop", { headers: { cookie: "cart=abc" } })),
          forgedContext: await status(env.STOREFRONT.fetch("https://storefront/shop", { headers: { "x-platform-ctx": "forged-context-id" } })),
          noContext: await status(env.STOREFRONT.fetch("https://storefront/shop")),
          cartViaBinding: await status(sf("/cart")),
          postViaBinding: await status(sf("/shop", { method: "POST", body: "{}" })),
          allowedShop: await status(sf("/shop")),
        });
      }
      default:
        return new Response("not found", { status: 404 });
    }
  },
};
`;

export const checkoutWorker = `
export default {
  async fetch(request, env) {
    const ctx = request.headers.get("x-platform-ctx");
    const cart = await env.CHECKOUT.fetch("https://checkout/cart", { headers: { "x-platform-ctx": ctx } });
    const me = await env.CHECKOUT.fetch("https://checkout/customer/me", { headers: { "x-platform-ctx": ctx } });
    return new Response("<!doctype html><title>Pokladna</title><pre>" + (await cart.text()) + (await me.text()) + "</pre>", {
      headers: { "content-type": "text/html" },
    });
  },
};
`;

export async function buildArtifact(
  root: string,
  kind: ArtifactKind,
  entry: string,
  assets: Record<string, string>,
) {
  const dist = await mkdtemp(path.join(tmpdir(), "wp2-dist-"));
  await mkdir(path.join(dist, "server"), { recursive: true });
  await writeFile(path.join(dist, "server", "entry.mjs"), entry);
  for (const [p, body] of Object.entries(assets)) {
    await mkdir(path.dirname(path.join(dist, "client", p)), { recursive: true });
    await writeFile(path.join(dist, "client", p), body);
  }
  const tokens = path.join(dist, "theme.tokens.json");
  await writeFile(
    tokens,
    JSON.stringify({
      colors: { brand: "#123456" },
      fonts: { sans: "system-ui" },
      radius: { control: "4px" },
    }),
  );
  return packArtifact({
    dist,
    outRoot: root,
    kind,
    projectDir: THEME_PROJECT,
    tokensFile: kind === "theme" ? tokens : undefined,
  });
}

export const site = (over: Partial<Site> = {}): Site => ({
  tenant_id: "t-demo",
  market_id: "m-cz",
  locale: "cs",
  locales: ["cs"],
  shop_host: "demo.localhost",
  storefront_token: "sf_demo_public",
  theme_artifact: "",
  retained_artifacts: [],
  checkout_artifact: null,
  ...over,
});

/** Records every upstream call and answers like the Storefront API. */
export function fakeApi() {
  const calls: { method: string; url: string; headers: Record<string, string>; body: string }[] =
    [];
  let carts = 0;
  // Handoff tokens (A1): single use, bound to the tenant + market that minted them.
  const handoffs = new Map<string, { tenant: string | null; market: string | null }>();
  const fn = async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    calls.push({
      method: req.method,
      url: req.url,
      headers: Object.fromEntries(req.headers),
      body: await req.text(),
    });
    const tenant = req.headers.get("x-tenant");
    const p = url.pathname.replace("/storefront/v1", "");
    if (p === "/shop")
      return Response.json({
        name: `Shop of ${tenant}`,
        cache: { public: true, max_age: 60, tags: ["shop"] },
      });
    if (p === "/pages/product/private")
      return Response.json({ cache: { public: false, max_age: 0, tags: [] } });
    if (p === "/pages/product/limited")
      return Response.json(
        { code: "rate_limited", status: 429 },
        {
          status: 429,
          headers: { "content-type": "application/problem+json", "retry-after": "7" },
        },
      );
    if (p === "/pages/product/short")
      return Response.json({ cache: { public: true, max_age: 5, tags: ["product:short"] } });
    if (p === "/cart" && req.method === "POST") {
      carts++;
      return new Response(null, {
        status: 201,
        headers: { "x-cart-token": `carttoken_${String(carts).padStart(20, "0")}` },
      });
    }
    if (p === "/cart")
      return Response.json({ token_seen: req.headers.get("x-cart-token"), lines: [] });
    if (p === "/cart/lines")
      return Response.json({ token_seen: req.headers.get("x-cart-token"), lines: [{ id: "l1" }] });
    if (p === "/cart/handoff") {
      const token = `handofftoken_${String(handoffs.size + 1).padStart(20, "0")}`;
      handoffs.set(token, { tenant, market: req.headers.get("x-market") });
      return Response.json({ token });
    }
    if (p === "/checkout/handoff") {
      const { token } = JSON.parse(calls.at(-1)?.body || "{}") as { token?: string };
      const h = token ? handoffs.get(token) : undefined;
      if (!h || h.tenant !== tenant || h.market !== req.headers.get("x-market"))
        return Response.json({ code: "invalid_handoff" }, { status: 400 });
      handoffs.delete(token as string);
      return Response.json({ cart_token: "checkouttoken_000000000001" });
    }
    if (p === "/redirects/resolve") {
      const path = url.searchParams.get("path");
      if (path === "/stary-produkt") return Response.json({ to_path: "/p/novy", code: 301 });
      if (path === "/cs/stary") return Response.json({ to_path: "/cs/c/novy", code: 301 });
      if (path === "/do-cestiny") return Response.json({ to_path: "/cs/p/novy", code: 301 });
      if (path === "/docasne") return Response.json({ to_path: "/c/akce?x=1", code: 302 });
      if (path === "/podvrh") return Response.json({ to_path: "//evil.example/", code: 301 });
    }
    if (p === "/customer/login")
      return Response.json(
        { customer: { email: "jana@example.cz" }, redirect: "/account" },
        { headers: { "x-session-token": "sessiontoken_000000000001" } },
      );
    if (p === "/customer/me")
      return req.headers.get("x-customer-session") === "sessiontoken_000000000001"
        ? Response.json({ email: "jana@example.cz", session_seen: true })
        : Response.json({ code: "not_signed_in" }, { status: 401 });
    if (p === "/customer/logout")
      return new Response(null, { status: 204, headers: { "x-session-clear": "1" } });
    if (p === "/consent")
      return Response.json(
        { purposes: { analytics: true }, text_version: "2026-09-25" },
        {
          headers: {
            "x-consent-subject":
              req.headers.get("x-consent-subject") ?? "0123456789abcdef0123456789abcdef",
            "x-consent-summary": "analytics,personalization",
          },
        },
      );
    if (p === "/events") return new Response(null, { status: 202 });
    // WP10: checkout, order page and the fake gateway.
    if (p === "/checkout/place-order")
      return Response.json(
        { order_id: "o1", confirmation_url: `/o/${"a".repeat(64)}` },
        { status: 201, headers: { "idempotent-replayed": "true", "set-cookie": "x=1" } },
      );
    if (p.startsWith("/checkout/fake-pay/") && req.method === "GET")
      return Response.json({
        order_number: "<b>100001</b>",
        amount: { formatted: "10 Kč" },
        status: "pending",
      });
    if (p.startsWith("/checkout") || p.startsWith("/orders/"))
      return Response.json({ path: p, cart_seen: req.headers.get("x-cart-token") });
    // WP18: newsletter links (token "a…a" is valid, anything else is not).
    if (p === "/newsletter/click") {
      const q = new URL(req.url).searchParams;
      return q.get("s") === "good"
        ? Response.json({ url: q.get("u") })
        : Response.json({ code: "not_found" }, { status: 404 });
    }
    if (["/newsletter/unsubscribe", "/newsletter/confirmation", "/newsletter/resubscribe"].includes(p)) {
      const token = (JSON.parse(calls.at(-1)?.body || "{}") as { token?: string }).token;
      return token === "a".repeat(64)
        ? Response.json({ status: "ok" }, { status: p.endsWith("resubscribe") ? 202 : 200 })
        : Response.json({ code: "not_found" }, { status: 404 });
    }
    if (p === "/newsletter/subscribe")
      return Response.json(
        { tenant, body: JSON.parse(calls.at(-1)?.body || "null") },
        { status: 202 },
      );
    return Response.json({ code: "not_found" }, { status: 404 });
  };
  return { fn, calls };
}
