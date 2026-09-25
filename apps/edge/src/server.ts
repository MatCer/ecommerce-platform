import { serve } from "@hono/node-server";
import { ArtifactFetcher } from "./artifacts.ts";
import { Counters } from "./counters.ts";
import { createGateway } from "./gateway.ts";
import { ApiResolver } from "./sites.ts";

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name}`);
  return v;
}

const artifactRoot = required("ARTIFACT_ROOT");
const apiOrigin = required("API_ORIGIN");
const serviceToken = required("INTERNAL_API_TOKEN");
const counters = new Counters();
const gateway = createGateway({
  artifactRoot,
  // Sites (tenant, market, token, active artifacts) come from the API (spec §8.4).
  resolver: new ApiResolver(apiOrigin, serviceToken),
  // Artifacts are downloaded from the private bucket through the API on first use (A22).
  artifacts: new ArtifactFetcher({ root: artifactRoot, apiOrigin, token: serviceToken }),
  apiOrigin,
  mediaOrigin: required("MEDIA_ORIGIN"),
  scheme: process.env.PUBLIC_SCHEME === "http" ? "http" : "https",
  purgeToken: required("EDGE_PURGE_TOKEN"),
  packetaWidgetUrl: process.env.PACKETA_WIDGET_URL || undefined,
  counters,
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

// A20: cookieless counters go to the API every 30 s (kept for the next flush on failure).
const flushCounters = () =>
  counters
    .flush(apiOrigin, serviceToken)
    .catch((err) =>
      console.log(JSON.stringify({ level: "warn", msg: "counter flush failed", err: String(err) })),
    );
const flushing = setInterval(() => void flushCounters(), 30_000);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    clearInterval(idle);
    clearInterval(flushing);
    for (const s of servers) s.close();
    void flushCounters()
      .then(() => gateway.dispose())
      .finally(() => process.exit(0));
  });
}
