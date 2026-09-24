import { imageAttrs } from "@platform/storefront-sdk/format";
import type { Image } from "@platform/storefront-sdk/types";
import { createSignal, For, onMount, Show } from "solid-js";

/**
 * Product gallery: a native scroll-snap strip (swipe works before hydration); the island adds
 * thumbnail navigation and keeps `aria-current` in sync. The first image is the LCP element.
 */
export default function Gallery(props: { images: Image[] }) {
  const [current, setCurrent] = createSignal(0);
  // Only the LCP image is in the server HTML: lazy images in a horizontal strip sit inside
  // Chrome's lazy-load margin and would compete with it for bandwidth (measured in WP2).
  const [hydrated, setHydrated] = createSignal(false);
  onMount(() =>
    "requestIdleCallback" in window
      ? requestIdleCallback(() => setHydrated(true))
      : setTimeout(() => setHydrated(true), 200),
  );
  const shown = () => (hydrated() ? props.images : props.images.slice(0, 1));
  let strip: HTMLDivElement | undefined;

  const go = (i: number) => {
    const el = strip?.children[i] as HTMLElement | undefined;
    el?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "start" });
    setCurrent(i);
  };
  const onScroll = () => {
    if (strip) setCurrent(Math.round(strip.scrollLeft / strip.clientWidth));
  };

  return (
    <div class="flex flex-col gap-3">
      <section
        ref={strip}
        onScroll={onScroll}
        class="flex snap-x snap-mandatory overflow-x-auto rounded-xl bg-muted [scrollbar-width:none]"
        aria-label="Fotografie produktu"
        aria-roledescription="galerie"
        tabindex="0"
        onKeyDown={(e) => {
          if (e.key === "ArrowRight") go(Math.min(current() + 1, props.images.length - 1));
          if (e.key === "ArrowLeft") go(Math.max(current() - 1, 0));
        }}
      >
        <For each={shown()}>
          {(img, i) => (
            <img
              {...imageAttrs(img, {
                sizes: "(min-width: 48rem) 50vw, calc(100vw - 2rem)",
                priority: i() === 0,
                width: 720,
              })}
              alt={img.alt}
              class="aspect-[4/5] w-full shrink-0 snap-start object-cover"
            />
          )}
        </For>
      </section>
      <div class="flex gap-2 overflow-x-auto">
        <For each={props.images}>
          {(img, i) => (
            <button
              type="button"
              onClick={() => go(i())}
              aria-current={current() === i()}
              aria-label={`Fotografie ${i() + 1} z ${props.images.length}`}
              class="w-16 shrink-0 overflow-hidden rounded-md border-2 border-transparent aria-[current=true]:border-identity"
            >
              <Show
                when={hydrated()}
                fallback={<span class="block aspect-[4/5] w-full bg-muted" />}
              >
                <img
                  {...imageAttrs(img, { sizes: "64px", width: 360 })}
                  alt=""
                  class="aspect-[4/5] w-full object-cover"
                />
              </Show>
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
