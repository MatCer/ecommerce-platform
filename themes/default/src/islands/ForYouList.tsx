import { recommendations } from "@platform/storefront-sdk/client";
import type { Messages } from "@platform/storefront-sdk/format";
import type { ProductCard } from "@platform/storefront-sdk/types";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import MiniCard from "../lib/MiniCard";

export type Props = { title: string; base: string; locale: string; labels: Messages };

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
      <section aria-labelledby="for-you" class="container-shop mt-14 md:mt-20">
        <h2 id="for-you" class="mb-5 text-2xl font-extrabold md:text-3xl">
          {props.title}
        </h2>
        <ul class="flex snap-x gap-3 overflow-x-auto pb-2 md:gap-4">
          <For each={items()}>
            {(p) => (
              <li class="w-40 shrink-0 snap-start md:w-52">
                <MiniCard product={p} base={props.base} labels={props.labels} />
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
