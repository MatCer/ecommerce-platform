/// <reference types="astro/client" />

// The checkout app's binding (spec A7: separate from the theme's STOREFRONT).
declare module "cloudflare:workers" {
  export const env: {
    CHECKOUT: import("@platform/storefront-sdk/server").StorefrontBinding;
  };
}
