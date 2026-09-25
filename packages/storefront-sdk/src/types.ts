/**
 * Storefront page models (spec §8.2), generated from the API's storefront OpenAPI document
 * (`openapi.storefront.json` → `schema.d.ts`, `make openapi`). This file only names them.
 */
import type { components } from "./schema";

type S = components["schemas"];

/** Spec §8.1: `{ amount_minor: 12900, currency: "CZK", formatted: "129,00 Kč" }`. */
export type Money = S["MoneyView"];
/** Responsive image: AVIF `srcset`, WebP/fallback candidates, intrinsic size (no CLS). */
export type Image = S["Image"];
export type Seo = S["Seo"];
/** Cache hints the edge reads from every page model (spec §8.2, A2). */
export type CacheHints = S["CacheHints"];
export type Link = S["Link"];
export type MenuItem = S["MenuItem"];
export type ConsentPurpose = S["ConsentPurpose"];
export type ShopModel = S["ShopModel"];
export type StockState = S["StockState"];
/** Price with its Omnibus reference (A18): `reference_price` only when a reduction is claimed. */
export type Price = S["PriceView"];
export type ProductCard = S["ProductCard"];
export type Variant = S["VariantView"];
export type ProductOption = S["OptionView"];
export type ProductPage = S["ProductPage"];
export type Facet = S["FacetView"];
/** A category page or search results. */
export type ListingPage = S["ListingPage"];
export type HomePage = S["HomePage"];
export type Recommendations = S["Recommendations"];
export type SearchSuggest = S["SearchSuggest"];
export type CartLine = S["CartLineView"];
export type Cart = S["CartView"];

/** `GET /_p/cart` before anything was added: the edge answers without creating a cart. */
export interface EmptyCart {
  id: null;
  lines: never[];
  item_count: 0;
  coupon: null;
  subtotal: null;
  discount: null;
  total: null;
  vat: never[];
  vat_total: null;
  free_shipping_remaining: null;
}

export type CartState = Cart | EmptyCart;
