import { hasConsent } from "@platform/storefront-sdk/client";
import { CONSENT_CHANGED } from "@platform/storefront-sdk/consent";
import { type Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { Props } from "./ForYouList";

/**
 * Home "for you" gate: loads the list (`ForYouList`) only while the visitor grants
 * `personalization` (A20), so everyone else downloads almost nothing and the cached home page
 * stays the same for all; a withdrawal unmounts it. Renders nothing on the server: hydrate
 * with client:idle.
 */
export default function ForYou(props: Props) {
  const [List, setList] = createSignal<Component<Props>>();
  const [granted, setGranted] = createSignal(false);
  onMount(() => {
    const sync = () => {
      setGranted(hasConsent("personalization"));
      if (granted() && !List()) void import("./ForYouList").then((m) => setList(() => m.default));
    };
    sync();
    addEventListener(CONSENT_CHANGED, sync);
    onCleanup(() => removeEventListener(CONSENT_CHANGED, sync));
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
