import type { Messages } from "@platform/storefront-sdk/format";
import { FOR_YOU_SIZES } from "../lib/images";
import { recommendations } from "@platform/storefront-sdk/recommendations";
import type { ProductCard } from "@platform/storefront-sdk/types";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import MiniCard from "../lib/MiniCard";

export type Props = {
  title: string;
  subtitle: string;
  base: string;
  locale: string;
  labels: Messages;
};

/**
 * Personal picks on the home page (spec §11.2 "for you"): the private
 * `/_p/recommendations?context=home`, which personalizes only while the server-side consent
 * records grant `personalization` (A20). Shown only when the answer really is personalized;
 * otherwise the server-rendered bestsellers already cover it. The `ForYou` gate unmounts this
 * on withdrawal, which also aborts a pending request.
 */
export default function ForYouList(props: Props) {
  const [items, setItems] = createSignal<ProductCard[]>([]);
  onMount(() => {
    const abort = new AbortController();
    onCleanup(() => abort.abort());
    recommendations({ context: "home", limit: 8 }, { locale: props.locale, signal: abort.signal })
      .then((r) => setItems(r.strategy === "personalized" ? r.products : []))
      .catch(() => setItems([]));
  });
  return (
    <Show when={items().length > 0}>
      <section aria-labelledby="for-you" class="container-shop mt-12 md:mt-20">
        <div class="mb-5 md:mb-7">
          <h2 id="for-you" class="text-2xl md:text-[2rem]">
            {props.title}
          </h2>
          <p class="mt-1 text-[0.9375rem] text-muted-foreground">{props.subtitle}</p>
        </div>
        <ul class="grid grid-cols-2 gap-x-3 gap-y-8 md:grid-cols-4 md:gap-x-6 md:gap-y-10">
          <For each={items()}>
            {(p) => (
              <li>
                <MiniCard product={p} base={props.base} labels={props.labels} sizes={FOR_YOU_SIZES} />
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
