import { defineConfig, devices } from "@playwright/test";

// End-to-end suites against the running compose stack (`make up`, then `make e2e`).
// Ports come from the environment / .env (HTTP_PORT, MAILPIT_UI_PORT).
const port = process.env.HTTP_PORT ?? "8080";

export default defineConfig({
  testDir: ".",
  testMatch: "**/*.spec.ts",
  // Machine budget (spec §15): at most 4 workers.
  workers: 4,
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: [["list"]],
  use: {
    baseURL: `http://admin.localhost:${port}`,
    trace: "retain-on-failure",
    locale: "en-US",
    viewport: { width: 1280, height: 860 },
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 860 } },
    },
  ],
});
