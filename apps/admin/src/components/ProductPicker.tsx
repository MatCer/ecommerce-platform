import { Checkbox, TextField } from "@platform/ui";
import { createQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { contentLocales, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

function pick(name: Record<string, string>): string {
  for (const l of contentLocales()) if (name[l]) return name[l];
  return Object.values(name)[0] ?? t("products.untitled");
}

/** Search-and-tick product selection (ids), for sale targets. */
export function ProductPicker(props: { value: string[]; onChange: (ids: string[]) => void }) {
  const [q, setQ] = createSignal("");
  // Names of ticked products, so they stay listed when the search changes.
  const [names, setNames] = createSignal<Record<string, string>>({});
  const results = createQuery(() => ({
    queryKey: tenantKey("product-picker", q().trim()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/products", {
          params: { header: tenantHeader(), query: { q: q().trim() || undefined, limit: 20 } },
        }),
      ),
  }));
  const toggle = (id: string, name: string, on: boolean) => {
    setNames({ ...names(), [id]: name });
    props.onChange(on ? [...props.value, id] : props.value.filter((x) => x !== id));
  };
  const selectedOnly = () =>
    props.value.filter((id) => !results.data?.items.some((p) => p.id === id));

  return (
    <div class="flex flex-col gap-2">
      <TextField type="search" label={t("sales.productSearch")} value={q()} onChange={setQ} />
      <ul class="flex max-h-48 flex-col gap-1 overflow-y-auto">
        <For each={selectedOnly()}>
          {(id) => (
            <li>
              <Checkbox
                label={names()[id] ?? id}
                checked
                onChange={() => toggle(id, names()[id] ?? id, false)}
              />
            </li>
          )}
        </For>
        <For each={results.data?.items ?? []}>
          {(p) => (
            <li>
              <Checkbox
                label={pick(p.name)}
                checked={props.value.includes(p.id)}
                onChange={(on) => toggle(p.id, pick(p.name), on)}
              />
            </li>
          )}
        </For>
      </ul>
      <Show when={results.isPending}>
        <span class="text-xs text-muted-foreground">{t("common.loading")}</span>
      </Show>
    </div>
  );
}
