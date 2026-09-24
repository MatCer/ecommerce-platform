/// <reference types="astro/client" />

// The only bindings a theme worker receives (spec A7). Provided by the platform edge.
declare module "cloudflare:workers" {
  export const env: {
    STOREFRONT: import("@platform/storefront-sdk/server").StorefrontBinding;
  };
}

declare module "virtual:theme-tokens.css";
