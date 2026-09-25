import { env } from "cloudflare:workers";
import { createStorefront } from "@platform/storefront-sdk/server";

/** Page-model access for SSR. The platform injects tenant, market and credentials. */
export const storefront = (request: Request) =>
  createStorefront({ binding: env.STOREFRONT, request });

/** Serializes JSON-LD safely inside a `<script type="application/ld+json">`. */
export const jsonLd = (data: unknown) => JSON.stringify(data).replace(/</g, "\\u003c");
