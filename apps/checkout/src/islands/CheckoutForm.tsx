import { t } from "@platform/storefront-sdk/format";
import type {
  Address,
  CheckoutAddress,
  CheckoutView,
  PaymentMethodKind,
  PickupPoint,
  ShippingOption,
} from "@platform/storefront-sdk/types";
import { createSignal, For, type JSX, onMount, Show } from "solid-js";
import { call } from "../lib/client";
import { loadPacketa, toPickupPoint } from "../lib/packeta";
import {
  forgetPlacement,
  nextUrl,
  type Placement,
  sendPlacement,
  unknownOutcome,
} from "../lib/placement";
import { Button, Checkbox, SelectField, TextField } from "../ui.tsx";

import HydratedControls from "./HydratedControls";

type M = Record<string, string>;

const blank = (country: string): CheckoutAddress => ({
  name: "",
  company: null,
  street: "",
  city: "",
  postal_code: "",
  country,
  phone: null,
});

const fromSaved = (a: Address): CheckoutAddress => ({
  name: a.name,
  company: a.company ?? null,
  street: a.street,
  city: a.city,
  postal_code: a.postal_code,
  country: a.country,
  phone: a.phone ?? null,
});

const complete = (a: CheckoutAddress) =>
  [a.name, a.street, a.city, a.postal_code].every((v) => v.trim() !== "");

/** The user-facing text for a problem+json code of the checkout API. */
export function checkoutProblem(m: M, code: string | null): string {
  switch (code) {
    case "cart_changed":
    case "price_changed":
      return t(m, "checkout.changed");
    case "insufficient_stock":
    case "out_of_stock":
    case "cart_unavailable":
    case "cart_empty":
      return t(m, "checkout.stock");
    case "order_already_placed":
      return t(m, "checkout.already_placed");
    case "ship_to_not_allowed":
    case "pickup_point_country":
      return t(m, "checkout.ship_to_not_allowed");
    case "legal_consent_required":
      return t(m, "checkout.required_legal");
    case "checkout_incomplete":
    case "pickup_point_required":
    case "cod_not_allowed":
    case "shipping_unavailable":
    case "payment_method_unavailable":
      return t(m, "checkout.incomplete");
    case "invalid_email":
      return t(m, "account.invalid_email");
    case "invalid_address":
    case "invalid_phone":
      return t(m, "address.invalid");
    default:
      return code?.startsWith("coupon_") ? t(m, "checkout.coupon_invalid") : t(m, "checkout.error");
  }
}

/** A fresh key per placement; a replay after an unknown outcome reuses it (A12). */
const newKey = () =>
  typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) =>
        b.toString(16).padStart(2, "0"),
      ).join("");

export default function CheckoutForm(props: {
  m: M;
  locale: string;
  initial: CheckoutView;
  customerEmail?: string | undefined;
  saved: Address[];
  shopUrl: string;
}) {
  const m = props.m;
  const [view, setView] = createSignal(props.initial);
  const firstCountry = () => props.initial.ship_to_countries[0] ?? "CZ";
  const savedDefault = props.saved.find((a) => a.is_default) ?? props.saved[0];
  const [email, setEmail] = createSignal(props.initial.email ?? props.customerEmail ?? "");
  const [phone, setPhone] = createSignal(props.initial.phone ?? "");
  const [billing, setBilling] = createSignal<CheckoutAddress>(
    props.initial.billing_address ??
      (savedDefault && props.initial.ship_to_countries.includes(savedDefault.country)
        ? fromSaved(savedDefault)
        : blank(firstCountry())),
  );
  const [elsewhere, setElsewhere] = createSignal(props.initial.shipping_address != null);
  const [delivery, setDelivery] = createSignal<CheckoutAddress>(
    props.initial.shipping_address ?? blank(firstCountry()),
  );
  const [terms, setTerms] = createSignal(false);
  const [withdrawal, setWithdrawal] = createSignal(false);
  const [marketing, setMarketing] = createSignal(false);
  const [reviews, setReviews] = createSignal(false);
  const [note, setNote] = createSignal("");
  /** A pickup-point method the customer chose but has no point for yet. */
  const [pendingPickup, setPendingPickup] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [placing, setPlacing] = createSignal(false);
  const [error, setError] = createSignal("");
  const [notice, setNotice] = createSignal("");
  const [touched, setTouched] = createSignal(false);
  /** A placement whose outcome is unknown, replayed as is on the next click. */
  // An open cart means no order came of an earlier placement: nothing to resume (a converted
  // cart shows the empty page, whose ResumePlacement replays a kept placement).
  let pending: Placement | null = null;
  onMount(forgetPlacement);

  const regions = (() => {
    try {
      return new Intl.DisplayNames([props.locale], { type: "region" });
    } catch {
      return null;
    }
  })();
  const countries = () =>
    view().ship_to_countries.map((c) => ({ value: c, label: regions?.of(c) ?? c }));
  const shipCountry = () => (elsewhere() ? delivery() : billing()).country;
  const selectedMethod = () =>
    view().shipping_methods.find((s) => s.id === view().shipping_method_id) ?? null;

  /** Saves one step; the answer is the recomputed checkout. */
  async function put(path: string, body: unknown): Promise<boolean> {
    setSaving(true);
    setError("");
    const r = await call<CheckoutView>("PUT", `/_p/checkout/${path}`, body);
    setSaving(false);
    if (r.ok && r.data) {
      setView(r.data);
      return true;
    }
    setError(checkoutProblem(m, r.code));
    return false;
  }

  async function chooseShipping(s: ShippingOption) {
    if (!s.needs_pickup_point) {
      setPendingPickup(null);
      await put("shipping", { method_id: s.id });
      return;
    }
    // Pickup points: the widget first; the method is saved together with the point.
    setPendingPickup(s.id);
    await openWidget(s.id);
  }

  async function openWidget(methodId: string) {
    const widget = view().packeta;
    const Packeta = widget ? await loadPacketa(widget.script_url).catch(() => null) : null;
    if (!widget || !Packeta) {
      setError(t(m, "checkout.pickup_unavailable"));
      return;
    }
    Packeta.Widget.pick(
      widget.api_key,
      async (raw) => {
        const point: PickupPoint | null = toPickupPoint(raw);
        if (!point) return;
        if (await put("shipping", { method_id: methodId, pickup_point: point })) {
          setPendingPickup(null);
        }
      },
      { country: shipCountry().toLowerCase(), language: props.locale.slice(0, 2) },
    );
  }

  async function choosePayment(kind: PaymentMethodKind) {
    await put("payment", { method: kind });
  }

  /** Saving disables the fieldset and replaces its options. Return keyboard focus to the choice. */
  async function keepChoiceFocus(input: HTMLInputElement, save: () => Promise<void>) {
    const focused =
      document.activeElement instanceof HTMLInputElement &&
      document.activeElement.name === input.name;
    const { name, value } = input;
    await save();
    if (focused && document.activeElement === document.body) {
      for (const candidate of document.getElementsByName(name)) {
        if (candidate instanceof HTMLInputElement && candidate.value === value) {
          candidate.focus();
          break;
        }
      }
    }
  }

  const emailValid = () => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email().trim());
  const addressesValid = () => complete(billing()) && (!elsewhere() || complete(delivery()));

  /** Sends a placement; an unknown outcome keeps it for an exact replay (A12). */
  async function submit(p: Placement) {
    const r = await sendPlacement(p);
    if (r.ok && r.data) {
      location.assign(nextUrl(r.data));
      return;
    }
    setPlacing(false);
    // Lost response, gateway error or still running: the order may exist. The next click (or
    // a reload) replays the same key and body; no step is saved before it, so nothing changes.
    pending = unknownOutcome(r) ? p : null;
    setError(checkoutProblem(m, r.code));
    if (r.code === "cart_changed" || r.code === "price_changed") {
      const fresh = await call<CheckoutView>("GET", "/_p/checkout");
      if (fresh.ok && fresh.data) setView(fresh.data);
    }
  }

  async function place(e: SubmitEvent) {
    e.preventDefault();
    setTouched(true);
    setError("");
    setNotice("");
    if (pending) {
      setPlacing(true);
      return submit(pending);
    }
    const v = view();
    if (!emailValid() || !addressesValid()) return setError(t(m, "checkout.incomplete"));
    if (!v.shipping_method_id || !v.payment_method) return setError(t(m, "checkout.incomplete"));
    if (pendingPickup() || (selectedMethod()?.needs_pickup_point && !v.pickup_point))
      return setError(t(m, "checkout.pickup_missing"));
    if (!terms() || !withdrawal()) return setError(t(m, "checkout.required_legal"));
    setPlacing(true);
    const shown = v.totals.total.amount_minor;
    const ok =
      (await put("contact", { email: email().trim(), phone: phone().trim() || null })) &&
      (await put("addresses", {
        billing: billing(),
        shipping: elsewhere() ? delivery() : null,
      }));
    if (!ok) return setPlacing(false);
    const now = view();
    // The delivery country sets the VAT (A3): a different total must be seen before ordering.
    if (now.totals.total.amount_minor !== shown) {
      setPlacing(false);
      return setNotice(t(m, "checkout.price_updated"));
    }
    await submit({
      key: newKey(),
      at: Date.now(),
      body: {
        version: now.cart.version,
        total_minor: now.totals.total.amount_minor,
        accept_terms: true,
        accept_withdrawal: true,
        email_marketing: marketing(),
        review_invites: reviews(),
        notes: note().trim() || null,
      },
    });
  }

  const section = "rounded-lg border border-border bg-card p-4 sm:p-5";
  const h2 = "mb-3 font-display text-lg font-bold";
  const option =
    "flex cursor-pointer items-start gap-3 rounded-md border border-border p-3 has-[:checked]:border-identity has-[:checked]:bg-identity-wash has-[:disabled]:cursor-not-allowed has-[:disabled]:opacity-60";

  const addressFields = (
    a: () => CheckoutAddress,
    set: (v: CheckoutAddress) => void,
    idPrefix: string,
  ): JSX.Element => {
    const upd = <K extends keyof CheckoutAddress>(k: K, v: CheckoutAddress[K]) =>
      set({ ...a(), [k]: v });
    const err = (v: string) => (touched() && !v.trim() ? t(m, "address.invalid") : undefined);
    return (
      <div class="grid gap-3 sm:grid-cols-2">
        <TextField
          class="sm:col-span-2"
          label={t(m, "address.name")}
          name={`${idPrefix}-name`}
          autocomplete={`section-${idPrefix} name`}
          required
          value={a().name}
          onChange={(v) => upd("name", v)}
          error={err(a().name)}
        />
        <TextField
          class="sm:col-span-2"
          label={t(m, "address.company")}
          autocomplete={`section-${idPrefix} organization`}
          value={a().company ?? ""}
          onChange={(v) => upd("company", v || null)}
        />
        <TextField
          class="sm:col-span-2"
          label={t(m, "address.street")}
          autocomplete={`section-${idPrefix} street-address`}
          required
          value={a().street}
          onChange={(v) => upd("street", v)}
          error={err(a().street)}
        />
        <TextField
          label={t(m, "address.postal_code")}
          autocomplete={`section-${idPrefix} postal-code`}
          required
          value={a().postal_code}
          onChange={(v) => upd("postal_code", v)}
          error={err(a().postal_code)}
        />
        <TextField
          label={t(m, "address.city")}
          autocomplete={`section-${idPrefix} address-level2`}
          required
          value={a().city}
          onChange={(v) => upd("city", v)}
          error={err(a().city)}
        />
        <SelectField
          class="sm:col-span-2"
          label={t(m, "address.country")}
          value={a().country}
          options={countries()}
          onChange={(v) => upd("country", v)}
        />
      </div>
    );
  };

  return (
    <form noValidate onSubmit={place}>
      <HydratedControls class="grid gap-6 md:grid-cols-[1fr_20rem] md:items-start">
        <div class="grid min-w-0 gap-5">
          <section aria-labelledby="co-contact" class={section}>
            <h2 id="co-contact" class={h2}>
              1. {t(m, "checkout.contact")}
            </h2>
            <Show
              when={props.customerEmail}
              fallback={
                <p class="mb-3 text-sm">
                  <a href="/account?next=/" class="text-identity-ink underline">
                    {t(m, "checkout.have_account")}
                  </a>
                </p>
              }
            >
              <p class="mb-3 text-sm text-muted-foreground">
                {t(m, "checkout.signed_in", { email: props.customerEmail ?? "" })}
              </p>
            </Show>
            <div class="grid gap-3 sm:grid-cols-2">
              <TextField
                label={t(m, "account.email")}
                type="email"
                autocomplete="email"
                required
                value={email()}
                onChange={setEmail}
                description={t(m, "checkout.email_hint")}
                error={touched() && !emailValid() ? t(m, "account.invalid_email") : undefined}
              />
              <TextField
                label={t(m, "address.phone")}
                type="tel"
                autocomplete="tel"
                value={phone()}
                onChange={setPhone}
              />
            </div>
          </section>

          <section aria-labelledby="co-address" class={section}>
            <h2 id="co-address" class={h2}>
              2. {t(m, "checkout.address")}
            </h2>
            <Show when={props.saved.length > 0}>
              <SelectField
                class="mb-3"
                label={t(m, "checkout.saved_address")}
                value=""
                options={[
                  { value: "", label: t(m, "checkout.saved_address_none") },
                  ...props.saved.map((a) => ({
                    value: a.id,
                    label: `${a.name}, ${a.street}, ${a.city}`,
                  })),
                ]}
                onChange={(id) => {
                  const a = props.saved.find((s) => s.id === id);
                  if (a) setBilling(fromSaved(a));
                }}
              />
            </Show>
            {addressFields(billing, setBilling, "billing")}
            <Checkbox
              class="mt-4"
              label={t(m, "checkout.ship_elsewhere")}
              checked={elsewhere()}
              onChange={setElsewhere}
            />
            <Show when={elsewhere()}>
              <fieldset class="mt-4 border-t border-border pt-4">
                <legend class="mb-2 text-sm font-semibold">
                  {t(m, "checkout.delivery_address")}
                </legend>
                {addressFields(delivery, setDelivery, "shipping")}
              </fieldset>
            </Show>
          </section>

          <section aria-labelledby="co-shipping" class={section}>
            <h2 id="co-shipping" class={h2}>
              3. {t(m, "checkout.shipping")}
            </h2>
            <fieldset class="grid gap-2" aria-labelledby="co-shipping" disabled={saving()}>
              <For each={view().shipping_methods}>
                {(s) => (
                  <label class={option}>
                    <input
                      type="radio"
                      name="shipping"
                      class="mt-1 size-4 accent-[var(--color-identity)]"
                      value={s.id}
                      checked={(pendingPickup() ?? view().shipping_method_id) === s.id}
                      disabled={s.price == null}
                      onChange={(e) =>
                        void keepChoiceFocus(e.currentTarget, () => chooseShipping(s))
                      }
                    />
                    <span class="grid flex-1 gap-0.5">
                      <span class="flex justify-between gap-2 font-semibold">
                        <span>{s.name}</span>
                        <span class="tabular-nums">
                          {s.price == null
                            ? t(m, "checkout.shipping_unavailable")
                            : s.price.amount_minor === 0
                              ? t(m, "checkout.free")
                              : s.price.formatted}
                        </span>
                      </span>
                      <Show when={s.description}>
                        <span class="text-sm text-muted-foreground">{s.description}</span>
                      </Show>
                    </span>
                  </label>
                )}
              </For>
            </fieldset>
            <Show when={pickupMethod(view(), pendingPickup())}>
              {(method) => (
                <div class="mt-3 grid gap-2 rounded-md bg-muted p-3 text-sm">
                  <Show
                    when={!pendingPickup() && view().pickup_point}
                    fallback={<p>{t(m, "checkout.pickup_missing")}</p>}
                  >
                    {(p) => (
                      <p data-testid="pickup-point">
                        <span class="font-semibold">{t(m, "checkout.pickup_selected")}:</span>{" "}
                        {p().name}, {p().street}, {p().zip} {p().city}
                      </p>
                    )}
                  </Show>
                  <div>
                    <Button variant="secondary" onClick={() => void openWidget(method().id)}>
                      {view().pickup_point && !pendingPickup()
                        ? t(m, "checkout.pickup_change")
                        : t(m, "checkout.pickup_choose")}
                    </Button>
                  </div>
                </div>
              )}
            </Show>
          </section>

          <section aria-labelledby="co-payment" class={section}>
            <h2 id="co-payment" class={h2}>
              4. {t(m, "checkout.payment")}
            </h2>
            <fieldset class="grid gap-2" aria-labelledby="co-payment" disabled={saving()}>
              <For each={view().payment_methods}>
                {(p) => (
                  <label class={option}>
                    <input
                      type="radio"
                      name="payment"
                      class="mt-1 size-4 accent-[var(--color-identity)]"
                      value={p.kind}
                      checked={view().payment_method === p.kind}
                      disabled={!p.selectable}
                      onChange={(e) =>
                        void keepChoiceFocus(e.currentTarget, () => choosePayment(p.kind))
                      }
                    />
                    <span class="grid flex-1 gap-0.5">
                      <span class="flex justify-between gap-2 font-semibold">
                        <span>{p.name}</span>
                        <Show when={p.fee.amount_minor > 0}>
                          <span class="tabular-nums">+{p.fee.formatted}</span>
                        </Show>
                      </span>
                      <Show when={!p.selectable}>
                        <span class="text-sm text-muted-foreground">
                          {t(m, "checkout.cod_needs_shipping")}
                        </span>
                      </Show>
                    </span>
                  </label>
                )}
              </For>
            </fieldset>
          </section>

          <section aria-labelledby="co-review" class={section}>
            <h2 id="co-review" class={h2}>
              5. {t(m, "checkout.review")}
            </h2>
            <div class="grid gap-3">
              <div class="flex flex-wrap items-start gap-x-2">
                <Checkbox
                  label={t(m, "checkout.accept_terms")}
                  checked={terms()}
                  onChange={setTerms}
                />
                <a
                  href={view().legal.terms_url}
                  target="_blank"
                  rel="noopener"
                  class="text-sm text-identity-ink underline"
                >
                  {t(m, "legal.terms")}
                </a>
              </div>
              <div class="flex flex-wrap items-start gap-x-2">
                <Checkbox
                  label={t(m, "checkout.accept_withdrawal")}
                  checked={withdrawal()}
                  onChange={setWithdrawal}
                />
                <a
                  href={view().legal.withdrawal_url}
                  target="_blank"
                  rel="noopener"
                  class="text-sm text-identity-ink underline"
                >
                  {t(m, "checkout.withdrawal_info")}
                </a>
              </div>
              <fieldset class="grid gap-3 border-t border-border pt-3">
                <legend class="mb-1 text-sm text-muted-foreground">
                  {t(m, "checkout.optional")}
                </legend>
                <Checkbox
                  label={t(m, "checkout.marketing")}
                  description={t(m, "consent.email_marketing_hint")}
                  checked={marketing()}
                  onChange={setMarketing}
                />
                <Checkbox
                  label={t(m, "checkout.reviews")}
                  checked={reviews()}
                  onChange={setReviews}
                />
              </fieldset>
              <TextField
                label={t(m, "checkout.note")}
                multiline
                rows={2}
                maxLength={1000}
                value={note()}
                onChange={setNote}
              />
            </div>
          </section>
        </div>

        <aside aria-labelledby="co-summary" class={`${section} md:sticky md:top-4`}>
          <h2 id="co-summary" class={h2}>
            {t(m, "checkout.summary")}
          </h2>
          <ul class="grid gap-2 text-sm">
            <For each={view().cart.lines}>
              {(l) => (
                <li class="flex justify-between gap-2">
                  <span>
                    {l.quantity}× {l.product_name}
                    <Show when={l.variant_label}>
                      <span class="text-muted-foreground"> ({l.variant_label})</span>
                    </Show>
                  </span>
                  <span class="tabular-nums">{l.total.formatted}</span>
                </li>
              )}
            </For>
          </ul>
          <a href={props.shopUrl} class="mt-2 inline-block text-sm text-identity-ink underline">
            {t(m, "checkout.edit_cart")}
          </a>
          <dl class="mt-3 grid gap-1 border-t border-border pt-3 text-sm">
            <Row label={t(m, "checkout.subtotal")} value={view().totals.subtotal.formatted} />
            <Show when={view().totals.discount.amount_minor > 0}>
              <Row
                class="text-sale"
                label={`${t(m, "cart.discount")}${view().cart.coupon ? ` (${view().cart.coupon?.code})` : ""}`}
                value={`−${view().totals.discount.formatted}`}
              />
            </Show>
            <Row label={t(m, "checkout.shipping")} value={view().totals.shipping.formatted} />
            <Show when={view().totals.payment_fee.amount_minor !== 0}>
              <Row
                label={t(m, "checkout.payment_fee")}
                value={view().totals.payment_fee.formatted}
              />
            </Show>
            <For each={view().totals.vat}>
              {(r) => (
                <Row
                  class="text-muted-foreground"
                  label={t(m, "checkout.vat", { rate: r.rate })}
                  value={r.vat.formatted}
                />
              )}
            </For>
            <div class="mt-1 flex justify-between border-t border-border pt-2 text-base font-bold">
              <dt>{t(m, "cart.total")}</dt>
              <dd class="tabular-nums" data-testid="checkout-total">
                {view().totals.total.formatted}
              </dd>
            </div>
          </dl>
          <div aria-live="polite" class="mt-3 text-sm">
            <Show when={notice()}>
              <p role="status" class="rounded-md bg-identity-wash p-2">
                {notice()}
              </p>
            </Show>
          </div>
          <Show when={error()}>
            <p role="alert" class="mt-3 rounded-md border border-sale p-2 text-sm text-sale">
              {error()}
            </p>
          </Show>
          <button
            type="submit"
            class="mt-4 h-12 w-full rounded-md bg-buy px-4 font-display font-bold hover:bg-buy-hover disabled:cursor-not-allowed disabled:opacity-60"
            disabled={placing() || saving()}
            aria-busy={placing() || undefined}
          >
            {placing() ? t(m, "checkout.placing") : t(m, "checkout.place")}
          </button>
        </aside>
      </HydratedControls>
    </form>
  );
}

function pickupMethod(v: CheckoutView, pending: string | null): ShippingOption | undefined {
  const id = pending ?? v.shipping_method_id;
  return v.shipping_methods.find((s) => s.id === id && s.needs_pickup_point);
}

function Row(props: { label: string; value: string; class?: string }) {
  return (
    <div class={`flex justify-between gap-2 ${props.class ?? ""}`}>
      <dt>{props.label}</dt>
      <dd class="tabular-nums">{props.value}</dd>
    </div>
  );
}
