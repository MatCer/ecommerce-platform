import type { CategoryPage, HomePage, ProductPage, SearchSuggest, ShopModel } from "./types.ts";

/** The `STOREFRONT` service binding the platform injects into theme workers (spec A7). */
export interface StorefrontBinding {
  fetch(input: string, init?: RequestInit): Promise<Response>;
}

export class StorefrontError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string) {
    super(`storefront API ${status} ${code}`);
    this.status = status;
    this.code = code;
  }
}

/** Header the edge uses to tie binding calls to the page request (see apps/edge bindings.ts). */
const CTX_HEADER = "x-platform-ctx";

type Query = Record<string, string | string[] | undefined>;

function qs(query: Query = {}): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    for (const x of Array.isArray(v) ? v : v === undefined ? [] : [v]) p.append(k, x);
  }
  const s = p.toString();
  return s ? `?${s}` : "";
}

const segment = (slug: string) => slug.split("/").map(encodeURIComponent).join("/");

/**
 * Typed page-model fetchers for theme SSR. Tenant, market and credentials are added by the
 * platform; a theme only names the page it wants.
 *
 * ```astro
 * ---
 * import { env } from "cloudflare:workers";
 * const sf = createStorefront({ binding: env.STOREFRONT, request: Astro.request });
 * const page = await sf.product(Astro.params.slug);
 * if (!page) return Astro.rewrite("/404");
 * ---
 * ```
 */
export function createStorefront({
  binding,
  request,
}: {
  binding: StorefrontBinding;
  request: Request;
}) {
  const ctx = request.headers.get(CTX_HEADER) ?? "";

  async function get<T>(path: string, query?: Query): Promise<T | null> {
    const res = await binding.fetch(`https://storefront${path}${qs(query)}`, {
      headers: { accept: "application/json", [CTX_HEADER]: ctx },
    });
    if (res.status === 404) return null;
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { code?: string };
      throw new StorefrontError(res.status, body.code ?? "error");
    }
    return (await res.json()) as T;
  }

  async function required<T>(path: string): Promise<T> {
    const v = await get<T>(path);
    if (v === null) throw new StorefrontError(404, "not_found");
    return v;
  }

  return {
    shop: () => required<ShopModel>("/shop"),
    home: () => required<HomePage>("/pages/home"),
    category: (slug: string, query?: Query) =>
      get<CategoryPage>(`/pages/category/${segment(slug)}`, query),
    product: (slug: string) => get<ProductPage>(`/pages/product/${segment(slug)}`),
    search: (query: Query) => required<CategoryPage>(`/pages/search${qs(query)}`),
    suggest: (q: string) => required<SearchSuggest>(`/search/suggest${qs({ q })}`),
  };
}

export type Storefront = ReturnType<typeof createStorefront>;
