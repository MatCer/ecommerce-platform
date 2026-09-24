import createClient, { type ClientOptions } from "openapi-fetch";
import type { paths } from "./schema";

export type { components, paths } from "./schema";

/** Typed Storefront API client; types are generated from the API's /openapi.json (`make openapi`). */
export function createStorefrontClient(options: ClientOptions = {}) {
  return createClient<paths>(options);
}
