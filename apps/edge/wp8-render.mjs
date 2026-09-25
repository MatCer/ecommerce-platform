// Temporary WP8 debug harness (not committed): renders the built theme in workerd with a
// STOREFRONT binding proxied to the running stack's /_p/public routes.
import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { Log, LogLevel, Miniflare, Response as MfResponse } from "miniflare";

const serverDir = path.resolve(process.argv[2]);
const clientDir = path.resolve(serverDir, "../client");
const host = process.argv[3] ?? "demo.localhost:8880";
const pages = process.argv.slice(4);
const modules = {};
const walk = (d, rel = "") => {
  for (const f of readdirSync(path.join(d, rel))) {
    const r = rel ? `${rel}/${f}` : f;
    if (statSync(path.join(d, r)).isDirectory()) walk(d, r);
    else if (r.endsWith(".mjs")) modules[r] = { type: "esm", contents: readFileSync(path.join(d, r), "utf8") };
  }
};
walk(serverDir);
const mf = new Miniflare({
  port: 0,
  cf: false,
  log: new Log(LogLevel.DEBUG),
  workers: [
    {
      config: {
        name: "t",
        compatibilityDate: "2026-09-21",
        manifest: { mainModule: "entry.mjs", modulesRoot: serverDir, modules },
        env: {
          STOREFRONT: {
            type: "fetcher",
            handler: async (req) => {
              const u = new URL(req.url);
              const r = await fetch(`http://${host}/_p/public${u.pathname}${u.search}`);
              return new MfResponse(await r.text(), { status: r.status, headers: { "content-type": "application/json" } });
            },
          },
          ASSETS: {
            type: "fetcher",
            handler: async (req) => {
              try {
                return new MfResponse(readFileSync(path.join(clientDir, new URL(req.url).pathname)));
              } catch {
                return new MfResponse("nf", { status: 404 });
              }
            },
          },
        },
      },
    },
  ],
});
for (const p of pages) {
  const t0 = Date.now();
  const res = await Promise.race([
    mf.dispatchFetch(`http://${host}${p}`),
    new Promise((r) => setTimeout(() => r(null), 8000)),
  ]);
  if (!res) console.log(p, "TIMEOUT");
  else {
    const body = await res.text();
    console.log(p, res.status, Date.now() - t0, "ms", body.length, "bytes");
    if (process.env.DUMP) console.log(body);
  }
}
await mf.dispose();
