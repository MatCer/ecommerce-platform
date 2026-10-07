// Platform-owned (theme contract §9.1): themes edit src/**, public/** and theme.tokens.json only.
import cloudflare from "@astrojs/cloudflare";
import solid from "@astrojs/solid-js";
import { themeTokens } from "@platform/theme-kit/vite";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";

export default defineConfig({
  output: "server",
  // No IMAGES/SESSION bindings: theme workers get only STOREFRONT (+ read-only ASSETS), spec A7.
  adapter: cloudflare({ imageService: "passthrough" }),
  session: false,
  integrations: [solid()],
  // External stylesheets only: inline <style> would need per-build CSP hashes.
  build: { inlineStylesheets: "never" },
  // Prefetch is the edge's job (Speculation-Rules header, A26); Astro's would add JS.
  prefetch: false,
  vite: { plugins: [tailwindcss(), themeTokens()] },
});
