import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

// Dev: `pnpm --filter @platform/admin dev` against the Docker stack. Better Auth is reached on
// the admin origin (`/api/auth/*`, see docker/caddy/Caddyfile), so the dev server proxies it;
// the auth service must trust the dev origin (its ADMIN_ORIGIN) for sign-in to work.
const authTarget = process.env.ADMIN_AUTH_PROXY ?? "http://auth.localhost:8080";

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  server: {
    proxy: { "/api/auth": { target: authTarget, changeOrigin: true } },
  },
  build: { target: "es2022", sourcemap: true },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
