export interface CheckoutForm {
  step: "contact" | "addresses";
  build: (form: URLSearchParams) => unknown;
}

const field = (f: URLSearchParams, name: string) => f.get(name) ?? "";

const address = (f: URLSearchParams, prefix: "billing" | "shipping") => ({
  name: field(f, `${prefix}_name`),
  company: field(f, `${prefix}_company`),
  street: field(f, `${prefix}_street`),
  city: field(f, `${prefix}_city`),
  postal_code: field(f, `${prefix}_postal_code`),
  country: field(f, `${prefix}_country`),
  phone: field(f, `${prefix}_phone`),
});

export const PLACE_ORDER_FORM = "/checkout/place-order";

const whole = (f: URLSearchParams, name: string) => {
  const value = field(f, name);
  return /^\d{1,15}$/.test(value) ? Number(value) : null;
};

const ticked = (f: URLSearchParams, name: string) => f.get(name) === "1";

export const placeOrderBody = (f: URLSearchParams) => ({
  version: whole(f, "version"),
  total_minor: whole(f, "total_minor"),
  accept_terms: ticked(f, "accept_terms"),
  accept_withdrawal: ticked(f, "accept_withdrawal"),
  email_marketing: ticked(f, "email_marketing"),
  review_invites: ticked(f, "review_invites"),
  notes: f.get("notes") || null,
});

export const CHECKOUT_FORMS: ReadonlyMap<string, CheckoutForm> = new Map<string, CheckoutForm>([
  [
    "/checkout/contact",
    {
      step: "contact",
      build: (f) => ({ contact: { email: field(f, "email"), phone: field(f, "phone") } }),
    },
  ],
  [
    "/checkout/addresses",
    {
      step: "addresses",
      build: (f) => ({
        addresses: {
          billing: address(f, "billing"),
          shipping: f.get("ship_elsewhere") === "1" ? address(f, "shipping") : null,
        },
      }),
    },
  ],
]);
