import { hasConsent } from "@platform/storefront-sdk/client";
import { CONSENT_CHANGED } from "@platform/storefront-sdk/consent";
import { type Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { Props } from "./RecentlyViewedList";

/**
 * "Recently viewed" island: a small gate that loads the list (`RecentlyViewedList`) only once
 * the visitor has granted `personalization` (A20), so everyone else downloads almost nothing.
 * Renders nothing on the server: hydrate with client:idle, never client:visible.
 */
export default function RecentlyViewed(props: Props) {
  const [List, setList] = createSignal<Component<Props>>();
  onMount(() => {
    const load = () => {
      if (List() || !hasConsent("personalization")) return;
      void import("./RecentlyViewedList").then((m) => setList(() => m.default));
    };
    load();
    addEventListener(CONSENT_CHANGED, load);
    onCleanup(() => removeEventListener(CONSENT_CHANGED, load));
  });
  return (
    <Show when={List()}>
      {(L) => {
        const Loaded = L();
        return <Loaded {...props} />;
      }}
    </Show>
  );
}
