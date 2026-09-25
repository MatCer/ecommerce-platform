/**
 * Stripe.js, loaded on demand on the order page only (the edge allows `js.stripe.com` there and
 * on the checkout page, WP11). Only the calls the Payment Element needs are typed.
 */
export interface StripeElements {
  create(type: "payment"): { mount(el: HTMLElement): void };
}

export interface StripeJs {
  elements(options: { clientSecret: string }): StripeElements;
  confirmPayment(options: {
    elements: StripeElements;
    confirmParams: { return_url: string };
  }): Promise<{ error?: { message?: string } }>;
}

type StripeFactory = (publishableKey: string, options: { stripeAccount: string }) => StripeJs;

const SRC = "https://js.stripe.com/v3/";
let loading: Promise<StripeFactory> | undefined;

function factory(): StripeFactory | undefined {
  const s: unknown = (window as unknown as { Stripe?: unknown }).Stripe;
  return typeof s === "function" ? (s as StripeFactory) : undefined;
}

/** Stripe for the shop's connected account (direct charges). */
export async function loadStripe(publishableKey: string, account: string): Promise<StripeJs> {
  loading ??= new Promise<StripeFactory>((resolve, reject) => {
    const ready = factory();
    if (ready) return resolve(ready);
    const script = document.createElement("script");
    script.src = SRC;
    script.async = true;
    script.onload = () => {
      const f = factory();
      if (f) resolve(f);
      else reject(new Error("Stripe.js did not load"));
    };
    script.onerror = () => reject(new Error("Stripe.js did not load"));
    document.head.append(script);
  });
  const create = await loading;
  return create(publishableKey, { stripeAccount: account });
}
