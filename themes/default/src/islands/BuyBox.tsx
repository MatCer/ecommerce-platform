import type { ProductPage, Variant } from "@platform/storefront-sdk/types";
import { createMemo, createSignal, For, Show } from "solid-js";
import { addToCart, setOpen } from "../lib/cart-store";

type Props = {
  options: ProductPage["product"]["options"];
  variants: Variant[];
};

const STOCK = {
  in_stock: { label: "Skladem", cls: "text-stock-in" },
  low_stock: { label: "Poslední kusy", cls: "text-stock-low" },
  out_of_stock: { label: "Vyprodáno", cls: "text-subtle" },
} as const;

/** Variant picker + price + add to cart (spec §9.1 islands; sticky on mobile, §9.2). */
export default function BuyBox(props: Props) {
  const first = props.variants.find((v) => v.stock !== "out_of_stock") ?? props.variants[0];
  const [selected, setSelected] = createSignal<Record<string, string>>({
    ...(first?.options ?? {}),
  });
  const [state, setState] = createSignal<"idle" | "busy" | "added" | "error">("idle");

  const variant = createMemo(() =>
    props.variants.find((v) =>
      Object.entries(selected()).every(([k, val]) => v.options[k] === val),
    ),
  );
  const available = (name: string, value: string) =>
    props.variants.some(
      (v) =>
        v.stock !== "out_of_stock" &&
        v.options[name] === value &&
        Object.entries(selected()).every(([k, val]) => k === name || v.options[k] === val),
    );

  async function add() {
    const v = variant();
    if (!v || v.stock === "out_of_stock") return;
    setState("busy");
    try {
      await addToCart(v.id);
      setState("added");
      setOpen(true);
      setTimeout(() => setState("idle"), 1800);
    } catch {
      setState("error");
    }
  }

  return (
    <div class="flex flex-col gap-5">
      <div>
        <p class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <span class={`price text-3xl ${variant()?.reference_price ? "text-sale" : ""}`}>
            {variant()?.price.formatted}
          </span>
          <Show when={variant()?.reference_price}>
            {(ref) => (
              <span class="text-sm text-muted-foreground">
                <span class="sr-only">Původní cena </span>
                <s class="price text-base font-semibold text-subtle">{ref().formatted}</s>
                <span class="ml-2 rounded-sm bg-sale-wash px-1.5 py-0.5 text-xs font-bold text-sale">
                  −{variant()?.discount_percent} %
                </span>
              </span>
            )}
          </Show>
        </p>
        <Show when={variant()?.reference_price}>
          {(ref) => (
            <p class="mt-1 text-xs text-muted-foreground">
              Nejnižší cena za 30 dní před slevou: {ref().formatted}
            </p>
          )}
        </Show>
        <p class="mt-1 text-xs text-muted-foreground">Cena včetně DPH</p>
      </div>

      <For each={props.options}>
        {(opt) => (
          <fieldset>
            <legend class="mb-2 text-sm font-semibold">
              {opt.name}:{" "}
              <span class="font-normal text-muted-foreground">{selected()[opt.name]}</span>
            </legend>
            <div class="flex flex-wrap gap-2">
              <For each={opt.values}>
                {(value) => (
                  <label
                    class="relative cursor-pointer rounded-md border border-border bg-card px-3 py-2 text-sm font-medium has-checked:border-identity has-checked:bg-identity-wash has-checked:text-identity-ink has-focus-visible:outline-2 has-focus-visible:outline-identity"
                    classList={{
                      "text-subtle line-through decoration-1": !available(opt.name, value),
                    }}
                  >
                    <input
                      class="sr-only"
                      type="radio"
                      name={opt.name}
                      value={value}
                      checked={selected()[opt.name] === value}
                      onChange={() => setSelected({ ...selected(), [opt.name]: value })}
                    />
                    {value}
                    <Show when={!available(opt.name, value)}>
                      <span class="sr-only"> (nedostupné)</span>
                    </Show>
                  </label>
                )}
              </For>
            </div>
          </fieldset>
        )}
      </For>

      <p
        class={`flex items-center gap-2 text-sm font-semibold ${STOCK[variant()?.stock ?? "out_of_stock"].cls}`}
      >
        <span aria-hidden="true" class="size-2 rounded-full bg-current" />
        {STOCK[variant()?.stock ?? "out_of_stock"].label}
      </p>

      <div class="sticky bottom-0 -mx-4 border-t border-border bg-background/95 px-4 py-3 md:static md:m-0 md:border-0 md:bg-transparent md:p-0">
        <button
          type="button"
          onClick={add}
          disabled={!variant() || variant()?.stock === "out_of_stock" || state() === "busy"}
          class="h-12 w-full rounded-md bg-buy px-6 font-display text-base font-bold text-foreground transition-[background-color,transform] duration-200 hover:bg-buy-hover active:scale-[0.98] disabled:cursor-not-allowed disabled:bg-muted disabled:text-subtle"
          classList={{ "bg-stock-in! text-card!": state() === "added" }}
          aria-live="polite"
        >
          {state() === "added"
            ? "✓ Přidáno do košíku"
            : state() === "busy"
              ? "Přidávám…"
              : variant()?.stock === "out_of_stock"
                ? "Vyprodáno"
                : "Přidat do košíku"}
        </button>
        <Show when={state() === "error"}>
          <p role="alert" class="mt-2 text-sm text-sale">
            Nepodařilo se přidat do košíku. Zkuste to prosím znovu.
          </p>
        </Show>
      </div>
    </div>
  );
}
