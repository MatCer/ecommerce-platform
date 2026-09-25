import { env } from "cloudflare:workers";
import type {
  Address,
  Cart,
  CheckoutView,
  ConsentState,
  Customer,
  DocumentLinks,
  NewsletterConfirmation,
  NewsletterPreferences,
  Order,
  OrderPage,
  ShopModel,
  WithdrawalForm,
} from "@platform/storefront-sdk/types";

/**
 * CHECKOUT binding calls. The edge injects tenant, market, the checkout-scoped cart capability
 * (`__Host-cart`), the customer session (`__Host-sid`) and the consent subject; this app never
 * sees credentials.
 */
export function checkoutApi(request: Request) {
  const ctx = request.headers.get("x-platform-ctx") ?? "";
  const get = async <T>(path: string): Promise<T | null> => {
    const res = await env.CHECKOUT.fetch(`https://checkout${path}`, {
      headers: { "x-platform-ctx": ctx },
    });
    // 404: no cart; 401: not signed in.
    if (res.status === 404 || res.status === 401) return null;
    if (!res.ok) throw new Error(`checkout API ${res.status}`);
    return (await res.json()) as T;
  };
  return {
    shop: () => get<ShopModel>("/shop"),
    cart: () => get<Cart>("/cart"),
    me: () => get<Customer>("/customer/me"),
    addresses: async () => (await get<{ items: Address[] }>("/customer/addresses"))?.items ?? [],
    consent: () => get<ConsentState>("/consent"),
    // WP10: the checkout of the handed-off cart, the order page, account orders.
    checkout: () => get<CheckoutView>("/checkout"),
    order: (token: string) =>
      /^[0-9a-f]{64}$/.test(token) ? get<Order>(`/orders/${token}`) : Promise.resolve(null),
    orders: async () => (await get<OrderPage>("/customer/orders"))?.items ?? [],
    customerOrder: (id: string) =>
      /^[0-9a-f-]{36}$/.test(id) ? get<Order>(`/customer/orders/${id}`) : Promise.resolve(null),
    // WP18: newsletter links (capability tokens from the emails; 404 = invalid or used).
    newsletterConfirmation: (token: string) =>
      TOKEN.test(token)
        ? get<NewsletterConfirmation>(`/newsletter/confirmation?token=${token}`)
        : Promise.resolve(null),
    newsletterPreferences: (token: string) =>
      TOKEN.test(token)
        ? get<NewsletterPreferences>(`/newsletter/preferences?t=${token}`)
        : Promise.resolve(null),
    withdrawalForm: (token: string) =>
      /^[0-9a-f]{64}$/.test(token)
        ? get<WithdrawalForm>(`/withdrawals/${token}`)
        : Promise.resolve(null),
    myWithdrawalForm: (id: string) =>
      /^[0-9a-f-]{36}$/.test(id)
        ? get<WithdrawalForm>(`/customer/orders/${id}/withdrawal`)
        : Promise.resolve(null),
    orderDocuments: (token: string) =>
      /^[0-9a-f]{64}$/.test(token)
        ? get<DocumentLinks>(`/orders/${token}/documents`)
        : Promise.resolve(null),
    myDocuments: (id: string) =>
      /^[0-9a-f-]{36}$/.test(id)
        ? get<DocumentLinks>(`/customer/orders/${id}/documents`)
        : Promise.resolve(null),
  };
}

const TOKEN = /^[0-9a-f]{64}$/;

/** The shop origin of this checkout origin (`checkout.demo.localhost` → `demo.localhost`). */
export function shopUrl(url: URL): string {
  return `${url.protocol}//${url.host.replace(/^checkout\./, "")}/`;
}

/** Countries of the tenant's markets (`cs-CZ` → `CZ`), for address forms. */
export function marketCountries(shop: ShopModel | null): string[] {
  const codes = (shop?.markets ?? [])
    .map((m) => m.locale.split("-")[1])
    .filter((c): c is string => typeof c === "string" && /^[A-Z]{2}$/.test(c));
  return codes.length > 0 ? [...new Set(codes)] : ["CZ", "SK"];
}

/** The messages islands need (they serialize props into the page): keys under `prefixes`. */
export function messages(m: Record<string, string>, ...prefixes: string[]): Record<string, string> {
  return Object.fromEntries(
    Object.entries(m).filter(([k]) => prefixes.some((p) => k.startsWith(p))),
  );
}
