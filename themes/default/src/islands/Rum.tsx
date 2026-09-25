import { startRum } from "@platform/storefront-sdk/client";
import { onMount } from "solid-js";

/** Web Vitals RUM (§9.6): consented + sampled only; web-vitals loads lazily. Renders nothing. */
export default function Rum(props: { sampleRate: number; template: string }) {
  onMount(() => void startRum(props.sampleRate, props.template));
  return null;
}
