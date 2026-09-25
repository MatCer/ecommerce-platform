import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    name: "theme-builder",
    include: ["src/**/*.test.ts", "test/**/*.test.ts"],
  },
});
