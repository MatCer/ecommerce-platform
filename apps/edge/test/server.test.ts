import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";

test("production refuses the local Miniflare runtime", () => {
  const server = fileURLToPath(new URL("../src/server.ts", import.meta.url));
  const result = spawnSync(process.execPath, [server], {
    encoding: "utf8",
    timeout: 5000,
    env: {
      ...process.env,
      APP_ENV: "prod",
      E2E_RATE_SECRET: "",
      ARTIFACT_ROOT: "/tmp/unused-artifacts",
      API_ORIGIN: "http://127.0.0.1:1",
      INTERNAL_API_TOKEN: "test-token",
    },
  });
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("production requires managed Workers runtime limits");
});
