import type { CheckoutAddress, CheckoutView } from "@platform/storefront-sdk/types";

export type DetailsStep = "contact" | "addresses";

const STEPS: readonly DetailsStep[] = ["contact", "addresses"];

export interface Problem {
  readonly step: DetailsStep | null;
  readonly field: "email" | "phone" | null;
  readonly message: string;
}

const PROBLEMS: Record<string, Problem> = {
  invalid_email: { step: "contact", field: "email", message: "account.invalid_email" },
  invalid_phone: { step: "contact", field: "phone", message: "address.invalid" },
  invalid_address: { step: "addresses", field: null, message: "address.invalid" },
  ship_to_not_allowed: { step: "addresses", field: null, message: "checkout.ship_to_not_allowed" },
  stale_page: { step: null, field: null, message: "checkout.stale_page" },
};
const FAILED: Problem = { step: null, field: null, message: "checkout.details_failed" };

export interface ContactValues {
  email: string;
  phone: string;
}
export interface AddressValues {
  billing: CheckoutAddress;
  shipping: CheckoutAddress;
  elsewhere: boolean;
}

export type CheckoutPage =
  | { kind: "contact"; values: ContactValues; problem: Problem | null; canCancel: boolean }
  | { kind: "addresses"; values: AddressValues; problem: Problem | null; canCancel: boolean }
  | { kind: "review"; problem: Problem | null };

const blank = (country: string): CheckoutAddress => ({
  name: "",
  company: null,
  street: "",
  city: "",
  postal_code: "",
  country,
  phone: null,
});

const drafted = (draft: FormData, prefix: "billing" | "shipping"): CheckoutAddress => {
  const field = (suffix: string) => String(draft.get(`${prefix}_${suffix}`) ?? "");
  return {
    name: field("name"),
    company: field("company"),
    street: field("street"),
    postal_code: field("postal_code"),
    city: field("city"),
    country: field("country"),
  };
};

const saved: Record<DetailsStep, (view: CheckoutView) => boolean> = {
  contact: (view) => Boolean(view.email),
  addresses: (view) => view.billing_address != null,
};

function openStep(view: CheckoutView, edit: DetailsStep | undefined, drafted: boolean) {
  if (drafted && edit) return edit;
  if (!saved.contact(view)) return "contact";
  if (edit) return edit;
  return saved.addresses(view) ? null : "addresses";
}

export function checkoutPage(view: CheckoutView, url: URL, submitted?: FormData): CheckoutPage {
  const edit = STEPS.find((s) => s === url.searchParams.get("edit"));
  const draft = edit ? submitted : undefined;
  const open = openStep(view, edit, draft !== undefined);
  const code = url.searchParams.get("error");
  const known = code === null ? null : (Object.hasOwn(PROBLEMS, code) && PROBLEMS[code]) || FAILED;
  const problem: Problem | null =
    known && known.step !== open ? { step: null, field: null, message: known.message } : known;
  if (open === null) return { kind: "review", problem };
  const canCancel = saved[open](view);
  if (open === "contact")
    return {
      kind: "contact",
      values: draft
        ? { email: String(draft.get("email") ?? ""), phone: String(draft.get("phone") ?? "") }
        : { email: view.email ?? "", phone: view.phone ?? "" },
      problem,
      canCancel,
    };
  const first = view.ship_to_countries[0] ?? "CZ";
  return {
    kind: "addresses",
    values: draft
      ? {
          billing: drafted(draft, "billing"),
          shipping: drafted(draft, "shipping"),
          elsewhere: draft.get("ship_elsewhere") === "1",
        }
      : {
          billing: view.billing_address ?? blank(first),
          shipping: view.shipping_address ?? blank(first),
          elsewhere: view.shipping_address != null,
        },
    problem,
    canCancel,
  };
}

export function regionName(locale: string): (code: string) => string {
  let names: Intl.DisplayNames;
  try {
    names = new Intl.DisplayNames([locale], { type: "region" });
  } catch {
    return (code) => code;
  }
  return (code) => {
    try {
      return names.of(code) ?? code;
    } catch {
      return code;
    }
  };
}

export function addressLine(a: CheckoutAddress, region: (code: string) => string): string {
  return [a.name, a.company, a.street, `${a.postal_code} ${a.city}`, region(a.country)]
    .filter(Boolean)
    .join(", ");
}

export interface ReviewFacts {
  readonly method: string | null;
  readonly destination: {
    readonly label: "checkout.pickup_selected" | "checkout.delivery_address";
    readonly line: string;
  } | null;
  readonly payment: string | null;
  readonly email: string | null;
}

export function reviewFacts(view: CheckoutView, region: (code: string) => string): ReviewFacts {
  const method = view.shipping_methods.find((s) => s.id === view.shipping_method_id) ?? null;
  const point = view.pickup_point;
  const address = view.shipping_address ?? view.billing_address;
  const destination = method?.needs_pickup_point
    ? point
      ? {
          label: "checkout.pickup_selected" as const,
          line: `${point.name}, ${point.street}, ${point.zip} ${point.city}`,
        }
      : null
    : address
      ? { label: "checkout.delivery_address" as const, line: addressLine(address, region) }
      : null;
  return {
    method: method?.name ?? null,
    destination,
    payment: view.payment_methods.find((p) => p.kind === view.payment_method)?.name ?? null,
    email: view.email ?? null,
  };
}
