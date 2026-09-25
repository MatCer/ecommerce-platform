import { consentStorage } from "@platform/storefront-sdk/client";
import { CONSENT_CHANGED } from "@platform/storefront-sdk/consent";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";

type Item = { slug: string; name: string; image?: string };

/**
 * "Recently viewed" (A20: needs the `personalization` purpose). Without consent it stores and
 * shows nothing; withdrawing consent clears the list (SDK). Keeps name + photo only: a stored
 * price would go stale and read as a price claim. Renders nothing on the server, so it is
 * hydrated with client:idle (client:visible needs a box to observe).
 */
export default function RecentlyViewed(props: { current: Item; title: string; base: string }) {
  const [items, setItems] = createSignal<Item[]>([]);
  const store = consentStorage("personalization");
  const KEY = "recent";

  const sync = () => {
    const seen = (store.get<Item[]>(KEY) ?? []).filter(
      (x) => typeof x?.slug === "string" && x.slug !== props.current.slug,
    );
    setItems(seen.slice(0, 8));
    store.set(KEY, [props.current, ...seen].slice(0, 12));
  };
  onMount(() => {
    sync();
    addEventListener(CONSENT_CHANGED, sync);
    onCleanup(() => removeEventListener(CONSENT_CHANGED, sync));
  });

  return (
    <Show when={items().length > 0}>
      <section aria-labelledby="recently-viewed" class="mt-14">
        <h2 id="recently-viewed" class="mb-4 text-xl font-extrabold md:text-2xl">
          {props.title}
        </h2>
        <ul class="flex snap-x gap-3 overflow-x-auto pb-2 md:gap-4">
          <For each={items()}>
            {(item) => (
              <li class="w-36 shrink-0 snap-start md:w-44">
                <a href={`${props.base}/p/${encodeURIComponent(item.slug)}`} class="group block">
                  <img
                    src={item.image}
                    alt=""
                    width="176"
                    height="220"
                    loading="lazy"
                    class="aspect-[4/5] w-full rounded-lg bg-muted object-cover"
                  />
                  <span class="mt-2 block text-sm font-medium group-hover:underline">
                    {item.name}
                  </span>
                </a>
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}
