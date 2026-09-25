import { cart as api } from "@platform/storefront-sdk/client";
import type { CartState } from "@platform/storefront-sdk/types";
import { createSignal } from "solid-js";

/**
 * Cart state shared by islands (mini cart, buy box). Islands are separate Solid roots, but they
 * import this module from the same chunk, so they share one signal. The cart itself lives on
 * the server behind the HttpOnly `cart` capability cookie (A4); this is only its last answer.
 */
const [cart, setCart] = createSignal<CartState | null>(null);
const [open, setOpen] = createSignal(false);
/** Increments whenever an item lands, to animate the cart count. */
const [added, setAdded] = createSignal(0);
let loading: Promise<void> | null = null;

export { added, cart, open, setOpen };

export function loadCart() {
  loading ??= api.get().then(
    (c) => void setCart(c),
    () => void setCart(null),
  );
  return loading;
}

/** Adds a variant; retried adds reuse the key, so a lost response is not counted twice. */
export async function addToCart(variantId: string, quantity = 1, key = crypto.randomUUID()) {
  setCart(await api.add(variantId, quantity, key));
  setAdded(added() + 1);
}

export async function updateLine(lineId: string, quantity: number) {
  setCart(quantity > 0 ? await api.update(lineId, quantity) : await api.remove(lineId));
}
