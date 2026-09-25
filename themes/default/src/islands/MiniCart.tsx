import { type Messages, t } from "@platform/storefront-sdk/format";
import type { Money } from "@platform/storefront-sdk/types";
import { type Component, createEffect, createSignal, on, onMount, Show } from "solid-js";
import { added, cart, loadCart, open, setOpen } from "../lib/cart-store";
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

  return (
    <>
      <button
        type="button"
        class="relative grid size-11 place-items-center rounded-md hover:bg-muted lg:flex lg:w-auto lg:gap-2 lg:px-3 lg:text-sm lg:font-semibold"
        onClick={() => setOpen(true)}
        onPointerEnter={() => void load()}
        onFocus={() => void load()}
        aria-haspopup="dialog"
      >
        <Icon d={bag} class="size-6 lg:size-5" />
        <span class="hidden lg:inline" aria-hidden="true">
          {l("cart.title")}
        </span>
        <span class="sr-only">{l("cart.open", { count: count() })}</span>
        <span
          aria-hidden="true"
          class="absolute top-1 right-0.5 grid h-5 min-w-5 place-items-center rounded-full bg-identity-ink px-1 text-[0.6875rem] font-bold text-card tabular-nums lg:static"
          classList={{ "animate-bump": bump(), invisible: count() === 0 }}
        >
          {count()}
        </span>
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
