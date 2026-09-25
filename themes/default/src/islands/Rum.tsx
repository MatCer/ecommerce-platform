import { startRum, track } from "@platform/storefront-sdk/client";
import { onMount } from "solid-js";

/**
 * Analytics (A20, consented visitors only): the page view (+ the product on a product page) and
 * Web Vitals RUM (§9.6, sampled; the reporter loads lazily). Renders nothing.
 */
export default function Rum(props: { sampleRate: number; template: string; productId?: string }) {
  onMount(() => {
    track({ type: "page_view", template: props.template });
    if (props.productId)
      track({ type: "view_item", template: props.template, product_id: props.productId });
    void startRum(props.sampleRate, props.template);
  });
  return null;
}
