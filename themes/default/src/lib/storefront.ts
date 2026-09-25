import { env } from "cloudflare:workers";
import { createStorefront } from "@platform/storefront-sdk/server";

/** Page-model access for SSR. The platform injects tenant, market, locale and credentials. */
export const storefront = (request: Request) =>
  createStorefront({ binding: env.STOREFRONT, request });

/** Serializes JSON-LD safely inside a `<script type="application/ld+json">`. */
export const jsonLd = (data: unknown) => JSON.stringify(data).replace(/</g, "\\u003c");

/** The listing query (filters `f.*`, `sort`, `page`, `q`) as the page model expects it. */
export const listingQuery = (url: URL) =>
  Object.fromEntries(
    [...new Set(url.searchParams.keys())].map((k) => [k, url.searchParams.getAll(k)]),
  );
