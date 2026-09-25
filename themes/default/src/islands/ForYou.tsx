import { hasConsent } from "@platform/storefront-sdk/client";
import { CONSENT_CHANGED } from "@platform/storefront-sdk/consent";
import { type Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { Props } from "./ForYouList";

/**
 * Home "for you" gate: loads the list (`ForYouList`) only once the visitor has granted
 * `personalization` (A20), so everyone else downloads almost nothing and the cached home page
 * stays the same for all. Renders nothing on the server: hydrate with client:idle.
 */
export default function ForYou(props: Props) {
  const [List, setList] = createSignal<Component<Props>>();
  onMount(() => {
    const load = () => {
      if (List() || !hasConsent("personalization")) return;
      void import("./ForYouList").then((m) => setList(() => m.default));
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
