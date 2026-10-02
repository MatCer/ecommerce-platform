import { hitThumb, suggest } from "@platform/storefront-sdk/client";
import { type Messages, t } from "@platform/storefront-sdk/format";
import type { SearchSuggest } from "@platform/storefront-sdk/types";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import Icon from "../lib/Icon";
import { search as searchIcon } from "../lib/icons";

type Option = { id: string; href: string; label: string; kind: "category" | "product" | "all" };

/**
 * Search typeahead, ARIA 1.2 combobox with a listbox popup (focus stays in the input,
 * `aria-activedescendant` marks the option). Server-rendered as a plain GET form to /search,
 * so it works before hydration and without JS. Suggestions come from `/_p/public/search/suggest`.
 */
export default function SearchBox(props: { q?: string; base: string; labels: Messages }) {
  const l = (key: string, args?: Record<string, string>) => t(props.labels, key, args);
  // Read before hydration applies `value`: the server-rendered input may already hold text.
  const typed =
    typeof document === "undefined"
      ? ""
      : ((document.getElementById("sb-q") as HTMLInputElement | null)?.value ?? "");
  const [q, setQ] = createSignal(typed || props.q || "");
  const [result, setResult] = createSignal<SearchSuggest | null>(null);
  const [active, setActive] = createSignal(-1);
  const [open, setOpen] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctrl: AbortController | undefined;
  let root: HTMLElement | undefined;
  onCleanup(() => clearTimeout(timer));
  // Text typed before hydration (the island loads on idle) is picked up, not overwritten.
  onMount(() => {
    if (typed && typed !== props.q) onInput(typed);
  });

  const searchHref = (query: string) => `${props.base}/search?${new URLSearchParams({ q: query })}`;
  const options = createMemo<Option[]>(() => {
    const r = result();
    if (!r) return [];
    return [
      ...r.categories.map((c) => ({
        id: `c-${c.id}`,
        href: `${props.base}/c/${c.slug}`,
        label: c.name,
        kind: "category" as const,
      })),
      ...r.products.map((p) => ({
        id: `p-${p.variant_id}`,
        href: `${props.base}/p/${p.slug}`,
        label: p.name,
        kind: "product" as const,
      })),
      {
        id: "all",
        href: searchHref(q().trim()),
        label: l("search.all_results", { q: q().trim() }),
        kind: "all" as const,
      },
    ];
  });
  const expanded = () => open() && options().length > 1;
  const optionId = (i: number) => `sb-opt-${i}`;
  const hit = (id: string) => result()?.products.find((p) => `p-${p.variant_id}` === id);

  function onInput(value: string) {
    setQ(value);
    clearTimeout(timer);
    if (value.trim().length < 2) {
      setResult(null);
      return;
    }
    timer = setTimeout(async () => {
      ctrl?.abort();
      ctrl = new AbortController();
      try {
        setResult(await suggest(value.trim(), { base: props.base, signal: ctrl.signal }));
        setActive(-1);
        setOpen(true);
      } catch {
        /* aborted or offline: the plain form still works */
      }
    }, 150);
  }

  function onKeyDown(e: KeyboardEvent) {
    const n = options().length;
    if (e.key === "Escape") {
      if (expanded()) setOpen(false);
      else setQ("");
      return;
    }
    if (!expanded()) {
      if (e.key === "ArrowDown" && n > 1) {
        setOpen(true);
        setActive(0);
        e.preventDefault();
      }
      return;
    }
    if (e.key === "ArrowDown") setActive((active() + 1) % n);
    else if (e.key === "ArrowUp") setActive(active() <= 0 ? n - 1 : active() - 1);
    else if (e.key === "Enter" && active() >= 0) {
      const o = options()[active()];
      if (o) location.assign(o.href);
    } else return;
    e.preventDefault();
  }

  return (
    <search
      ref={root}
      class="relative block w-full"
      onFocusOut={(e) => {
        if (!root?.contains(e.relatedTarget as Node | null)) setOpen(false);
      }}
    >
      <form action={`${props.base}/search`} method="get" class="relative">
        <label for="sb-q" class="sr-only">
          {l("search.label")}
        </label>
        <input
          id="sb-q"
          name="q"
          type="search"
          autocomplete="off"
          enterkeyhint="search"
          placeholder={l("search.placeholder")}
          value={q()}
          onInput={(e) => onInput(e.currentTarget.value)}
          onFocus={() => result() && setOpen(true)}
          onKeyDown={onKeyDown}
          role="combobox"
          aria-expanded={expanded()}
          aria-controls="sb-list"
          aria-autocomplete="list"
          aria-activedescendant={expanded() && active() >= 0 ? optionId(active()) : undefined}
          class="h-11 w-full rounded-full border border-transparent bg-card pr-12 pl-5 text-base text-foreground placeholder:text-subtle focus-visible:outline-card md:text-[0.9375rem]"
        />
        <button
          type="submit"
          class="absolute inset-y-0 right-0 grid w-12 place-items-center rounded-r-full text-muted-foreground hover:text-foreground"
        >
          <Icon d={searchIcon} />
          <span class="sr-only">{l("search.label")}</span>
        </button>
      </form>
      <div
        id="sb-list"
        role="listbox"
        aria-label={l("search.label")}
        hidden={!expanded()}
        class="absolute inset-x-0 top-12 z-40 max-h-[70dvh] overflow-y-auto rounded-lg border border-border bg-card py-1 text-foreground shadow-lift"
      >
        <For each={options()}>
          {(o, i) => (
            // biome-ignore lint/a11y/useKeyWithClickEvents lint/a11y/useFocusableInteractive: APG combobox, focus stays in the input (aria-activedescendant) and the keys are handled there
            <div
              id={optionId(i())}
              role="option"
              aria-selected={active() === i()}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => location.assign(o.href)}
              onMouseEnter={() => setActive(i())}
              class="flex min-h-11 cursor-pointer items-center gap-3 px-3 py-1.5 text-sm aria-selected:bg-identity-wash"
              classList={{
                "border-t border-border font-semibold text-identity-ink": o.kind === "all",
              }}
            >
              <Show when={hit(o.id)}>
                {(h) => (
                  <img
                    src={hitThumb(h(), 80)}
                    alt=""
                    width="32"
                    height="40"
                    class="h-10 w-8 rounded-sm bg-muted object-cover"
                  />
                )}
              </Show>
              <span class="flex-1">{o.label}</span>
              <Show when={hit(o.id)}>
                {(h) => <span class="price">{h().price.formatted}</span>}
              </Show>
              <Show when={o.kind === "category"}>
                <span class="text-xs text-muted-foreground">{l("nav.categories")}</span>
              </Show>
            </div>
          )}
        </For>
      </div>
    </search>
  );
}
