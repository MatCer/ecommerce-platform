import { imageAttrs, type Messages, t } from "@platform/storefront-sdk/format";
import type { Image } from "@platform/storefront-sdk/types";
import { createEffect, createSignal, For, on, onMount, Show } from "solid-js";
import Icon from "../lib/Icon";
import { chevronLeft, chevronRight } from "../lib/icons";
import { GALLERY_SIZES, THUMB_SIZES } from "../lib/images";
import { imageIndex } from "../lib/product-store";

/**
 * Product gallery: a native scroll-snap strip (swipe works before hydration) with thumbnails,
 * arrows and arrow keys once hydrated. The first image is the LCP element and the only one in
 * the server HTML: lazy images in a horizontal strip sit inside Chrome's lazy-load margin and
 * would compete with it for bandwidth (measured in WP2). Choosing a colour in the buy box
 * scrolls to that variant's photo.
 */
export default function Gallery(props: { images: Image[]; labels: Messages }) {
  const l = (key: string, args?: Record<string, string | number>) => t(props.labels, key, args);
  const [current, setCurrent] = createSignal(0);
  const [hydrated, setHydrated] = createSignal(false);
  const total = () => props.images.length;
  let strip: HTMLDivElement | undefined;

  // The other photos wait for the page's load event (the LCP photo has arrived by then), or for
  // the first touch/key on the gallery, whichever comes first.
  onMount(() => {
    const reveal = () =>
      "requestIdleCallback" in window
        ? requestIdleCallback(() => setHydrated(true))
        : setTimeout(() => setHydrated(true), 200);
    if (document.readyState === "complete") reveal();
    else addEventListener("load", reveal, { once: true });
  });
  const shown = () => (hydrated() ? props.images : props.images.slice(0, 1));

  const go = (i: number): void => {
    if (!hydrated()) {
      setHydrated(true);
      return void requestAnimationFrame(() => go(i));
    }
    const target = Math.max(0, Math.min(i, total() - 1));
    const el = strip?.children[target] as HTMLElement | undefined;
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    strip?.scrollTo({ left: el?.offsetLeft ?? 0, behavior: reduce ? "auto" : "smooth" });
    setCurrent(target);
  };
  createEffect(
    on(imageIndex, (i) => i !== null && go(i), { defer: true }),
  );
  const onScroll = () => {
    if (strip) setCurrent(Math.round(strip.scrollLeft / strip.clientWidth));
  };

  return (
    <div class="flex flex-col gap-3">
      <div class="relative">
        <div
          ref={strip}
          onScroll={onScroll}
          onPointerDown={() => setHydrated(true)}
          onFocus={() => setHydrated(true)}
          class="flex snap-x snap-mandatory overflow-x-auto overscroll-x-contain rounded-xl bg-muted [scrollbar-width:none]"
          role="group"
          aria-roledescription="carousel"
          aria-label={l("product.gallery")}
          tabindex="0"
          onKeyDown={(e) => {
            if (e.key === "ArrowRight") go(current() + 1);
            else if (e.key === "ArrowLeft") go(current() - 1);
            else return;
            e.preventDefault();
          }}
        >
          <For each={shown()}>
            {(img, i) => (
              <img
                {...imageAttrs(img, { sizes: GALLERY_SIZES, priority: i() === 0, width: 720 })}
                alt={img.alt}
                class="aspect-[4/5] w-full shrink-0 snap-start object-cover"
              />
            )}
          </For>
        </div>
        <Show when={total() > 1}>
          <button
            type="button"
            onClick={() => go(current() - 1)}
            disabled={current() === 0}
            class="absolute top-1/2 left-3 hidden size-11 -translate-y-1/2 place-items-center rounded-full bg-card/90 shadow-md hover:bg-card disabled:opacity-0 md:grid"
          >
            <Icon d={chevronLeft} />
            <span class="sr-only">{l("product.prev_image")}</span>
          </button>
          <button
            type="button"
            onClick={() => go(current() + 1)}
            disabled={current() === total() - 1}
            class="absolute top-1/2 right-3 hidden size-11 -translate-y-1/2 place-items-center rounded-full bg-card/90 shadow-md hover:bg-card disabled:opacity-0 md:grid"
          >
            <Icon d={chevronRight} />
            <span class="sr-only">{l("product.next_image")}</span>
          </button>
          <p
            aria-hidden="true"
            class="absolute right-3 bottom-3 rounded-full bg-card/90 px-2.5 py-1 text-xs font-semibold tabular-nums md:hidden"
          >
            {current() + 1} / {total()}
          </p>
        </Show>
      </div>
      <Show when={total() > 1}>
        <ul class="flex gap-2 overflow-x-auto">
          <For each={props.images}>
            {(img, i) => (
              <li class="shrink-0">
                <button
                  type="button"
                  onClick={() => go(i())}
                  aria-current={current() === i() ? "true" : undefined}
                  class="block w-16 overflow-hidden rounded-md border-2 border-transparent opacity-70 hover:opacity-100 aria-[current=true]:border-identity aria-[current=true]:opacity-100"
                >
                  <Show
                    when={hydrated()}
                    fallback={<span class="block aspect-[4/5] w-full bg-muted" />}
                  >
                    <img
                      {...imageAttrs(img, { sizes: THUMB_SIZES, width: 160 })}
                      alt=""
                      class="aspect-[4/5] w-full object-cover"
                    />
                  </Show>
                  <span class="sr-only">{l("product.image", { n: i() + 1, total: total() })}</span>
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}
