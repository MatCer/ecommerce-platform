import { serve } from "@hono/node-server";
import { app } from "./app.ts";

const port = Number(process.env.PORT ?? 4010);
const server = serve({ fetch: app.fetch, port }, (info) => {
  console.log(JSON.stringify({ level: "info", msg: "mocks listening", port: info.port }));
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => server.close(() => process.exit(0)));
}
