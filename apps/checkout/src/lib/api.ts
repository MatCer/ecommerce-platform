import { env } from "cloudflare:workers";
import type { Cart, ShopModel } from "@platform/storefront-sdk/types";

/**
 * CHECKOUT binding calls. The edge injects tenant, market and the checkout-scoped cart
 * capability from the `__Host-cart` cookie; this app never sees credentials.
 */
export function checkoutApi(request: Request) {
  const ctx = request.headers.get("x-platform-ctx") ?? "";
  const get = async <T>(path: string): Promise<T | null> => {
    const res = await env.CHECKOUT.fetch(`https://checkout${path}`, {
      headers: { "x-platform-ctx": ctx },
    });
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`checkout API ${res.status}`);
    return (await res.json()) as T;
  };
  return {
    shop: () => get<ShopModel>("/shop"),
    cart: () => get<Cart>("/cart"),
  };
}
