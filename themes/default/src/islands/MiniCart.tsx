import { imageUrl } from "@platform/storefront-sdk/format";
import { createEffect, For, onMount, Show } from "solid-js";
import { cart, loadCart, open, setOpen, updateLine } from "../lib/cart-store";

/** Header cart button + drawer with free-shipping progress and the checkout handoff form (A1). */
export default function MiniCart() {
  let dialog: HTMLDialogElement | undefined;
  onMount(() => void loadCart());
  createEffect(() => {
    if (!dialog) return;
    if (open() && !dialog.open) dialog.showModal();
    if (!open() && dialog.open) dialog.close();
  });
  const count = () => cart()?.item_count ?? 0;

  return (
    <>
      <button
        type="button"
        class="relative inline-flex h-10 items-center gap-2 rounded-md border border-border bg-card px-3 text-sm font-semibold"
        onClick={() => setOpen(true)}
        aria-haspopup="dialog"
      >
        <svg
          aria-hidden="true"
          viewBox="0 0 24 24"
          class="size-5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.8"
        >
          <path d="M3 4h2l2.4 11.2a2 2 0 0 0 2 1.6h7.7a2 2 0 0 0 2-1.5L21 8H6" />
          <circle cx="10" cy="20" r="1.3" />
          <circle cx="17" cy="20" r="1.3" />
        </svg>
        <span class="sr-only md:not-sr-only">Košík</span>
        <span class="grid min-w-5 place-items-center rounded-full bg-identity px-1 text-xs text-card tabular-nums">
          {count()}
          <span class="sr-only"> položek</span>
        </span>
      </button>

      <dialog
        ref={dialog}
        onClose={() => setOpen(false)}
        class="ml-auto h-dvh max-h-none w-full max-w-md bg-card p-0 text-foreground backdrop:bg-foreground/40"
        aria-label="Košík"
      >
        <div class="flex h-full flex-col">
          <header class="flex items-center justify-between border-b border-border p-4">
            <h2 class="font-display text-lg font-bold">Košík</h2>
            <button
              type="button"
              class="rounded-md px-2 py-1 text-sm underline"
              onClick={() => setOpen(false)}
            >
              Zavřít
            </button>
          </header>
          <Show
            when={cart()?.lines.length}
            fallback={<p class="p-4 text-muted-foreground">Košík je prázdný.</p>}
          >
            <Show
              when={cart()?.free_shipping_remaining}
              fallback={
                <p class="bg-identity-wash p-4 text-sm font-semibold text-identity-ink">
                  Doprava zdarma ✓
                </p>
              }
            >
              {(rest) => (
                <p class="bg-identity-wash p-4 text-sm text-identity-ink">
                  Do dopravy zdarma zbývá {rest().formatted}
                </p>
              )}
            </Show>
            <ul class="flex-1 divide-y divide-border overflow-y-auto px-4">
              <For each={cart()?.lines}>
                {(line) => (
                  <li class="flex gap-3 py-3">
                    <img
                      src={imageUrl(line.image, 120)}
                      alt=""
                      width="60"
                      height="75"
                      loading="lazy"
                      class="rounded-sm bg-muted"
                    />
                    <div class="flex-1 text-sm">
                      <p class="font-semibold">{line.product_name}</p>
                      <p class="text-muted-foreground">{line.variant_label}</p>
                      <div class="mt-1 flex items-center gap-2">
                        <button
                          type="button"
                          class="size-7 rounded-sm border border-border"
                          aria-label="Ubrat"
                          onClick={() => updateLine(line.id, line.quantity - 1)}
                        >
                          −
                        </button>
                        <span class="tabular-nums">{line.quantity}</span>
                        <button
                          type="button"
                          class="size-7 rounded-sm border border-border"
                          aria-label="Přidat"
                          onClick={() => updateLine(line.id, line.quantity + 1)}
                        >
                          +
                        </button>
                      </div>
                    </div>
                    <p class="price text-sm">{line.total.formatted}</p>
                  </li>
                )}
              </For>
            </ul>
            <footer class="border-t border-border p-4">
              <p class="mb-3 flex justify-between text-sm">
                <span>Mezisoučet</span>
                <span class="price text-lg">{cart()?.subtotal?.formatted}</span>
              </p>
              {/* Edge-owned handoff: mints a one-time token and redirects to checkout.<host>. */}
              <form method="post" action="/_p/checkout/start">
                <button
                  type="submit"
                  class="h-12 w-full rounded-md bg-buy font-display font-bold text-foreground hover:bg-buy-hover"
                >
                  Pokračovat k pokladně
                </button>
              </form>
            </footer>
          </Show>
        </div>
      </dialog>
    </>
  );
}
