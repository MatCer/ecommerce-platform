import type { Hono } from "hono";

/**
 * Local stand-in for the Packeta pickup-point widget (spec §10.5, WP10). Same entry point as
 * the real library (`Packeta.Widget.pick(apiKey, callback, options)`, callback gets a point
 * object or `null`), so the checkout code is identical against the mock and the real widget;
 * `PACKETA_WIDGET_URL` picks which one loads.
 *
 * The library opens a modal iframe of `/packeta/widget`, whose keyboard-operable list posts
 * the chosen point back to the opening page only (its origin is passed as `origin`).
 */

interface Point {
  id: string;
  name: string;
  street: string;
  city: string;
  zip: string;
  country: string;
}

const POINTS: Record<string, Point[]> = {
  cz: [
    {
      id: "4101",
      name: "Z-BOX Praha 1, Dlouhá",
      street: "Dlouhá 1",
      city: "Praha",
      zip: "110 00",
      country: "cz",
    },
    {
      id: "4102",
      name: "Trafika Vinohrady",
      street: "Vinohradská 48",
      city: "Praha",
      zip: "120 00",
      country: "cz",
    },
    {
      id: "4103",
      name: "Z-BOX Brno, Náměstí Svobody",
      street: "Náměstí Svobody 5",
      city: "Brno",
      zip: "602 00",
      country: "cz",
    },
  ],
  sk: [
    {
      id: "5101",
      name: "Z-BOX Bratislava, Obchodná",
      street: "Obchodná 12",
      city: "Bratislava",
      zip: "811 06",
      country: "sk",
    },
    {
      id: "5102",
      name: "Papiernictvo Košice",
      street: "Hlavná 30",
      city: "Košice",
      zip: "040 01",
      country: "sk",
    },
  ],
};

/** An http(s) origin, or null. */
export function safeOrigin(raw: string | undefined): string | null {
  if (!raw) return null;
  try {
    const u = new URL(raw);
    return (u.protocol === "http:" || u.protocol === "https:") && u.origin === raw
      ? u.origin
      : null;
  } catch {
    return null;
  }
}

const esc = (v: string) =>
  v.replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] ?? c,
  );

const LIBRARY = `(() => {
  const script = document.currentScript;
  const base = new URL(".", script ? script.src : location.href);
  const widgetOrigin = base.origin;
  let open = null;
  function close(point) {
    if (!open) return;
    const { overlay, callback, onMessage, before, inerted } = open;
    open = null;
    window.removeEventListener("message", onMessage);
    overlay.remove();
    for (const el of inerted) el.inert = false;
    if (before && typeof before.focus === "function") before.focus();
    callback(point);
  }
  function pick(apiKey, callback, options) {
    if (open) close(null);
    const opts = options || {};
    const url = new URL("widget", base);
    url.searchParams.set("apiKey", String(apiKey || ""));
    url.searchParams.set("country", String(opts.country || "cz"));
    url.searchParams.set("language", String(opts.language || "cs"));
    url.searchParams.set("origin", location.origin);
    const overlay = document.createElement("div");
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-modal", "true");
    overlay.setAttribute("aria-label", "Packeta");
    overlay.dataset.packetaWidget = "";
    Object.assign(overlay.style, { position: "fixed", inset: "0", zIndex: "2147483647",
      background: "rgba(0,0,0,.45)", display: "flex", alignItems: "center", justifyContent: "center" });
    const frame = document.createElement("iframe");
    frame.src = url.toString();
    frame.title = "Packeta";
    Object.assign(frame.style, { width: "min(36rem, 96vw)", height: "min(34rem, 90vh)",
      border: "0", borderRadius: "8px", background: "#fff" });
    overlay.append(frame);
    const onMessage = (event) => {
      if (event.origin !== widgetOrigin || event.source !== frame.contentWindow) return;
      const data = event.data || {};
      if (data.type === "packeta.point") close(data.point || null);
      else if (data.type === "packeta.close") close(null);
    };
    // A modal: the page behind it leaves the tab order and the accessibility tree.
    const inerted = Array.from(document.body.children).filter((el) => !el.inert);
    for (const el of inerted) el.inert = true;
    open = { overlay, callback, onMessage, before: document.activeElement, inerted };
    window.addEventListener("message", onMessage);
    overlay.addEventListener("keydown", (e) => { if (e.key === "Escape") close(null); });
    document.body.append(overlay);
    frame.addEventListener("load", () => frame.focus());
  }
  window.Packeta = { Widget: { pick, close: () => close(null) } };
})();
`;

function widgetPage(country: string, origin: string | null): string {
  const points = POINTS[country] ?? [];
  const items = points
    .map(
      (p, i) =>
        `<li><button type="button" data-i="${i}"${i === 0 ? " autofocus" : ""}><strong>${esc(p.name)}</strong><br>${esc(`${p.street}, ${p.zip} ${p.city}`)}</button></li>`,
    )
    .join("");
  return `<!doctype html><html lang="cs"><meta charset="utf-8"><title>Výběr výdejního místa</title>
<style>body{font:15px/1.4 system-ui,sans-serif;margin:0;padding:1rem}ul{list-style:none;padding:0;margin:0;display:grid;gap:.5rem}
button{font:inherit;text-align:left;width:100%;min-height:2.75rem;padding:.5rem .75rem;border:1px solid #c4c8ce;border-radius:.5rem;background:#fff;cursor:pointer}
button:hover{border-color:#2b5aa8}:focus-visible{outline:3px solid #2b5aa8;outline-offset:2px}.close{width:auto;margin-top:1rem}</style>
<main><h1 style="font-size:1.25rem">Výdejní místa (mock)</h1><ul>${items}</ul>
<button type="button" class="close" data-close>Zavřít</button></main>
<script>
const points = ${JSON.stringify(points).replace(/</g, "\\u003c")};
const target = ${JSON.stringify(origin)};
const send = (msg) => { if (target) parent.postMessage(msg, target); };
document.querySelectorAll("button[data-i]").forEach((b) =>
  b.addEventListener("click", () => send({ type: "packeta.point", point: points[Number(b.dataset.i)] })));
document.querySelector("[data-close]").addEventListener("click", () => send({ type: "packeta.close" }));
// A modal dialog: Escape closes it, Tab and Shift+Tab cycle inside it.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") return send({ type: "packeta.close" });
  if (e.key !== "Tab") return;
  const all = Array.from(document.querySelectorAll("button"));
  const first = all[0], last = all[all.length - 1];
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});
</script></html>`;
}

export function packetaRoutes(app: Hono) {
  app.get("/packeta/library.js", (c) =>
    c.body(LIBRARY, 200, {
      "content-type": "text/javascript; charset=utf-8",
      "cache-control": "no-store",
    }),
  );
  app.get("/packeta/widget", (c) => {
    const country = (c.req.query("country") ?? "cz").toLowerCase();
    return c.html(widgetPage(country, safeOrigin(c.req.query("origin"))), 200, {
      "cache-control": "no-store",
    });
  });
}
