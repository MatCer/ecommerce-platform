/**
 * Storefront page models (spec §8.2). Hand-written stubs for WP2; WP6 replaces them with types
 * generated from the API's OpenAPI document (the shapes are meant to stay the same).
 */

/** Spec §8.1: `{ amount_minor: 12900, currency: "CZK", formatted: "129,00 Kč" }`. */
export interface Money {
  amount_minor: number;
  currency: string;
  formatted: string;
}

/** An image asset with pre-generated AVIF variants (`widths`) under `/media/<key>/<w>.avif`. */
export interface Image {
  key: string;
  alt: string;
  width: number;
  height: number;
  widths: number[];
}

export interface Seo {
  title: string;
  description: string;
  canonical: string;
  alternates: { locale: string; href: string }[];
  json_ld: Record<string, unknown>[];
  /** e.g. `noindex,follow` on filtered category URLs (spec §9.5). */
  robots?: string;
}

/** Cache hints the edge reads from every page model (spec §8.2, A2). */
export interface CacheHints {
  public: boolean;
  max_age: number;
  tags: string[];
}

export interface Link {
  label: string;
  href: string;
}

export interface MenuItem extends Link {
  children?: MenuItem[];
}

export type ConsentPurpose =
  | "analytics"
  | "ads"
  | "personalization"
  | "email_marketing"
  | "review_invites";

export interface ShopModel {
  name: string;
  locale: string;
  currency: string;
  locales: { locale: string; href: string }[];
  menus: { main: MenuItem[]; footer: MenuItem[] };
  legal_pages: Link[];
  consent: { purposes: ConsentPurpose[]; policy_url: string };
  free_shipping_threshold: Money | null;
  tracking: { rum_sample_rate: number };
  trust: { delivery: string; returns: string; payments: string[] };
  seo: Seo;
  cache: CacheHints;
}

export type StockState = "in_stock" | "low_stock" | "out_of_stock";

export interface Price {
  price: Money;
  /** Omnibus reference (lowest price in the 30 days before the reduction, A18); null = no claim. */
  reference_price: Money | null;
  discount_percent: number | null;
  unit_price: { price: Money; unit: string } | null;
}

export interface ProductCard extends Price {
  id: string;
  slug: string;
  name: string;
  brand: string | null;
  image: Image;
  stock: StockState;
  badges: string[];
}

export interface Variant extends Price {
  id: string;
  sku: string;
  options: Record<string, string>;
  stock: StockState;
  image_index: number | null;
}

export interface ProductPage {
  product: {
    id: string;
    slug: string;
    name: string;
    brand: string | null;
    /** Sanitized server-side; the only HTML a theme may render unescaped. */
    description_html: string;
    images: Image[];
    options: { name: string; values: string[] }[];
    variants: Variant[];
    parameters: { name: string; value: string }[];
    gpsr: { manufacturer: string; contact: string } | null;
  };
  breadcrumbs: Link[];
  delivery_estimate: { from: string; to: string };
  seo: Seo;
  cache: CacheHints;
}

export interface Facet {
  key: string;
  label: string;
  values: { value: string; label: string; selected: boolean; disabled: boolean; href: string }[];
}

export interface CategoryPage {
  category: { id: string; slug: string; name: string; description: string };
  breadcrumbs: Link[];
  facets: Facet[];
  sort: { value: string; label: string; selected: boolean; href: string }[];
  products: ProductCard[];
  pagination: { page: number; pages: number; prev: string | null; next: string | null };
  seo: Seo;
  cache: CacheHints;
}

export interface HomePage {
  hero: { title: string; subtitle: string; image: Image; cta: Link };
  categories: (Link & { image: Image })[];
  featured: ProductCard[];
  seo: Seo;
  cache: CacheHints;
}

/** `GET /recommendations?context=` (M2 fills it with real strategies; private if personalized). */
export interface Recommendations {
  products: ProductCard[];
  cache: CacheHints;
}

export interface SearchSuggest {
  query: string;
  products: { slug: string; name: string; image: Image; price: Money }[];
  categories: Link[];
}

export interface CartLine {
  id: string;
  variant_id: string;
  product_name: string;
  variant_label: string;
  image: Image;
  quantity: number;
  unit_price: Money;
  total: Money;
}

export interface Cart {
  id: string | null;
  lines: CartLine[];
  item_count: number;
  subtotal: Money | null;
  free_shipping_remaining: Money | null;
}
