import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    name: "edge",
    include: ["src/**/*.test.ts", "test/**/*.test.ts"],
    // Tests start real workerd processes (one per artifact).
    testTimeout: 30_000,
    hookTimeout: 60_000,
  },
});
