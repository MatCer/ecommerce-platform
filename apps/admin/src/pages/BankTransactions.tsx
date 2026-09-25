import { Badge, Button, EmptyState, SelectField, type Tone } from "@platform/ui";
import { A } from "@solidjs/router";
import {
  createInfiniteQuery,
  createMutation,
  createQuery,
  useQueryClient,
} from "@tanstack/solid-query";
import { createSignal, For, type JSX, Show } from "solid-js";
import { ApiProblem } from "../components/CheckoutSettings.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, locale, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";

const statuses: Schemas["TxStatus"][] = [
  "matched",
  "unmatched",
  "partial",
  "overpaid",
  "dismissed",
];
const formats = ["camt053", "fio_csv", "gpc"] as const;
type Format = (typeof formats)[number];

export const txTone: Record<Schemas["TxStatus"], Tone> = {
  matched: "success",
  unmatched: "warning",
  partial: "warning",
  overpaid: "warning",
  dismissed: "neutral",
};

/** Imported bank transactions and statement upload (WP11, A25). */
export default function BankTransactions() {
  const [status, setStatus] = createSignal<Schemas["TxStatus"]>();
  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("bank-transactions", status() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/bank-transactions", {
          params: {
            header: tenantHeader(),
            query: { status: status(), cursor: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <>
      <PageHeader title={t("pay.bankTitle")} description={t("pay.bankDesc")} />
      <StatementUpload />
      <div class="mb-4 max-w-xs">
        <SelectField
          label={t("pay.status")}
          value={status() ?? ""}
          options={[
            { value: "", label: t("pay.allStatuses") },
            ...statuses.map((value) => ({ value, label: t(`txStatuses.${value}`) })),
          ]}
          onChange={(v) => setStatus(statuses.find((s) => s === v))}
        />
      </div>
      <QueryState query={list}>
        {() => (
          <Show
            when={rows().length}
            fallback={
              <EmptyState
                title={t("pay.noTransactions")}
                description={t("pay.noTransactionsDesc")}
              />
            }
          >
            <TransactionTable rows={rows()} />
            <Show when={list.hasNextPage}>
              <Button
                class="mt-4"
                loading={list.isFetchingNextPage}
                onClick={() => list.fetchNextPage()}
              >
                {t("common.loadMore")}
              </Button>
            </Show>
          </Show>
        )}
      </QueryState>
    </>
  );
}

export function TransactionTable(props: {
  rows: Schemas["BankTransaction"][];
  actions?: (tx: Schemas["BankTransaction"]) => JSX.Element;
}) {
  return (
    <div class="overflow-x-auto">
      <table class={tableClass}>
        <thead>
          <tr>
            <For
              each={[
                t("pay.booked"),
                t("pay.amount"),
                t("pay.vs"),
                t("pay.counterparty"),
                t("pay.status"),
                t("pay.order"),
                t("pay.source"),
              ]}
            >
              {(label) => <Th>{label}</Th>}
            </For>
            <Show when={props.actions}>
              <Th srOnly>{t("common.actions")}</Th>
            </Show>
          </tr>
        </thead>
        <tbody>
          <For each={props.rows}>
            {(tx) => (
              <tr data-testid="bank-tx">
                <td class={`${tdClass} whitespace-nowrap`}>{tx.booked_on}</td>
                <td class={`${tdClass} figures whitespace-nowrap`}>
                  {formatMoney(tx.amount_minor, tx.currency, locale())}
                  <Show when={tx.expected_minor != null && tx.status !== "matched"}>
                    <p class="text-xs text-muted-foreground">
                      {t("pay.expected")}:{" "}
                      {formatMoney(tx.expected_minor ?? 0, tx.currency, locale())}
                    </p>
                  </Show>
                </td>
                <td class={tdClass}>{tx.variable_symbol ?? "—"}</td>
                <td class={tdClass}>
                  {tx.counterparty_name ?? ""}
                  <p class="text-xs text-muted-foreground">{tx.counterparty ?? ""}</p>
                </td>
                <td class={tdClass}>
                  <Badge tone={txTone[tx.status]}>{t(`txStatuses.${tx.status}`)}</Badge>
                  <Show when={tx.reason}>
                    {(r) => <p class="text-xs text-muted-foreground">{t(`txReasons.${r()}`)}</p>}
                  </Show>
                  <Show when={tx.note}>
                    <p class="text-xs">{tx.note}</p>
                  </Show>
                </td>
                <td class={tdClass}>
                  <Show when={tx.order_id} fallback="—">
                    <A class="text-accent-700 hover:underline" href={`/orders/${tx.order_id}`}>
                      {tx.order_number}
                    </A>
                  </Show>
                </td>
                <td class={tdClass}>
                  {tx.source}
                  <p class="text-xs text-muted-foreground">{formatDateTime(tx.created_at)}</p>
                </td>
                <Show when={props.actions}>
                  {(render) => <td class={tdClass}>{render()(tx)}</td>}
                </Show>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

/** Upload of a statement file into a market's receiving account. */
function StatementUpload() {
  const qc = useQueryClient();
  const accounts = createQuery(() => ({
    queryKey: tenantKey("bank-accounts"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/bank-accounts", { params: { header: tenantHeader() } })),
  }));
  const available = () => accounts.data?.items ?? [];
  const [account, setAccount] = createSignal("");
  const [format, setFormat] = createSignal<Format>("camt053");
  const [file, setFile] = createSignal<File>();
  const [report, setReport] = createSignal<Schemas["StatementImport"]>();
  const upload = createMutation(() => ({
    mutationFn: async () => {
      const f = file();
      const id = account() || available()[0]?.id;
      if (!f || !id) throw new ApiError(400, "invalid_statement");
      return unwrap(
        api.POST("/admin/v1/bank-accounts/{id}/statements", {
          params: { header: tenantHeader(), path: { id }, query: { format: format() } },
          body: [],
          // The raw file is the body (the schema's byte array).
          bodySerializer: () => f,
          headers: { "Content-Type": "application/octet-stream" },
        }),
      );
    },
    onSuccess: (r) => {
      setReport(r);
      void qc.invalidateQueries({ queryKey: tenantKey("bank-transactions") });
      void qc.invalidateQueries({ queryKey: tenantKey("payment-exceptions") });
    },
  }));
  return (
    <section class="mb-6 rounded-md border border-border p-4" aria-labelledby="upload-heading">
      <h2 id="upload-heading" class="font-semibold">
        {t("pay.upload")}
      </h2>
      <p class="mb-3 text-sm text-muted-foreground">{t("pay.uploadDesc")}</p>
      <Show when={available().length} fallback={<p class="text-sm">{t("pay.noAccount")}</p>}>
        <form
          class="grid gap-3 sm:grid-cols-3 sm:items-end"
          onSubmit={(e) => {
            e.preventDefault();
            setReport(undefined);
            upload.mutate();
          }}
        >
          <SelectField
            label={t("pay.account")}
            value={account() || (available()[0]?.id ?? "")}
            options={available().map((a) => ({
              value: a.id,
              label: `${a.iban} (${a.currency})${a.active ? "" : ` · ${t("pay.retired")}`}`,
            }))}
            onChange={setAccount}
          />
          <SelectField
            label={t("pay.format")}
            value={format()}
            options={formats.map((f) => ({ value: f, label: t(`statementFormats.${f}`) }))}
            onChange={(v) => setFormat(formats.find((f) => f === v) ?? "camt053")}
          />
          <label class="flex flex-col gap-1 text-xs font-medium text-muted-foreground">
            {t("pay.file")}
            <input
              type="file"
              required
              accept=".xml,.csv,.gpc,.abo,.txt,text/xml,application/xml,text/csv,text/plain"
              onChange={(e) => setFile(e.currentTarget.files?.[0])}
              class="max-w-full text-sm text-foreground"
            />
          </label>
          <div>
            <Button type="submit" variant="primary" loading={upload.isPending}>
              {t("pay.importButton")}
            </Button>
          </div>
        </form>
      </Show>
      <Show when={report()}>
        {(r) => (
          <p role="status" class="mt-3 text-sm" data-testid="import-report">
            {t("pay.imported", {
              imported: r().imported,
              duplicates: r().duplicates,
              debits: r().debits,
              matched: r().matched,
              exceptions: r().exceptions,
            })}
          </p>
        )}
      </Show>
      <ApiProblem error={upload.error} />
    </section>
  );
}
