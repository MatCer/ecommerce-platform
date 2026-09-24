import { serve } from "@hono/node-server";
import { createGateway } from "./gateway.ts";
import { StaticResolver } from "./sites.ts";

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name}`);
  return v;
}

const gateway = createGateway({
  artifactRoot: required("ARTIFACT_ROOT"),
  // ponytail: static host map until WP6 serves GET /internal/v1/resolve (same Site shape).
  resolver: await StaticResolver.fromFile(required("SITES_FILE")),
  checkoutArtifact: required("CHECKOUT_ARTIFACT"),
  apiOrigin: required("API_ORIGIN"),
  mediaOrigin: process.env.MEDIA_ORIGIN ?? required("API_ORIGIN"),
  scheme: process.env.PUBLIC_SCHEME === "http" ? "http" : "https",
  purgeToken: required("EDGE_PURGE_TOKEN"),
});

const port = Number(process.env.PORT ?? 8787);
const adminPort = Number(process.env.ADMIN_PORT ?? 8788);
const servers = [
  serve({ fetch: gateway.fetch, port }),
  serve({ fetch: gateway.admin, port: adminPort }),
];
console.log(JSON.stringify({ level: "info", msg: "edge listening", port, adminPort }));

// A30: one instance per artifact; idle ones are disposed and recreated on demand.
const idle = setInterval(() => void gateway.pool.evictIdle(10 * 60_000), 60_000);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    clearInterval(idle);
    for (const s of servers) s.close();
    void gateway.dispose().finally(() => process.exit(0));
  });
}
