/** Builds the `POST /storefront/v1/checkout/details` body from a form's own fields. */
export type CheckoutForm = (form: URLSearchParams) => unknown;

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

/**
 * The navigation forms that write customer details and rotate the checkout capability (C4, C6),
 * by exact path. A field outside a form's list is never forwarded, and no other path reaches the
 * API.
 */
export const CHECKOUT_FORMS: ReadonlyMap<string, CheckoutForm> = new Map<string, CheckoutForm>([
  [
    "/checkout/contact",
    (f) => ({ contact: { email: field(f, "email"), phone: field(f, "phone") } }),
  ],
  [
    "/checkout/addresses",
    (f) => ({
      addresses: {
        billing: address(f, "billing"),
        shipping: f.get("ship_elsewhere") === "1" ? address(f, "shipping") : null,
      },
    }),
  ],
]);
