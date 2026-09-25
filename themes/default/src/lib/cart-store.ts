import { cart as api } from "@platform/storefront-sdk/client";
import type { Cart } from "@platform/storefront-sdk/types";
import { createSignal } from "solid-js";

/**
 * Cart state shared by islands (mini cart, buy box). Islands are separate Solid roots, but
 * they import this module from the same chunk, so they share one signal.
 */
const [cart, setCart] = createSignal<Cart | null>(null);
const [open, setOpen] = createSignal(false);
let loading: Promise<void> | null = null;

export { cart, open, setOpen };

export function loadCart() {
  loading ??= api.get().then(
    (c) => void setCart(c),
    () => void setCart(null),
  );
  return loading;
}

export async function addToCart(variantId: string, quantity = 1) {
  setCart(await api.add(variantId, quantity));
}

export async function updateLine(lineId: string, quantity: number) {
  setCart(quantity > 0 ? await api.update(lineId, quantity) : await api.remove(lineId));
}
