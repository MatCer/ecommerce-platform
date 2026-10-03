import { type Messages, t } from "@platform/storefront-sdk/format";
import type { Money } from "@platform/storefront-sdk/types";
import { type Component, createEffect, createSignal, on, onMount, Show } from "solid-js";
import { added, cart, loadCart, open, openCart } from "../lib/cart-store";
import Icon from "../lib/Icon";
import { bag } from "../lib/icons";

// The drawer (and its code) loads on first open; hovering or focusing the button warms it.
// A plain dynamic import: Solid's lazy() would pull ~1 kB of Suspense runtime into every page.
const load = () => import("./CartDrawer");

/**
 * Header cart button with the item count. The drawer is `CartDrawer`, fetched on demand.
 */
export default function MiniCart(props: {
  labels: Messages;
  locale: string;
  base: string;
  threshold?: Money;
}) {
  const l = (key: string, args?: Record<string, string | number>) => t(props.labels, key, args);
  const [bump, setBump] = createSignal(false);
  const [Drawer, setDrawer] = createSignal<Component<typeof props>>();

  onMount(() => void loadCart());
  createEffect(() => {
    if (open() && !Drawer()) void load().then((m) => setDrawer(() => m.default));
  });
  createEffect(
    on(
      added,
      () => {
        setBump(false);
        requestAnimationFrame(() => setBump(true));
      },
      { defer: true },
    ),
  );
  const count = () => cart()?.item_count ?? 0;
  const total = () => (count() > 0 ? cart()?.total?.formatted : undefined);

  return (
    <>
      <button
        type="button"
        class="relative grid size-11 place-items-center rounded-md hover:bg-panel-raised lg:flex lg:h-14 lg:w-auto lg:gap-3 lg:bg-panel-raised lg:px-4 lg:hover:bg-panel-raised/80"
        onClick={openCart}
        onPointerEnter={() => void load()}
        onFocus={() => void load()}
        aria-haspopup="dialog"
      >
        <span class="relative">
          <Icon d={bag} class="size-6 lg:size-7" />
          <span
            aria-hidden="true"
            class="absolute -top-2 -right-2.5 grid h-5 min-w-5 place-items-center rounded-full bg-buy px-1 text-[0.6875rem] font-bold text-foreground tabular-nums"
            classList={{ "animate-bump": bump(), invisible: count() === 0 }}
          >
            {count()}
          </span>
        </span>
        <span class="hidden text-left leading-tight lg:grid" aria-hidden="true">
          <span class="text-xs text-panel-foreground">{l("cart.title")}</span>
          <Show when={total()}>{(m) => <span class="price text-base text-card">{m()}</span>}</Show>
        </span>
        <span class="sr-only">{l("cart.open", { count: count() })}</span>
      </button>

      <Show when={Drawer()}>
        {(D) => {
          const Loaded = D();
          return <Loaded {...props} />;
        }}
      </Show>
    </>
  );
}
