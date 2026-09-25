import { suggest } from "@platform/storefront-sdk/client";
import { imageUrl, type Messages, t } from "@platform/storefront-sdk/format";
import type { SearchSuggest } from "@platform/storefront-sdk/types";
import { createSignal, For, onCleanup, Show } from "solid-js";

/**
 * Search typeahead (ARIA combobox). Without JS it is a plain GET form to /search, so the
 * server-rendered markup is fully functional before hydration.
 */
export default function SearchBox(props: { q?: string; labels: Messages }) {
  const [q, setQ] = createSignal(props.q ?? "");
  const [result, setResult] = createSignal<SearchSuggest | null>(null);
  const [active, setActive] = createSignal(-1);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let ctrl: AbortController | undefined;
  onCleanup(() => clearTimeout(timer));

  const items = () => result()?.products ?? [];
  const expanded = () => items().length > 0;

  function onInput(value: string) {
    setQ(value);
    clearTimeout(timer);
    if (value.trim().length < 2) return setResult(null);
    timer = setTimeout(async () => {
      ctrl?.abort();
      ctrl = new AbortController();
      try {
        setResult(await suggest(value, ctrl.signal));
        setActive(-1);
      } catch {
        /* aborted or offline: keep the plain form */
      }
    }, 150);
  }

  function onKeyDown(e: KeyboardEvent) {
    const n = items().length;
    if (!n) return;
    if (e.key === "ArrowDown") setActive((active() + 1) % n);
    else if (e.key === "ArrowUp") setActive((active() - 1 + n) % n);
    else if (e.key === "Escape") return setResult(null);
    else if (e.key === "Enter" && active() >= 0) {
      e.preventDefault();
      location.href = `/p/${items()[active()]?.slug}`;
      return;
    } else return;
    e.preventDefault();
  }

  return (
    <search class="relative block w-full">
      <form action="/search" method="get">
        <label for="q" class="sr-only">
          {t(props.labels, "search.label")}
        </label>
        <input
          id="q"
          name="q"
          type="search"
          autocomplete="off"
          placeholder={t(props.labels, "search.placeholder")}
          value={q()}
          onInput={(e) => onInput(e.currentTarget.value)}
          onKeyDown={onKeyDown}
          onBlur={() => setTimeout(() => setResult(null), 150)}
          role="combobox"
          aria-expanded={expanded()}
          aria-controls="q-list"
          aria-autocomplete="list"
          aria-activedescendant={active() >= 0 ? `q-opt-${active()}` : undefined}
          class="h-10 w-full rounded-md border border-border bg-card px-3 text-[15px] placeholder:text-subtle"
        />
        <Show when={expanded()}>
          <div
            id="q-list"
            role="listbox"
            class="absolute inset-x-0 top-11 z-20 overflow-hidden rounded-md border border-border bg-card shadow-lg"
          >
            <For each={items()}>
              {(p, i) => (
                // biome-ignore lint/a11y/useFocusableInteractive: ARIA APG combobox, focus stays in the input (aria-activedescendant)
                <div
                  id={`q-opt-${i()}`}
                  role="option"
                  aria-selected={active() === i()}
                  class="aria-selected:bg-identity-wash"
                >
                  <a href={`/p/${p.slug}`} class="flex items-center gap-3 px-3 py-2 text-sm">
                    <img
                      src={p.image ? imageUrl(p.image, 120) : undefined}
                      alt=""
                      width="32"
                      height="40"
                      class="rounded-sm bg-muted"
                    />
                    <span class="flex-1">{p.name}</span>
                    <span class="price">{p.price.formatted}</span>
                  </a>
                </div>
              )}
            </For>
          </div>
        </Show>
      </form>
    </search>
  );
}
