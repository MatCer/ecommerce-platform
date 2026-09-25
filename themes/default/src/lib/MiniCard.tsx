import { imageUrl, type Messages, t } from "@platform/storefront-sdk/format";
import type { ProductCard } from "@platform/storefront-sdk/types";

/**
 * Compact product card for islands (cart cross-sell, "for you", recently viewed): photo, name,
 * current price. The full card (`ProductCard.astro`) stays server-rendered; this one only lives
 * in lazily loaded chunks. No reduction display: a strikethrough needs the Omnibus reference
 * (A18), which the full card shows.
 */
export default function MiniCard(props: {
  product: ProductCard;
  base: string;
  labels: Messages;
  class?: string;
}) {
  const p = () => props.product;
  const src = () => {
    const img = p().images[0];
    return img && imageUrl(img, 360);
  };
  return (
    <a
      href={`${props.base}/p/${encodeURIComponent(p().slug)}`}
      class={`group block ${props.class ?? ""}`}
    >
      <img
        src={src()}
        alt=""
        width="176"
        height="220"
        loading="lazy"
        class="aspect-[4/5] w-full rounded-lg bg-muted object-cover"
      />
      <span class="mt-2 block text-sm leading-snug font-medium group-hover:underline">
        {p().name}
      </span>
      <span class="price mt-0.5 block text-sm">
        {p().price_varies
          ? t(props.labels, "product.price_from", { price: p().price.formatted })
          : p().price.formatted}
      </span>
    </a>
  );
}
