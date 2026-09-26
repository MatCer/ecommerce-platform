import {
  Button,
  Checkbox,
  Dialog,
  EmptyState,
  LoadingState,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { A, useSearchParams } from "@solidjs/router";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { claims } from "../lib/session.ts";

type Row = Schemas["LevelRow"];

/** Integer text ("-3", "+5", "12"); `null` otherwise. */
export function parseCount(text: string): number | null {
  const s = text.trim().replace(/^\+/, "");
  return /^-?\d+$/.test(s) && Number.isSafeInteger(Number(s)) ? Number(s) : null;
}

function Movements(props: { row: Row }) {
  const movements = createInfiniteQuery(() => ({
    queryKey: tenantKey("movements", props.row.variant_id),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/inventory/{variant_id}/movements", {
          params: {
            header: tenantHeader(),
            path: { variant_id: props.row.variant_id },
            query: { cursor: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const items = () => movements.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <QueryState query={movements}>
      {() => (
        <Show
          when={items().length > 0}
          fallback={<p class="text-sm text-muted-foreground">{t("inventory.noMovements")}</p>}
        >
          <div class="max-h-96 overflow-auto">
            <table class={tableClass}>
              <thead>
                <tr>
                  <Th>{t("inventory.when")}</Th>
                  <Th>{t("inventory.kind")}</Th>
                  <Th class="text-right">{t("inventory.quantity")}</Th>
                  <Th class="text-right">{t("inventory.after")}</Th>
                  <Th>{t("inventory.note")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={items()}>
                  {(m) => (
                    <tr>
                      <td class={`${tdClass} figures text-xs whitespace-nowrap`}>
                        {formatDateTime(m.created_at)}
                      </td>
                      <td class={tdClass}>{t(`inventory.kind_${m.kind}`)}</td>
                      <td class={`${tdClass} figures text-right`}>
                        {m.quantity > 0 ? `+${m.quantity}` : m.quantity}
                      </td>
                      <td class={`${tdClass} figures text-right`}>{m.on_hand_after}</td>
                      <td class={`${tdClass} text-xs`}>
                        {m.note ?? ""}
                        <span class="block text-faint-foreground">
                          {m.actor === claims()?.sub ? t("audit.you") : m.actor}
                        </span>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
          <Show when={movements.hasNextPage}>
            <Button
              class="mt-2"
              loading={movements.isFetchingNextPage}
              onClick={() => void movements.fetchNextPage()}
            >
              {t("common.loadMore")}
            </Button>
          </Show>
        </Show>
      )}
    </QueryState>
  );
}

export default function Inventory() {
  const qc = useQueryClient();
  const [params, setParams] = useSearchParams();
  const productId = () => (typeof params.product === "string" ? params.product : undefined);
  const [adjusting, setAdjusting] = createSignal<Row | null>(null);
  const [viewing, setViewing] = createSignal<Row | null>(null);
  const [mode, setMode] = createSignal<"delta" | "count">("delta");
  const [amount, setAmount] = createSignal("");
  const [note, setNote] = createSignal("");
  const [error, setError] = createSignal<string>();

  const levels = createInfiniteQuery(() => ({
    queryKey: tenantKey("inventory", productId()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/inventory", {
          params: {
            header: tenantHeader(),
            query: { product_id: productId(), cursor: pageParam, limit: 100 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => levels.data?.pages.flatMap((p) => p.items) ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("inventory") });

  const settings = createMutation(() => ({
    mutationFn: (v: { row: Row; track: boolean; allow_backorder: boolean }) =>
      unwrap(
        api.PUT("/admin/v1/inventory/{variant_id}", {
          params: { header: tenantHeader(), path: { variant_id: v.row.variant_id } },
          body: { track: v.track, allow_backorder: v.allow_backorder },
        }),
      ),
    onSuccess: async () => {
      await refresh();
      showToast({ title: t("inventory.settingsSaved"), closeLabel: t("common.close") });
    },
    onError: (err) =>
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") }),
  }));

  // Pressing Save again after a lost response reuses the Idempotency-Key (no double +10).
  const adjustment = submission();
  const adjust = createMutation(() => ({
    mutationFn: (row: Row) => {
      const n = parseCount(amount());
      if (n === null || (mode() === "count" && n < 0)) throw new Error("invalid_quantity");
      const body =
        mode() === "delta"
          ? { delta: n, note: note().trim() || null }
          : { on_hand: n, note: note().trim() || null };
      return unwrap(
        api.POST("/admin/v1/inventory/{variant_id}/adjustments", {
          params: {
            header: adjustment.header({ variant: row.variant_id, ...body }),
            path: { variant_id: row.variant_id },
          },
          body,
        }),
      );
    },
    onSuccess: async (_, row) => {
      adjustment.done();
      if (adjusting() === row) setAdjusting(null);
      await refresh();
      await qc.invalidateQueries({ queryKey: tenantKey("movements", row.variant_id) });
      showToast({ title: t("inventory.adjusted"), closeLabel: t("common.close") });
    },
    onError: (err, row) =>
      adjusting() === row &&
      setError(
        err instanceof Error && err.message === "invalid_quantity"
          ? t("errors.invalid_quantity")
          : errorMessage(err),
      ),
  }));

  const openAdjust = (row: Row) => {
    setMode("delta");
    setAmount("");
    setNote("");
    setError(undefined);
    setAdjusting(row);
  };

  return (
    <>
      <PageHeader title={t("inventory.title")} />
      <Show when={productId()}>
        <p class="mb-3 text-sm text-muted-foreground">
          {t("inventory.filtered")}{" "}
          <button
            type="button"
            class="text-accent-700 underline"
            onClick={() => setParams({ product: undefined })}
          >
            {t("inventory.showAll")}
          </button>
        </p>
      </Show>
      <QueryState query={levels}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                title={t("inventory.emptyTitle")}
                description={t("inventory.emptyDesc")}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("inventory.sku")}</Th>
                    <Th class="text-right">{t("inventory.onHand")}</Th>
                    <Th class="text-right">{t("inventory.reserved")}</Th>
                    <Th class="text-right">{t("inventory.available")}</Th>
                    <Th>{t("inventory.tracked")}</Th>
                    <Th>{t("inventory.backorder")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(r) => (
                      <tr>
                        <th scope="row" class={`${tdClass} text-left`}>
                          <A
                            href={`/products/${r.product_id}`}
                            class="figures text-xs font-medium text-accent-700 hover:underline"
                          >
                            {r.sku}
                          </A>
                        </th>
                        <td class={`${tdClass} figures text-right`}>{r.on_hand}</td>
                        <td class={`${tdClass} figures text-right`}>{r.reserved}</td>
                        <td
                          class={`${tdClass} figures text-right font-medium`}
                          classList={{ "text-error-700": r.track && r.available <= 0 }}
                        >
                          {r.available}
                        </td>
                        <td class={tdClass}>
                          <Checkbox
                            label={
                              <span class="sr-only">{`${t("inventory.tracked")}: ${r.sku}`}</span>
                            }
                            checked={r.track}
                            disabled={
                              settings.isPending &&
                              settings.variables?.row.variant_id === r.variant_id
                            }
                            onChange={(track) =>
                              settings.mutate({ row: r, track, allow_backorder: r.allow_backorder })
                            }
                          />
                        </td>
                        <td class={tdClass}>
                          <Checkbox
                            label={
                              <span class="sr-only">{`${t("inventory.backorder")}: ${r.sku}`}</span>
                            }
                            checked={r.allow_backorder}
                            disabled={
                              settings.isPending &&
                              settings.variables?.row.variant_id === r.variant_id
                            }
                            onChange={(allow_backorder) =>
                              settings.mutate({ row: r, track: r.track, allow_backorder })
                            }
                          />
                        </td>
                        <td class={`${tdClass} text-right whitespace-nowrap`}>
                          <Button category="tertiary" onClick={() => openAdjust(r)}>
                            {t("inventory.adjust")}
                            <span class="sr-only">: {r.sku}</span>
                          </Button>
                          <Button category="tertiary" onClick={() => setViewing(r)}>
                            {t("inventory.movements")}
                            <span class="sr-only">: {r.sku}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={levels.hasNextPage}>
              <Button
                class="mt-3"
                loading={levels.isFetchingNextPage}
                onClick={() => void levels.fetchNextPage()}
              >
                {t("common.loadMore")}
              </Button>
            </Show>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={adjusting() !== null}
        onOpenChange={(o) => !o && setAdjusting(null)}
        title={t("inventory.adjustTitle", { sku: adjusting()?.sku ?? "" })}
        size="sm"
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            const row = adjusting();
            if (row) adjust.mutate(row);
          }}
        >
          <SelectField
            label={t("inventory.mode")}
            value={mode()}
            options={[
              { value: "delta", label: t("inventory.modeDelta") },
              { value: "count", label: t("inventory.modeCount") },
            ]}
            onChange={(v) => setMode(v as "delta" | "count")}
          />
          <TextField
            label={mode() === "delta" ? t("inventory.delta") : t("inventory.counted")}
            value={amount()}
            onChange={setAmount}
            inputMode="numeric"
            inputClass="figures text-right"
            required
          />
          <TextField
            label={t("inventory.note")}
            description={t("inventory.noteHint")}
            value={note()}
            onChange={setNote}
            maxLength={500}
          />
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setAdjusting(null)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="confirm" loading={adjust.isPending}>
              {t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={viewing() !== null}
        onOpenChange={(o) => !o && setViewing(null)}
        title={t("inventory.movementsTitle", { sku: viewing()?.sku ?? "" })}
      >
        <Show when={viewing()} fallback={<LoadingState label={t("common.loading")} />}>
          {(r) => <Movements row={r()} />}
        </Show>
      </Dialog>
    </>
  );
}
