import { Alert, EmptyState, SelectField, TextField } from "@platform/ui";
import { createSignal, For, type JSX, Show } from "solid-js";
import { errorMessage, LOCALES, t } from "../i18n/index.ts";
import { ApiError, type Schemas } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";
import { QueryState } from "./Page.tsx";

export function MarketSettings(props: { children: (market: Schemas["Market"]) => JSX.Element }) {
  const markets = useMarkets();
  const [selected, setSelected] = createSignal("");
  const market = () =>
    markets.data?.items.find((m) => m.id === selected()) ?? markets.data?.items[0];
  return (
    <QueryState query={markets}>
      {() => (
        <Show
          when={market()}
          fallback={
            <EmptyState
              icon="earth"
              title={t("checkout.noMarkets")}
              description={t("checkout.noMarketsDesc")}
            />
          }
        >
          <div class="mb-4 max-w-sm">
            <SelectField
              label={t("checkout.market")}
              value={market()?.id ?? ""}
              options={(markets.data?.items ?? []).map((m) => ({
                value: m.id,
                label: `${m.name} (${m.currency})`,
              }))}
              onChange={setSelected}
            />
          </div>
          <Show keyed when={market() && JSON.stringify(tenantKey("checkout", market()?.id))}>
            {(_key) => props.children(market() as Schemas["Market"])}
          </Show>
        </Show>
      )}
    </QueryState>
  );
}

export function TranslationFields(props: {
  label: string;
  values: Record<string, string>;
  onChange: (values: Record<string, string>) => void;
}) {
  return (
    <div class="grid gap-4 sm:grid-cols-3">
      <For each={LOCALES}>
        {(l) => (
          <TextField
            label={`${props.label} (${t(`common.locale_${l}`)})`}
            value={props.values[l] ?? ""}
            onChange={(value) => props.onChange({ ...props.values, [l]: value })}
          />
        )}
      </For>
    </div>
  );
}

export function ApiProblem(props: { error: unknown }) {
  return (
    <Show when={Boolean(props.error)}>
      <Alert tone="error">
        {props.error instanceof ApiError && props.error.detail
          ? props.error.detail
          : errorMessage(props.error)}
      </Alert>
    </Show>
  );
}
