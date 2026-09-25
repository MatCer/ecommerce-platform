import { hasConsent } from "@platform/storefront-sdk/client";
import { CONSENT_CHANGED } from "@platform/storefront-sdk/consent";
import { type Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { Props } from "./RecentlyViewedList";

/**
 * "Recently viewed" island: a small gate that loads the list (`RecentlyViewedList`) only while
 * the visitor grants `personalization` (A20), so everyone else downloads almost nothing; a
 * withdrawal unmounts it (the SDK clears the stored ids). Renders nothing on the server:
 * hydrate with client:idle, never client:visible.
 */
export default function RecentlyViewed(props: Props) {
  const [List, setList] = createSignal<Component<Props>>();
  const [granted, setGranted] = createSignal(false);
  onMount(() => {
    const sync = () => {
      setGranted(hasConsent("personalization"));
      if (granted() && !List())
        void import("./RecentlyViewedList").then((m) => setList(() => m.default));
    };
    sync();
    // A choice made in another tab reaches this one when it is shown again.
    const shown = () => document.visibilityState === "visible" && sync();
    addEventListener(CONSENT_CHANGED, sync);
    addEventListener("pageshow", sync);
    document.addEventListener("visibilitychange", shown);
    onCleanup(() => {
      removeEventListener(CONSENT_CHANGED, sync);
      removeEventListener("pageshow", sync);
      document.removeEventListener("visibilitychange", shown);
    });
  });
  return (
    <Show when={granted() && List()}>
      {(L) => {
        const Loaded = L();
        return <Loaded {...props} />;
      }}
    </Show>
  );
}
