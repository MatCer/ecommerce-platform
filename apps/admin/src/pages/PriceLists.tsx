import {
  Alert,
  Button,
  Card,
  Checkbox,
  Dialog,
  EmptyState,
  labelClass,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { CURRENCIES, type Currency } from "../lib/money.ts";
import { useMarkets, usePriceLists } from "../lib/queries.ts";

type PriceList = Schemas["PriceList"];

export default function PriceLists() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const lists = usePriceLists();
  const markets = useMarkets();
  const [editing, setEditing] = createSignal<PriceList | "new" | null>(null);
  const [name, setName] = createSignal("");
  const [code, setCode] = createSignal("");
  const [currency, setCurrency] = createSignal<Currency>("CZK");
  const [marketIds, setMarketIds] = createSignal<string[]>([]);
  const [error, setError] = createSignal<string>();

  const open = (l: PriceList | "new") => {
    setName(l === "new" ? "" : l.name);
    setCode(l === "new" ? "" : l.code);
    setCurrency(l === "new" ? "CZK" : l.currency);
    setMarketIds(l === "new" ? [] : l.market_ids);
    setError(undefined);
    setEditing(l);
  };
  const sameCurrency = () => (markets.data?.items ?? []).filter((m) => m.currency === currency());
  const marketName = (id: string) => markets.data?.items.find((m) => m.id === id)?.name ?? id;

  const create = submission();
  const save = createMutation(() => ({
    mutationFn: (current: PriceList | "new") => {
      if (current === "new") {
        const body = {
          name: name().trim(),
          code: code().trim(),
          currency: currency(),
          market_ids: marketIds(),
        };
        return unwrap(
          api.POST("/admin/v1/price-lists", {
            params: { header: create.header(body) },
            body,
          }),
        );
      }
      return unwrap(
        api.PUT("/admin/v1/price-lists/{id}", {
          params: { header: tenantHeader(), path: { id: current.id } },
          body: { name: name().trim(), market_ids: marketIds() },
        }),
      );
    },
    onSuccess: async (_, current) => {
      const created = current === "new";
      if (created) create.done();
      if (editing() === current) setEditing(null);
      await qc.invalidateQueries({ queryKey: tenantKey("price-lists") });
      await qc.invalidateQueries({ queryKey: tenantKey("markets") });
      showToast({
        title: created ? t("common.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
    },
    onError: (err, current) => {
      if (editing() === current) setError(errorMessage(err));
    },
  }));

  const formId = createUniqueId();
  const newButton = () => (
    <Button variant="confirm" disabled={!can("admin")} onClick={() => open("new")}>
      {t("priceLists.new")}
    </Button>
  );

  return (
    <>
      <PageHeader
        title={t("priceLists.title")}
        description={can("admin") ? undefined : t("priceLists.adminOnly")}
        actions={newButton()}
      />
      <QueryState query={lists}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <EmptyState
                icon="tag"
                title={t("priceLists.emptyTitle")}
                description={t("priceLists.emptyDesc")}
                action={can("admin") ? newButton() : undefined}
              />
            }
          >
            <Card padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("priceLists.name")}</Th>
                      <Th>{t("priceLists.code")}</Th>
                      <Th>{t("priceLists.currency")}</Th>
                      <Th>{t("priceLists.markets")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(l) => (
                        <tr class="hover:bg-subtle">
                          <td class={`${tdClass} font-semibold text-heading`}>{l.name}</td>
                          <td class={`${tdClass} font-mono text-xs text-muted-foreground`}>
                            {l.code}
                          </td>
                          <td class={`${tdClass} font-mono text-xs`}>{l.currency}</td>
                          <td class={tdClass}>{l.market_ids.map(marketName).join(", ") || "—"}</td>
                          <td class={`${tdClass} text-right`}>
                            <Button
                              category="tertiary"
                              size="small"
                              disabled={!can("admin")}
                              onClick={() => open(l)}
                            >
                              {t("common.edit")}
                              <span class="sr-only">: {l.name}</span>
                            </Button>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Card>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={editing() !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        title={editing() === "new" ? t("priceLists.new") : t("priceLists.editTitle")}
        footer={
          <>
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              form={formId}
              variant="confirm"
              loading={save.isPending}
              disabled={!name().trim()}
            >
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </>
        }
      >
        <form
          id={formId}
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            const current = editing();
            if (current) save.mutate(current);
          }}
        >
          <TextField
            label={t("priceLists.name")}
            value={name()}
            onChange={setName}
            required
            maxLength={200}
          />
          <div class="grid grid-cols-2 gap-4">
            <TextField
              label={t("priceLists.code")}
              description={t("priceLists.codeHint")}
              value={code()}
              onChange={setCode}
              inputClass="figures"
              required
              maxLength={64}
              disabled={editing() !== "new"}
            />
            <SelectField
              label={t("priceLists.currency")}
              value={currency()}
              options={CURRENCIES.map((c) => ({ value: c, label: c }))}
              onChange={(c) => {
                setCurrency(c as Currency);
                setMarketIds([]);
              }}
              disabled={editing() !== "new"}
            />
          </div>
          <fieldset class="flex flex-col gap-2">
            <legend class={`${labelClass} mb-1`}>{t("priceLists.markets")}</legend>
            <p class="text-sm text-muted-foreground">{t("priceLists.marketsHint")}</p>
            <Show when={markets.isError}>
              <Alert tone="error">{errorMessage(markets.error)}</Alert>
            </Show>
            <For each={sameCurrency()}>
              {(m) => (
                <Checkbox
                  label={`${m.name} (${m.code})`}
                  checked={marketIds().includes(m.id)}
                  onChange={(on) =>
                    setMarketIds(
                      on ? [...marketIds(), m.id] : marketIds().filter((x) => x !== m.id),
                    )
                  }
                />
              )}
            </For>
          </fieldset>
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
        </form>
      </Dialog>
    </>
  );
}
