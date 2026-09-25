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
/** hreflang alternate (`cs-CZ` → URL of the same page there). */
export type Alternate = S["Alternate"];
export type MarketLink = S["MarketLink"];
export type DeliveryEstimate = S["DeliveryEstimate"];
/** General Product Safety Regulation information (EU 2023/988, art. 19). */
export type Gpsr = S["GpsrView"];
export type Parameter = S["ParameterView"];
export type ProductDetail = S["ProductDetail"];
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
/** Typeahead (WP7): matching categories first, then products (`SearchHit`). */
export type SearchSuggest = S["Suggestions"];
/** Full search results with variant-correct facets (WP7, A23). */
export type SearchResult = S["SearchResult"];
export type SearchHit = S["SearchHit"];
export type CartLine = S["CartLineView"];
export type Cart = S["CartView"];

/** CMS and legal pages (`/pages/<slug>`) and the blog (WP13): content as typed blocks. */
export type CmsPage = S["CmsPage"];
export type BlogIndex = S["BlogIndex"];
export type BlogPost = S["BlogPost"];
export type BlogPostSummary = S["BlogPostSummary"];
/**
 * One content block. Render `rich_text.html` and `faq.items[].answer_html` unescaped (sanitized
 * by the platform); everything else is plain text.
 */
export type ContentBlock = S["BlockView"];

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

/** Checkout origin only (WP9): the signed-in customer, addresses, sign-in result. */
export type Customer = S["CustomerView"];
export type Address = S["Address"];
export type AddressInput = S["AddressInput"];
export type SignInResult = S["SignInResult"];

/**
 * Consent (A20). Themes post `ConsentChoice` to `/_p/consent` (see
 * `docs/decisions/consent-contract.md`); `null` purposes were never asked.
 */
export type ConsentChoice = S["ConsentChoice"];
export type ConsentState = S["ConsentState"];
export type ConsentPurposes = S["Purposes"];
