import { type Messages, t } from "@platform/storefront-sdk/format";
import type { ListingPage } from "@platform/storefront-sdk/types";
import { createSignal, For, onMount, Show } from "solid-js";

/**
 * Facet filters as progressive enhancement (spec §9.1): a plain GET form with checkboxes and a
 * submit button works without JS; once hydrated, changes apply immediately.
 */
export default function FacetFilters(props: {
  facets: ListingPage["facets"];
  sort: ListingPage["sort"];
  action: string;
  labels: Messages;
}) {
  const [enhanced, setEnhanced] = createSignal(false);
  let form: HTMLFormElement | undefined;
  onMount(() => setEnhanced(true));
  const submit = () => form?.requestSubmit();

  return (
    <form
      ref={form}
      method="get"
      action={props.action}
      class="flex flex-wrap items-end gap-x-6 gap-y-3"
      onChange={() => enhanced() && submit()}
    >
      <For each={props.facets}>
        {(f) => (
          <fieldset class="min-w-0">
            <legend class="mb-1.5 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
              {f.label}
            </legend>
            <div class="flex flex-wrap gap-1.5">
              <For each={f.values}>
                {(v) => (
                  <label class="cursor-pointer rounded-sm border border-border bg-card px-2.5 py-1 text-sm has-checked:border-identity has-checked:bg-identity-wash has-checked:text-identity-ink has-disabled:cursor-not-allowed has-disabled:text-subtle has-focus-visible:outline-2 has-focus-visible:outline-identity">
                    <input
                      class="sr-only"
                      type="checkbox"
                      name={f.key}
                      value={v.value}
                      checked={v.selected}
                      disabled={v.disabled && !v.selected}
                    />
                    {v.label}
                  </label>
                )}
              </For>
            </div>
          </fieldset>
        )}
      </For>
      <label class="ml-auto text-sm">
        <span class="mr-2 text-muted-foreground">{t(props.labels, "listing.sort")}</span>
        <select name="sort" class="h-9 rounded-md border border-border bg-card px-2">
          <For each={props.sort}>
            {(s) => (
              <option value={s.value} selected={s.selected}>
                {s.label}
              </option>
            )}
          </For>
        </select>
      </label>
      <Show when={!enhanced()}>
        <button
          type="submit"
          class="h-9 rounded-md border border-border bg-card px-4 text-sm font-semibold text-identity-ink"
        >
          {t(props.labels, "listing.apply")}
        </button>
      </Show>
    </form>
  );
}
