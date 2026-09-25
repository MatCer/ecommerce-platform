import { t } from "@platform/storefront-sdk/format";
import { createSignal, onMount, Show } from "solid-js";
import { forgetPlacement, loadPlacement, nextUrl, sendPlacement } from "../lib/placement";

/**
 * On the checkout page without a cart: an order whose placement response was lost (A12) may
 * exist, its cart already converted. Replays the kept placement (same key, same body), which
 * answers the same order, and continues to payment. Renders nothing otherwise.
 */
export default function ResumePlacement(props: { m: Record<string, string> }) {
  const [busy, setBusy] = createSignal(false);
  onMount(async () => {
    const p = loadPlacement();
    if (!p) return;
    setBusy(true);
    const r = await sendPlacement(p);
    if (r.ok && r.data) return location.assign(nextUrl(r.data));
    // Nothing to resume (no order came of it, or the cart is gone): drop it.
    forgetPlacement();
    setBusy(false);
  });
  return (
    <Show when={busy()}>
      <p role="status" class="mb-3 rounded-md bg-identity-wash p-3 text-sm">
        {t(props.m, "checkout.resuming")}
      </p>
    </Show>
  );
}
