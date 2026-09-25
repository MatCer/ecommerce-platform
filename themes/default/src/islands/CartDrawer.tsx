import { imageUrl, type Messages, t, tn } from "@platform/storefront-sdk/format";
import { recommendations } from "@platform/storefront-sdk/recommendations";
import type { CartLine, Money, ProductCard } from "@platform/storefront-sdk/types";
import { createEffect, createSignal, For, on, Show } from "solid-js";
import { cart, open, setOpen, updateLine } from "../lib/cart-store";
import Icon from "../lib/Icon";
import MiniCard from "../lib/MiniCard";

// Local copies of lib/icons.ts paths: importing them would pull these drawer-only icons into
// the icon chunk every page loads, while this module is fetched only when the cart opens.
const close = "M6 6l12 12M18 6 6 18";
const minus = "M5 12h14";
const plus = "M12 5v14M5 12h14";
const trash = "M5 7h14M10 7V5h4v2M7 7l1 12h8l1-12";
const truck =
  "M2.5 6h11v9.5h-11zM13.5 9h4l3 3.2v3.3h-7M6.5 19a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 0 0 0 3.6Zm11 0a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 0 0 0 3.6Z";

/**
 * The cart drawer (native modal <dialog>: focus trap, Escape, focus return to the opener).
 * Not an island itself: MiniCart imports it on first open, so visitors who never open the cart
 * never download it. Free-delivery progress only when the shop has a threshold; checkout is a
 * POST to the edge-owned handoff, which moves the cart to the checkout origin (A1).
 */
export default function CartDrawer(props: {
  labels: Messages;
  locale: string;
  base: string;
  threshold?: Money;
}) {
  const l = (key: string, args?: Record<string, string | number>) => t(props.labels, key, args);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  let dialog: HTMLDialogElement | undefined;

  createEffect(() => {
    if (!dialog) return;
    if (open() && !dialog.open) dialog.showModal();
    if (!open() && dialog.open) dialog.close();
  });

  // Cross-sell (WP17): products bought together with the cart's, from the private
  // `/_p/recommendations?context=cart` (the edge adds the cart capability). Refetched when the
  // cart's content changes while the drawer is open; a late answer for an older cart is dropped.
  const [crossSell, setCrossSell] = createSignal<ProductCard[]>([]);
  let asked = 0;
  createEffect(
    on(
      () => {
        const c = cart();
        return open() && c && "version" in c && c.lines.length > 0 ? `${c.id}:${c.version}` : null;
      },
      (key) => {
        if (!key) return;
        const mine = ++asked;
        recommendations({ context: "cart", limit: 4 }, { locale: props.locale })
          .then((r) => mine === asked && setCrossSell(r.products))
          .catch(() => mine === asked && setCrossSell([]));
      },
    ),
  );

  const count = () => cart()?.item_count ?? 0;
  const lines = () => cart()?.lines ?? [];
  const remaining = () => {
    const c = cart();
    return c && "version" in c ? c.free_shipping_remaining : null;
  };
  const progress = () => {
    const r = remaining();
    const total = props.threshold?.amount_minor;
    return r && total ? Math.min(100, Math.round(((total - r.amount_minor) / total) * 100)) : 100;
  };

  async function change(line: CartLine, quantity: number) {
    setBusy(true);
    setFailed(false);
    try {
      await updateLine(line.id, quantity);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    // biome-ignore lint/a11y/useKeyWithClickEvents: a backdrop click closes; Escape is native to <dialog>
    <dialog
      ref={dialog}
      onClose={() => dialog?.open || setOpen(false)}
      onClick={(e) => e.target === dialog && setOpen(false)}
      aria-labelledby="cart-title"
      class="m-0 ml-auto h-dvh max-h-none w-full max-w-md bg-card p-0 text-foreground shadow-sheet backdrop:bg-foreground/45"
    >
      <div class="flex h-full flex-col">
        <header class="flex h-16 items-center justify-between border-b border-border px-5">
          <h2 id="cart-title" class="text-lg font-bold">
            {l("cart.title")}
            <Show when={count() > 0}>
              <span class="ml-2 text-sm font-normal text-muted-foreground">
                {tn(props.labels, props.locale, "cart.items", count())}
              </span>
            </Show>
          </h2>
          <button
            type="button"
            class="-mr-2 grid size-11 place-items-center rounded-md hover:bg-muted"
            onClick={() => setOpen(false)}
          >
            <Icon d={close} class="size-6" />
            <span class="sr-only">{l("cart.close")}</span>
          </button>
        </header>

        <Show when={props.threshold && remaining() != null && lines().length > 0}>
          <div class="border-b border-border bg-identity-wash px-5 py-3 text-sm">
            <p class="mb-2 flex items-center gap-2 font-medium text-identity-ink">
              <Icon d={truck} class="size-4" />
              {remaining()?.amount_minor
                ? l("cart.free_shipping_remaining", { amount: remaining()?.formatted ?? "" })
                : l("cart.free_shipping_reached")}
            </p>
            <div class="h-1.5 overflow-hidden rounded-full bg-card" aria-hidden="true">
              <div
                class="h-full rounded-full bg-identity transition-[width] duration-300"
                style={{ width: `${progress()}%` }}
              />
            </div>
          </div>
        </Show>

        <Show
          when={lines().length > 0}
          fallback={
            <div class="grid flex-1 place-content-center gap-4 p-8 text-center">
              <p class="text-muted-foreground">{l("cart.empty")}</p>
              <button type="button" class="btn btn-secondary" onClick={() => setOpen(false)}>
                {l("cart.continue")}
              </button>
            </div>
          }
        >
          <div class="flex-1 overflow-y-auto">
            <ul class="divide-y divide-border px-5" aria-busy={busy()}>
              <For each={lines()}>
                {(line) => (
                  <li class="flex gap-4 py-4">
                    {/* The photo repeats the name link next to it: out of the tab order, named. */}
                    <a
                      href={`${props.base}/p/${line.slug}`}
                      class="shrink-0"
                      tabIndex={-1}
                      aria-label={line.product_name}
                    >
                      <img
                        src={line.image ? imageUrl(line.image, 160) : undefined}
                        alt=""
                        width="64"
                        height="80"
                        loading="lazy"
                        class="h-20 w-16 rounded-md bg-muted object-cover"
                      />
                    </a>
                    <div class="flex min-w-0 flex-1 flex-col gap-1 text-sm">
                      <a
                        href={`${props.base}/p/${line.slug}`}
                        class="font-semibold hover:underline"
                      >
                        {line.product_name}
                      </a>
                      <p class="text-muted-foreground">{line.variant_label}</p>
                      <Show when={!line.available}>
                        <p class="font-semibold text-sale">{l("cart.unavailable")}</p>
                      </Show>
                      <div class="mt-auto flex items-center justify-between gap-2 pt-1">
                        <fieldset class="flex items-center rounded-md border border-border">
                          <legend class="sr-only">
                            {l("cart.quantity")}: {line.product_name}
                          </legend>
                          <button
                            type="button"
                            class="grid size-9 place-items-center disabled:text-subtle"
                            disabled={busy() || line.quantity <= 1}
                            onClick={() => change(line, line.quantity - 1)}
                          >
                            <Icon d={minus} class="size-4" />
                            <span class="sr-only">{l("cart.decrease")}</span>
                          </button>
                          <span class="w-7 text-center tabular-nums" aria-live="polite">
                            {line.quantity}
                          </span>
                          <button
                            type="button"
                            class="grid size-9 place-items-center disabled:text-subtle"
                            disabled={busy() || !line.available}
                            onClick={() => change(line, line.quantity + 1)}
                          >
                            <Icon d={plus} class="size-4" />
                            <span class="sr-only">{l("cart.increase")}</span>
                          </button>
                        </fieldset>
                        <button
                          type="button"
                          class="grid size-9 place-items-center rounded-md text-muted-foreground hover:bg-muted hover:text-sale"
                          disabled={busy()}
                          onClick={() => change(line, 0)}
                        >
                          <Icon d={trash} class="size-4" />
                          <span class="sr-only">
                            {l("cart.remove")}: {line.product_name}
                          </span>
                        </button>
                        <p class="price ml-auto text-base">{line.total.formatted}</p>
                      </div>
                    </div>
                  </li>
                )}
              </For>
            </ul>
            <Show when={crossSell().length > 0}>
              <section aria-labelledby="cart-cross-sell" class="border-t border-border px-5 py-4">
                <h3 id="cart-cross-sell" class="mb-3 text-sm font-bold">
                  {l("cart.cross_sell")}
                </h3>
                <ul class="flex snap-x gap-3 overflow-x-auto pb-1">
                  <For each={crossSell()}>
                    {(p) => (
                      <li class="w-28 shrink-0 snap-start">
                        <MiniCard product={p} base={props.base} labels={props.labels} />
                      </li>
                    )}
                  </For>
                </ul>
              </section>
            </Show>
          </div>
          <footer class="border-t border-border bg-background px-5 py-4">
            <Show when={failed()}>
              <p role="alert" class="mb-3 text-sm font-medium text-sale">
                {l("cart.update_failed")}
              </p>
            </Show>
            <Show when={cart()?.discount?.amount_minor}>
              <p class="mb-1 flex justify-between text-sm text-sale">
                <span>{l("cart.discount")}</span>
                <span class="price">−{cart()?.discount?.formatted}</span>
              </p>
            </Show>
            <p class="flex items-baseline justify-between">
              <span class="font-semibold">
                {l("cart.total")}{" "}
                <span class="text-sm font-normal text-muted-foreground">
                  ({l("cart.vat_included")})
                </span>
              </span>
              <span class="price text-2xl">{cart()?.total?.formatted}</span>
            </p>
            <p class="mt-1 mb-4 text-xs text-muted-foreground">{l("cart.shipping_note")}</p>
            {/* Edge-owned handoff: mints a one-time token and redirects to checkout.<host>. */}
            <form method="post" action="/_p/checkout/start">
              <button type="submit" class="btn btn-buy h-13 w-full text-lg">
                {l("cart.checkout")}
              </button>
            </form>
            <button
              type="button"
              class="mt-2 min-h-11 w-full text-sm font-semibold text-identity-ink underline underline-offset-4"
              onClick={() => setOpen(false)}
            >
              {l("cart.continue")}
            </button>
          </footer>
        </Show>
      </div>
    </dialog>
  );
}
