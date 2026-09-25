import { type Messages, t } from "@platform/storefront-sdk/format";
import type { ProductOption, StockState, Variant } from "@platform/storefront-sdk/types";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { addToCart, openCart } from "../lib/cart-store";
import Icon from "../lib/Icon";
import { check } from "../lib/icons";
import { setImageIndex } from "../lib/product-store";

type Props = {
  options: ProductOption[];
  variants: Variant[];
  labels: Messages;
};

const STOCK_CLASS: Record<StockState, string> = {
  in_stock: "text-stock-in",
  low_stock: "text-stock-low",
  backorder: "text-stock-low",
  out_of_stock: "text-subtle",
};

/**
 * Price, variant picker and add to cart (spec §9.1/§9.2). Server-rendered with the default
 * variant, so price and stock are in the HTML before hydration. Omnibus (A18): strikethrough,
 * discount and the 30-day lowest price appear only when the variant carries `reference_price`.
 * On phones a bottom bar repeats price + button once the main button scrolls out of view.
 */
export default function BuyBox(props: Props) {
  const l = (key: string, args?: Record<string, string | number>) => t(props.labels, key, args);
  const first =
    props.variants.find((v) => v.is_default && v.stock !== "out_of_stock") ??
    props.variants.find((v) => v.stock !== "out_of_stock") ??
    props.variants[0];
  const [selected, setSelected] = createSignal<Record<string, string>>({
    ...(first?.options ?? {}),
  });
  const [state, setState] = createSignal<"idle" | "busy" | "added" | "error">("idle");
  const [barVisible, setBarVisible] = createSignal(false);
  let mainButton: HTMLButtonElement | undefined;

  const variant = createMemo(() =>
    props.variants.find((v) =>
      Object.entries(selected()).every(([k, val]) => v.options[k] === val),
    ),
  );
  const available = (code: string, value: string) =>
    props.variants.some(
      (v) =>
        v.stock !== "out_of_stock" &&
        v.options[code] === value &&
        Object.entries(selected()).every(([k, val]) => k === code || v.options[k] === val),
    );
  const valueName = (opt: ProductOption) =>
    opt.values.find((v) => v.code === selected()[opt.code])?.name ?? "";
  const stock = (): StockState => variant()?.stock ?? "out_of_stock";
  const canBuy = () => Boolean(variant()) && stock() !== "out_of_stock" && state() !== "busy";
  const reduced = () => {
    const v = variant();
    return v?.reference_price && v.discount_percent ? v : null;
  };

  function choose(code: string, value: string) {
    setSelected({ ...selected(), [code]: value });
    const index = variant()?.image_index;
    if (index !== null && index !== undefined) setImageIndex(index);
  }

  let reset: ReturnType<typeof setTimeout> | undefined;
  async function add() {
    const v = variant();
    if (!v || !canBuy()) return;
    setState("busy");
    try {
      await addToCart(v.id);
      setState("added");
      openCart();
      reset = setTimeout(() => setState("idle"), 2000);
    } catch {
      setState("error");
    }
  }

  onMount(() => {
    if (!mainButton) return;
    const io = new IntersectionObserver(([e]) => setBarVisible(!(e?.isIntersecting ?? true)));
    io.observe(mainButton);
    onCleanup(() => io.disconnect());
  });
  onCleanup(() => clearTimeout(reset));

  const buttonLabel = () =>
    state() === "added"
      ? l("cart.added")
      : state() === "busy"
        ? l("cart.adding")
        : !variant()
          ? l("product.unavailable")
          : stock() === "out_of_stock"
            ? l("product.out_of_stock")
            : l("cart.add");

  const Button = (p: { ref?: (el: HTMLButtonElement) => void; compact?: boolean }) => (
    <button
      ref={p.ref}
      type="button"
      onClick={add}
      disabled={!canBuy()}
      class="btn btn-buy w-full"
      classList={{
        "h-14 text-lg": !p.compact,
        "h-12": p.compact,
        "bg-stock-in! text-card!": state() === "added",
      }}
    >
      <Show when={state() === "added"}>
        <Icon d={check} class="size-5" />
      </Show>
      {buttonLabel()}
    </button>
  );

  const Price = (p: { compact?: boolean }) => (
    <p class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
      <Show when={reduced()}>
        <span class="sr-only">{l("price.current")}:</span>
      </Show>
      <span
        class="price"
        classList={{
          "text-sale": Boolean(reduced()),
          "text-3xl": !p.compact,
          "text-xl": p.compact,
        }}
      >
        {variant()?.price.formatted ?? first?.price.formatted}
      </span>
      <Show when={reduced()}>
        {(v) => (
          <>
            <s class="price text-base font-semibold text-subtle">
              <span class="sr-only">{l("price.original")}: </span>
              {v().reference_price?.formatted}
            </s>
            <Show when={!p.compact}>
              <span class="rounded-sm bg-sale px-1.5 py-0.5 text-xs font-bold text-card">
                <span aria-hidden="true">−{v().discount_percent} %</span>
                <span class="sr-only">
                  {l("price.discount", { percent: v().discount_percent ?? 0 })}
                </span>
              </span>
            </Show>
          </>
        )}
      </Show>
    </p>
  );

  return (
    <div class="flex flex-col gap-5">
      <div>
        <Price />
        <Show when={reduced()}>
          {(v) => (
            <p class="mt-1.5 text-sm text-muted-foreground">
              {l("price.lowest_30_days", { price: v().reference_price?.formatted ?? "" })}
            </p>
          )}
        </Show>
        <Show when={variant()?.unit_price}>
          {(u) => (
            <p class="mt-1 text-sm text-muted-foreground">
              {l("price.unit", { price: u().price.formatted, unit: u().unit })}
            </p>
          )}
        </Show>
        <p class="mt-1 text-xs text-muted-foreground">{l("product.vat_included")}</p>
      </div>

      <For each={props.options}>
        {(opt) => (
          <fieldset>
            <legend class="mb-2 text-sm font-semibold">
              {opt.name}: <span class="font-normal text-muted-foreground">{valueName(opt)}</span>
            </legend>
            <div class="flex flex-wrap gap-2">
              <For each={opt.values}>
                {(value) => (
                  <label
                    class="chip min-w-12 justify-center"
                    classList={{
                      "text-subtle line-through decoration-1": !available(opt.code, value.code),
                    }}
                  >
                    <input
                      class="sr-only"
                      type="radio"
                      name={`opt-${opt.code}`}
                      value={value.code}
                      checked={selected()[opt.code] === value.code}
                      onChange={() => choose(opt.code, value.code)}
                    />
                    {value.name}
                    <Show when={!available(opt.code, value.code)}>
                      <span class="sr-only"> ({l("product.unavailable")})</span>
                    </Show>
                  </label>
                )}
              </For>
            </div>
          </fieldset>
        )}
      </For>

      <div aria-live="polite" class="flex flex-col gap-1">
        <Show
          when={variant()}
          fallback={
            <p class="text-sm font-semibold text-subtle">{l("product.combination_unavailable")}</p>
          }
        >
          {(v) => (
            <>
              <p class={`flex items-center gap-2 text-sm font-semibold ${STOCK_CLASS[stock()]}`}>
                <span aria-hidden="true" class="size-2 rounded-full bg-current" />
                {l(`product.${stock()}`)}
              </p>
              <p class="text-xs text-muted-foreground">{l("product.sku", { sku: v().sku })}</p>
            </>
          )}
        </Show>
      </div>

      <div>
        <Button ref={(el) => (mainButton = el)} />
        <Show when={state() === "error"}>
          <p role="alert" class="mt-2 text-sm font-medium text-sale">
            {l("cart.add_failed")}
          </p>
        </Show>
      </div>

      {/* Phones: the buy action stays in reach once the main button scrolls away. */}
      <div
        class="fixed inset-x-0 bottom-0 z-20 flex items-center gap-3 border-t border-border bg-card/95 px-4 py-3 shadow-sheet backdrop-blur transition-transform duration-200 md:hidden"
        classList={{ "translate-y-full": !barVisible() }}
        inert={barVisible() ? undefined : true}
      >
        <div class="min-w-0 flex-1">
          <Price compact />
        </div>
        <div class="w-1/2">
          <Button compact />
        </div>
      </div>
    </div>
  );
}
