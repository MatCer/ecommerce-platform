import type { Image } from "@platform/storefront-sdk/types";

/**
 * `sizes` of every image slot, in one place. They must describe the rendered width exactly:
 * a loose guess (plain `50vw`) makes the browser pick the next, 2-4× heavier variant, and the
 * LCP preload must use the same value as its `<img>` (`lcpImage()` pairs them). Change a grid
 * or a container width → change the matching line here.
 */

/** Product grids: 2 columns, 3 from 48rem, 4 from 64rem, inside the 80rem container. */
export const CARD_SIZES =
  "(min-width: 80rem) 18.5rem, (min-width: 64rem) calc(25vw - 2rem), (min-width: 48rem) calc(33vw - 2rem), calc(50vw - 1.5rem)";

/** Home "shop by category" tiles: 3 columns on phones, 6 from 48rem. */
export const TILE_SIZES =
  "(min-width: 80rem) 11.5rem, (min-width: 48rem) calc(16vw - 1.75rem), calc(33vw - 2rem)";

/** Home hero photo: the 5/12 right column from 48rem, full width on phones. */
export const HERO_SIZES = "(min-width: 80rem) 31rem, (min-width: 48rem) 40vw, calc(100vw - 2rem)";

/** Home lead-category card photo: 2/5 of the card (half the container from 48rem). */
export const PROMO_SIZES =
  "(min-width: 80rem) 13rem, (min-width: 48rem) calc(20vw - 3rem), calc(40vw - 2.5rem)";

/** Home best-seller carousel: 2 cards per view on phones, 4 from 48rem, inside white frames. */
export const CAROUSEL_SIZES =
  "(min-width: 80rem) 16.5rem, (min-width: 48rem) calc(25vw - 3.5rem), calc(50vw - 2.5rem)";

/** Home category spotlight: the lead product spans 2 of 4 columns (all 2 on phones). */
export const SPOTLIGHT_SIZES =
  "(min-width: 80rem) 37.5rem, (min-width: 48rem) calc(50vw - 2.5rem), calc(100vw - 2rem)";

/** Home category spotlight, the other cards: 4 columns from 48rem, 2 on phones. */
export const SPOTLIGHT_CARD_SIZES =
  "(min-width: 80rem) 17.5rem, (min-width: 48rem) calc(25vw - 2rem), calc(50vw - 1.5rem)";

/** Home blog teasers: 3 columns from 48rem. */
export const BLOG_SIZES = "(min-width: 80rem) 24.5rem, (min-width: 48rem) calc(33vw - 2rem), calc(100vw - 2rem)";

/** Product gallery: full width on phones, 7/12 of the container from 48rem. */
export const GALLERY_SIZES =
  "(min-width: 80rem) 43rem, (min-width: 48rem) 55vw, calc(100vw - 2rem)";

/** Gallery thumbnails and cart/search thumbnails. */
export const THUMB_SIZES = "4rem";

/**
 * Drops srcset candidates wider than `max` px: a card never renders wider than ~300 CSS px, so
 * 960 w files only bloat the HTML (every card repeats its srcsets).
 */
export function capped(img: Image, max: number): Image {
  const keep = (srcset: string) =>
    srcset
      .split(",")
      .map((c) => c.trim())
      .filter((c) => {
        const w = Number(/(\d+)w$/.exec(c)?.[1] ?? 0);
        return w > 0 && w <= max;
      })
      .join(", ");
  return {
    ...img,
    srcset: keep(img.srcset),
    srcset_webp: keep(img.srcset_webp),
    srcset_fallback: keep(img.srcset_fallback),
  };
}

/** A product-card photo (a grid's LCP preload must use this same image). */
export const cardImage = (img: Image) => capped(img, 720);

/** Category and search grids beside the filter sidebar: 2 columns, 3 from 64rem. */
export const LISTING_SIZES =
  "(min-width: 80rem) 18rem, (min-width: 64rem) calc((100vw - 23.5rem) / 3), (min-width: 48rem) calc((100vw - 21rem) / 2), calc(50vw - 1.5rem)";
