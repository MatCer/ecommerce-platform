import { startRum, track } from "@platform/storefront-sdk/client";
import { onMount } from "solid-js";

/**
 * Analytics and ad-platform events (A20, WP20; consented visitors only, per purpose): the page
 * view (+ the product on a product page), the search and
 * Web Vitals RUM (§9.6, sampled; the reporter loads lazily). Renders nothing.
 */
export default function Rum(props: {
  sampleRate: number;
  template: string;
  productId?: string;
  query?: string;
}) {
  onMount(() => {
    track({ type: "page_view", template: props.template });
    if (props.productId)
      track({ type: "view_item", template: props.template, product_id: props.productId });
    if (props.query) track({ type: "search", template: props.template, query: props.query });
    void startRum(props.sampleRate, props.template);
  });
  return null;
}
