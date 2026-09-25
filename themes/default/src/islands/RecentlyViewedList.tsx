import { consentStorage, recentlyViewed } from "@platform/storefront-sdk/client";
import type { Messages } from "@platform/storefront-sdk/format";
import type { ProductCard } from "@platform/storefront-sdk/types";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import MiniCard from "../lib/MiniCard";

export type Props = { current: string; title: string; base: string; labels: Messages };

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/**
 * "Recently viewed" (A20: needs the `personalization` purpose). The device keeps product ids
 * only, and only with consent (SDK storage; withdrawing clears it). They are rehydrated with
 * live prices and availability through the public route, without cookies or any identity, so
 * the server never links the history to a visitor (§11.2). Loaded by the `RecentlyViewed`
 * gate, which unmounts it on withdrawal (aborting a pending request).
 */
export default function RecentlyViewedList(props: Props) {
  const [items, setItems] = createSignal<ProductCard[]>([]);
  const store = consentStorage("personalization");
  const KEY = "recent";

  onMount(() => {
    const abort = new AbortController();
    onCleanup(() => abort.abort());
    // Earlier theme versions stored {slug, name, image}; only ids are kept now.
    const seen = (store.get<unknown[]>(KEY) ?? []).filter(
      (x): x is string => typeof x === "string" && UUID.test(x) && x !== props.current,
    );
    store.set(KEY, [props.current, ...seen].slice(0, 12));
    if (seen.length === 0) return;
    recentlyViewed(seen.slice(0, 8), { base: props.base, signal: abort.signal })
      .then((r) => setItems(r.products))
      .catch(() => setItems([]));
  });

  return (
    <Show when={items().length > 0}>
      <section aria-labelledby="recently-viewed" class="mt-14">
        <h2 id="recently-viewed" class="mb-4 text-xl font-extrabold md:text-2xl">
          {props.title}
        </h2>
        <ul class="flex snap-x gap-3 overflow-x-auto pb-2 md:gap-4">
          <For each={items()}>
            {(p) => (
              <li class="w-36 shrink-0 snap-start md:w-44">
                <MiniCard product={p} base={props.base} labels={props.labels} />
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
