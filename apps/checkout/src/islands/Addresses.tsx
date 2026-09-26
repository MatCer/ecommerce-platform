import { t } from "@platform/storefront-sdk/format";
import type { Address, AddressInput } from "@platform/storefront-sdk/types";
import { Button, Checkbox, SelectField, TextField } from "@platform/ui";
import { createSignal, For, Show } from "solid-js";
import { call } from "../lib/client";
import HydratedControls from "./HydratedControls";
import { problemText } from "./SignIn";

type M = Record<string, string>;

const empty = (country: string): AddressInput => ({
  name: "",
  company: null,
  street: "",
  city: "",
  postal_code: "",
  country,
  phone: null,
  is_default: false,
});

export default function Addresses(props: {
  m: M;
  locale: string;
  countries: string[];
  initial: Address[];
}) {
  const m = props.m;
  const [items, setItems] = createSignal(props.initial);
  /** `undefined`: no form; `null`: adding; an id: editing that address. */
  const [editing, setEditing] = createSignal<string | null | undefined>(undefined);
  const [form, setForm] = createSignal<AddressInput>(empty(props.countries[0] ?? "CZ"));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [status, setStatus] = createSignal("");

  const regions = (() => {
    try {
      return new Intl.DisplayNames([props.locale], { type: "region" });
    } catch {
      return null;
    }
  })();
  const countryName = (c: string) => regions?.of(c) ?? c;
  const set = <K extends keyof AddressInput>(k: K, v: AddressInput[K]) =>
    setForm({ ...form(), [k]: v });

  async function reload() {
    const r = await call<{ items: Address[] }>("GET", "/_p/account/addresses");
    if (r.ok && r.data) setItems(r.data.items);
  }

  function open(a?: Address) {
    setError("");
    setStatus("");
    setEditing(a ? a.id : null);
    // Only the input fields: the API refuses unknown ones (`id`).
    setForm(
      a
        ? {
            name: a.name,
            company: a.company,
            street: a.street,
            city: a.city,
            postal_code: a.postal_code,
            country: a.country,
            phone: a.phone,
            is_default: a.is_default,
          }
        : empty(props.countries[0] ?? "CZ"),
    );
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    const f = form();
    if (![f.name, f.street, f.city, f.postal_code].every((v) => v.trim())) {
      return setError(t(m, "address.invalid"));
    }
    setBusy(true);
    const id = editing();
    const r = id
      ? await call("PUT", `/_p/account/addresses/${id}`, f)
      : await call("POST", "/_p/account/addresses", f);
    setBusy(false);
    if (!r.ok) {
      return setError(
        r.code === "invalid_address" ? t(m, "address.invalid") : problemText(m, r.code),
      );
    }
    setEditing(undefined);
    setStatus(t(m, "address.saved"));
    await reload();
  }

  async function remove(a: Address) {
    setStatus("");
    const r = await call("DELETE", `/_p/account/addresses/${a.id}`);
    if (!r.ok) return setError(problemText(m, r.code));
    setStatus(t(m, "address.deleted"));
    await reload();
  }

  const countryOptions = () =>
    [...new Set([...props.countries, form().country])].map((c) => ({
      value: c,
      label: countryName(c),
    }));

  return (
    <HydratedControls class="grid gap-4">
      <div aria-live="polite">
        <Show when={status()}>
          <p role="status" class="rounded-md bg-identity-wash p-3 text-sm">
            {status()}
          </p>
        </Show>
      </div>
      <Show
        when={items().length > 0}
        fallback={<p class="text-sm text-muted-foreground">{t(m, "address.empty")}</p>}
      >
        <ul class="grid gap-3">
          <For each={items()}>
            {(a) => (
              <li class="flex flex-wrap items-start justify-between gap-3 rounded-lg border border-border bg-card p-4">
                <address class="text-sm not-italic leading-relaxed">
                  <strong>{a.name}</strong>
                  <Show when={a.is_default}>
                    {" "}
                    <span class="ml-1 rounded-sm bg-identity-wash px-1.5 py-0.5 text-xs font-semibold text-identity-ink">
                      {t(m, "address.default")}
                    </span>
                  </Show>
                  <Show when={a.company}>
                    <br />
                    {a.company}
                  </Show>
                  <br />
                  {a.street}
                  <br />
                  {a.postal_code} {a.city}
                  <br />
                  {countryName(a.country)}
                  <Show when={a.phone}>
                    <br />
                    {a.phone}
                  </Show>
                </address>
                <div class="flex gap-2">
                  <Button
                    onClick={() => open(a)}
                    aria-label={`${t(m, "address.edit")}: ${a.name}, ${a.street}`}
                  >
                    {t(m, "address.edit")}
                  </Button>
                  <Button
                    category="tertiary"
                    onClick={() => remove(a)}
                    aria-label={`${t(m, "address.delete")}: ${a.name}, ${a.street}`}
                  >
                    {t(m, "address.delete")}
                  </Button>
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show
        when={editing() !== undefined}
        fallback={
          <div>
            <Button variant="confirm" onClick={() => open()}>
              {t(m, "address.add")}
            </Button>
          </div>
        }
      >
        <form
          class="grid gap-3 rounded-lg border border-border bg-card p-4 sm:grid-cols-2"
          noValidate
          onSubmit={save}
        >
          <TextField
            class="sm:col-span-2"
            label={t(m, "address.name")}
            autocomplete="name"
            required
            value={form().name}
            onChange={(v) => set("name", v)}
            maxLength={200}
          />
          <TextField
            class="sm:col-span-2"
            label={t(m, "address.company")}
            autocomplete="organization"
            value={form().company ?? ""}
            onChange={(v) => set("company", v || null)}
            maxLength={200}
          />
          <TextField
            class="sm:col-span-2"
            label={t(m, "address.street")}
            autocomplete="street-address"
            required
            value={form().street}
            onChange={(v) => set("street", v)}
            maxLength={200}
          />
          <TextField
            label={t(m, "address.postal_code")}
            autocomplete="postal-code"
            required
            value={form().postal_code}
            onChange={(v) => set("postal_code", v)}
            maxLength={20}
          />
          <TextField
            label={t(m, "address.city")}
            autocomplete="address-level2"
            required
            value={form().city}
            onChange={(v) => set("city", v)}
            maxLength={100}
          />
          <SelectField
            label={t(m, "address.country")}
            value={form().country}
            options={countryOptions()}
            onChange={(v) => set("country", v)}
          />
          <TextField
            label={t(m, "address.phone")}
            type="tel"
            autocomplete="tel"
            value={form().phone ?? ""}
            onChange={(v) => set("phone", v || null)}
            maxLength={40}
          />
          <Checkbox
            class="sm:col-span-2"
            label={t(m, "address.make_default")}
            checked={form().is_default ?? false}
            onChange={(v) => set("is_default", v)}
          />
          <Show when={error()}>
            <p role="alert" class="text-sm text-sale sm:col-span-2">
              {error()}
            </p>
          </Show>
          <div class="flex gap-2 sm:col-span-2">
            <Button type="submit" variant="confirm" loading={busy()}>
              {t(m, "address.save")}
            </Button>
            <Button category="tertiary" onClick={() => setEditing(undefined)}>
              {t(m, "address.cancel")}
            </Button>
          </div>
        </form>
      </Show>
    </HydratedControls>
  );
}
