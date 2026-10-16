import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [solid({ ssr: true })],
  test: { name: "storefront-sdk", environment: "node" },
});
