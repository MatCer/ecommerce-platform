import { serve } from "@hono/node-server";
import { getMigrations } from "better-auth/db/migration";
import pg from "pg";
import { createApp } from "./app.ts";
import { createAuth } from "./auth.ts";
import { loadConfig } from "./config.ts";
import { smtpMailer } from "./mail.ts";

const log = (msg: string, extra: Record<string, unknown> = {}) =>
  console.log(JSON.stringify({ level: "info", msg, ...extra }));

const cfg = loadConfig();
const pool = new pg.Pool({ connectionString: cfg.databaseUrl, max: 10 });
const auth = createAuth(cfg, pool, smtpMailer(cfg.smtpUrl, cfg.mailFrom));

// Better Auth owns the `auth` schema (the role's search_path); apply its schema on start.
const { toBeCreated, toBeAdded, runMigrations } = await getMigrations(auth.options);
if (toBeCreated.length > 0 || toBeAdded.length > 0) {
  await runMigrations();
  log("auth schema migrated", {
    created: toBeCreated.map((t) => t.table),
    altered: toBeAdded.map((t) => t.table),
  });
}

const server = serve({ fetch: createApp(auth, cfg).fetch, port: cfg.port }, (info) =>
  log("auth listening", { port: info.port }),
);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => server.close(() => pool.end().then(() => process.exit(0))));
}
