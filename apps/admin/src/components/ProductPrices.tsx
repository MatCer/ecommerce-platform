import {
  Alert,
  Badge,
  Button,
  Collapse,
  controlClass,
  FormGroup,
  linkClass,
  showToast,
  TextField,
} from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, Show } from "solid-js";
import { errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import {
  formatMoney,
  fromLocalInput,
  minorToInput,
  parseMoney,
  toLocalInput,
} from "../lib/money.ts";
import { usePriceLists } from "../lib/queries.ts";
import { QueryState, Th, tableClass, tdClass } from "./Page.tsx";

type PriceList = Schemas["PriceList"];
type History = Schemas["VariantPriceHistory"];

export interface PricedVariant {
  id: string;
  sku: string;
  label: string;
}

interface Row {
  price: string;
  compareAt: string;
}

function ListPrices(props: {
  productId: string;
  list: PriceList;
  variants: PricedVariant[];
  history: History[];
  at: string | null;
}) {
  const qc = useQueryClient();
  const entry = (variantId: string) =>
    props.history.find((h) => h.variant_id === variantId && h.price_list_id === props.list.id);
  const initial = (): Record<string, Row> =>
    Object.fromEntries(
      props.variants.map((v) => {
        const p = entry(v.id)?.price;
        return [
          v.id,
          { price: minorToInput(p?.amount_minor), compareAt: minorToInput(p?.compare_at_minor) },
        ];
      }),
    );
  const [rows, setRows] = createSignal<Record<string, Row>>(initial());
  const [error, setError] = createSignal<string>();
  const money = (minor: number) => formatMoney(minor, props.list.currency, locale());
  const refresh = () =>
    qc.invalidateQueries({ queryKey: tenantKey("price-history", props.productId) });

  const save = createMutation(() => ({
    mutationFn: () => {
      const items = props.variants.flatMap((v) => {
        const r = rows()[v.id];
        if (!r || r.price.trim() === "") return [];
        const amount = parseMoney(r.price);
        const compare = r.compareAt.trim() === "" ? null : parseMoney(r.compareAt);
        if (amount === null || (r.compareAt.trim() !== "" && compare === null)) {
          throw new Error("invalid_price");
        }
        const current = entry(v.id)?.price;
        const same =
          current?.amount_minor === amount && (current?.compare_at_minor ?? null) === compare;
        return same ? [] : [{ variant_id: v.id, amount_minor: amount, compare_at_minor: compare }];
      });
      return unwrap(
        api.PUT("/admin/v1/price-lists/{id}/prices", {
          params: { header: tenantHeader(), path: { id: props.list.id } },
          body: { items },
        }),
      );
    },
    onSuccess: async () => {
      setError(undefined);
      await refresh();
      showToast({ title: t("prices.saved"), closeLabel: t("common.close") });
    },
    onError: (err) =>
      setError(
        err instanceof Error && err.message === "invalid_price"
          ? t("errors.invalid_price")
          : errorMessage(err),
      ),
  }));

  const stop = createMutation(() => ({
    mutationFn: (variantId: string) =>
      unwrap(
        api.DELETE("/admin/v1/price-lists/{id}/prices/{variant_id}", {
          params: { header: tenantHeader(), path: { id: props.list.id, variant_id: variantId } },
        }),
      ),
    onSuccess: async (_, variantId) => {
      setRows({ ...rows(), [variantId]: { price: "", compareAt: "" } });
      await refresh();
    },
    onError: (err) =>
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") }),
  }));

  /** Only changed rows are sent (the API needs 1-1000 items). */
  const dirty = () => JSON.stringify(rows()) !== JSON.stringify(initial());
  const set = (id: string, patch: Partial<Row>) =>
    setRows({ ...rows(), [id]: { ...(rows()[id] ?? { price: "", compareAt: "" }), ...patch } });

  return (
    <section aria-labelledby={`pl-${props.list.id}`} class="flex flex-col gap-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <h3 id={`pl-${props.list.id}`} class="text-sm font-semibold text-heading">
          {props.list.name}{" "}
          <span class="font-mono text-xs font-normal text-muted-foreground">
            {props.list.currency}
          </span>
        </h3>
        <Button onClick={() => save.mutate()} loading={save.isPending} disabled={!dirty()}>
          {t("prices.save")}
          <span class="sr-only">: {props.list.name}</span>
        </Button>
      </div>
      <div class="overflow-x-auto rounded-md border border-border">
        <table class={tableClass}>
          <thead>
            <tr>
              <Th>{t("editor.variant")}</Th>
              <Th>{t("prices.price", { currency: props.list.currency })}</Th>
              <Th>{t("prices.compareAt", { currency: props.list.currency })}</Th>
              <Th class="text-right">{t("prices.current")}</Th>
              <Th class="text-right">{t("prices.omnibus")}</Th>
              <Th srOnly>{t("common.actions")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={props.variants}>
              {(v) => {
                const h = () => entry(v.id);
                return (
                  <tr>
                    <th scope="row" class={`${tdClass} text-left font-semibold text-heading`}>
                      {v.label}{" "}
                      <span class="block font-mono text-xs font-normal text-muted-foreground">
                        {v.sku}
                      </span>
                    </th>
                    <td class={`${tdClass} w-36 py-1`}>
                      <TextField
                        hideLabel
                        label={`${t("prices.price", { currency: props.list.currency })}: ${v.label}`}
                        value={rows()[v.id]?.price ?? ""}
                        onChange={(price) => set(v.id, { price })}
                        inputMode="decimal"
                        inputClass="figures text-right"
                      />
                    </td>
                    <td class={`${tdClass} w-36 py-1`}>
                      <TextField
                        hideLabel
                        label={`${t("prices.compareAt", { currency: props.list.currency })}: ${v.label}`}
                        value={rows()[v.id]?.compareAt ?? ""}
                        onChange={(compareAt) => set(v.id, { compareAt })}
                        inputMode="decimal"
                        inputClass="figures text-right"
                      />
                    </td>
                    <td class={`${tdClass} figures text-right`}>
                      <Show
                        when={h()?.omnibus.current_minor != null}
                        fallback={<span class="text-faint-foreground">{t("prices.notSold")}</span>}
                      >
                        {money(h()?.omnibus.current_minor ?? 0)}
                        <Show when={h()?.omnibus.on_sale}>
                          {" "}
                          {/* A percentage may only be advertised when the Omnibus claim holds. */}
                          <Badge tone="info">
                            {h()?.omnibus.claim && h()?.omnibus.discount_percent != null
                              ? t("prices.onSale", {
                                  percent: String(h()?.omnibus.discount_percent),
                                })
                              : t("prices.onSaleNoClaim")}
                          </Badge>
                        </Show>
                      </Show>
                    </td>
                    <td class={`${tdClass} figures text-right`}>
                      {h()?.omnibus.reference_minor != null
                        ? money(h()?.omnibus.reference_minor ?? 0)
                        : "—"}
                    </td>
                    <td class={`${tdClass} text-right`}>
                      <Show when={h()?.price}>
                        <Button
                          category="tertiary"
                          size="small"
                          loading={stop.isPending && stop.variables === v.id}
                          onClick={() => stop.mutate(v.id)}
                        >
                          {t("prices.stop")}
                          <span class="sr-only">: {v.label}</span>
                        </Button>
                      </Show>
                    </td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
      </div>
      <Show when={error()}>
        <Alert tone="error">{error()}</Alert>
      </Show>
      <div class="flex flex-col">
      <For each={props.variants}>
        {(v) => {
          const h = () => entry(v.id);
          return (
            <Collapse
              summary={
                <span>
                  {`${t("prices.history")}: ${v.label}`}{" "}
                  <span class="sr-only">({props.list.name})</span>
                </span>
              }
            >
              <Show
                when={(h()?.intervals.length ?? 0) > 0}
                fallback={<p class="pb-2 text-sm text-muted-foreground">{t("prices.noHistory")}</p>}
              >
                <div class="mb-2 max-w-2xl overflow-x-auto rounded-md border border-border">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("prices.from")}</Th>
                      <Th>{t("prices.to")}</Th>
                      <Th class="text-right">{t("prices.amount")}</Th>
                      <Th>{t("prices.cause")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={h()?.intervals ?? []}>
                      {(i) => (
                        <tr>
                          <td class={`${tdClass} figures text-xs`}>
                            {formatDateTime(i.valid_from)}
                          </td>
                          <td class={`${tdClass} figures text-xs`}>
                            {i.valid_to ? formatDateTime(i.valid_to) : t("prices.now")}
                          </td>
                          <td class={`${tdClass} figures text-right`}>{money(i.amount_minor)}</td>
                          <td class={tdClass}>{t(`prices.cause_${i.cause}`)}</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
                </div>
              </Show>
            </Collapse>
          );
        }}
      </For>
      </div>
      <Show when={props.at}>
        <p class="text-xs text-muted-foreground">
          {t("prices.asOf")}: {formatDateTime(props.at ?? "")}
        </p>
      </Show>
    </section>
  );
}

/** Per-variant gross prices per price list, current Omnibus figures and the price timeline. */
export function ProductPrices(props: { productId: string; variants: PricedVariant[] }) {
  const lists = usePriceLists();
  const [asOf, setAsOf] = createSignal("");
  const asOfId = createUniqueId();
  const history = createQuery(() => ({
    queryKey: tenantKey("price-history", props.productId, asOf()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/products/{id}/price-history", {
          params: {
            header: tenantHeader(),
            path: { id: props.productId },
            query: { at: fromLocalInput(asOf()) ?? undefined },
          },
        }),
      ),
  }));

  return (
    <QueryState query={lists}>
      {(data) => (
        <Show
          when={data.items.length > 0}
          fallback={
            <p class="text-sm text-muted-foreground">
              {t("prices.noLists")}{" "}
              <A href="/price-lists" class={linkClass}>
                {t("nav.priceLists")}
              </A>
            </p>
          }
        >
          <div class="flex flex-col gap-6">
            <FormGroup class="w-64" label={t("prices.asOf")} for={asOfId}>
              <input
                id={asOfId}
                type="datetime-local"
                class={`${controlClass} figures`}
                value={asOf()}
                max={toLocalInput(new Date(Date.now() + 365 * 86_400_000).toISOString())}
                onChange={(e) => setAsOf(e.currentTarget.value)}
              />
            </FormGroup>
            <QueryState query={history}>
              {(h) => (
                <For each={data.items}>
                  {(list) => (
                    <ListPrices
                      productId={props.productId}
                      list={list}
                      variants={props.variants}
                      history={h.items}
                      at={fromLocalInput(asOf())}
                    />
                  )}
                </For>
              )}
            </QueryState>
          </div>
        </Show>
      )}
    </QueryState>
  );
}
