import { defineConfig } from "vitest/config";

// One vitest run for the whole pnpm workspace; each package/app is a project.
export default defineConfig({
  test: {
    projects: ["packages/*", "apps/*"],
  },
});
