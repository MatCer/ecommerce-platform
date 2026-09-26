import {
  Alert,
  Badge,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  FormGroup,
  fileInputClass,
  labelClass,
  PermissionDenied,
  ProgressBar,
  SelectField,
  type Tone,
} from "@platform/ui";
import { A, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, onCleanup, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, idempotencyKey, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import {
  fullMapping,
  guessMapping,
  headerRow,
  IMPORT_FIELDS,
  type ImportKind,
  MAX_IMPORT_BYTES,
  missingRequired,
} from "../lib/portability.ts";
import { useMarkets } from "../lib/queries.ts";
import { putFile } from "../lib/upload.ts";

type Run = Schemas["DataImport"];
type RunStatus = Schemas["DataImportStatus"];

const KINDS: ImportKind[] = ["customers", "orders", "subscribers"];
const tone: Record<RunStatus, Tone> = {
  pending: "neutral",
  analyzing: "info",
  analyzed: "info",
  applying: "info",
  applied: "success",
  failed: "error",
};
const COUNTS = [
  "with_address",
  "lines",
  "linked_to_customer",
  "subscribed",
  "pending_not_marketable",
  "already_subscribed",
  "kept_unsubscribed",
] as const;
type CountKey = (typeof COUNTS)[number];
const isCount = (k: string): k is CountKey => (COUNTS as readonly string[]).includes(k);

function kindLabel(kind: ImportKind): string {
  return t(`data.kind_${kind}`);
}

function Counts(props: { counts: Record<string, number> }) {
  return (
    <For each={Object.entries(props.counts).filter(([k]) => isCount(k))}>
      {([key, n]) => (
        <div class="flex flex-col gap-1">
          <dt class="text-sm text-muted-foreground">
            {isCount(key) ? t(`data.count_${key}`) : key}
          </dt>
          <dd class="figures text-lg font-semibold text-heading">{n}</dd>
        </div>
      )}
    </For>
  );
}

function Report(props: { report: Schemas["DataImportReport"] }) {
  const r = () => props.report;
  const previewColumns = () => [...new Set(r().preview.flatMap((row) => Object.keys(row)))];
  return (
    <div class="flex flex-col gap-5">
      <Card>
        <dl class="grid grid-cols-2 gap-4 sm:grid-cols-5">
          <div class="flex flex-col gap-1">
            <dt class="text-sm text-muted-foreground">{t("data.rows")}</dt>
            <dd class="figures text-lg font-semibold text-heading">{r().rows}</dd>
          </div>
          <div class="flex flex-col gap-1">
            <dt class="text-sm text-muted-foreground">{t("data.records")}</dt>
            <dd class="figures text-lg font-semibold text-heading">{r().records}</dd>
          </div>
          <div class="flex flex-col gap-1">
            <dt class="text-sm text-muted-foreground">{t("data.invalid")}</dt>
            <dd
              class="figures text-lg font-semibold text-heading"
              classList={{ "text-error-700": r().invalid_rows > 0 }}
            >
              {r().invalid_rows}
            </dd>
          </div>
          <div class="flex flex-col gap-1">
            <dt class="text-sm text-muted-foreground">{t("data.new")}</dt>
            <dd class="figures text-lg font-semibold text-heading">{r().new}</dd>
          </div>
          <div class="flex flex-col gap-1">
            <dt class="text-sm text-muted-foreground">{t("data.existing")}</dt>
            <dd class="figures text-lg font-semibold text-heading">{r().existing}</dd>
          </div>
          <Counts counts={r().counts} />
        </dl>
      </Card>
      <Show when={r().errors.length}>
        <Card
          padding="none"
          titleId="row-errors"
          title={t("data.rowErrors")}
          count={r().errors.length}
          description={r().truncated ? t("data.truncated") : undefined}
        >
          <div class="max-h-96 overflow-auto">
            <table class={tableClass} aria-labelledby="row-errors">
              <thead>
                <tr>
                  <Th>{t("data.line")}</Th>
                  <Th>{t("data.field")}</Th>
                  <Th>{t("data.problem")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={r().errors}>
                  {(e) => (
                    <tr>
                      <td class={`${tdClass} figures`}>{e.line}</td>
                      <td class={`${tdClass} font-mono text-xs`}>{e.field ?? "—"}</td>
                      <td class={tdClass}>{e.detail}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Card>
      </Show>
      <Show when={r().preview.length}>
        <Card padding="none" titleId="import-preview" title={t("data.preview")}>
          <div class="overflow-x-auto">
            <table class={tableClass} aria-labelledby="import-preview">
              <thead>
                <tr>
                  <For each={previewColumns()}>{(c) => <Th>{c}</Th>}</For>
                </tr>
              </thead>
              <tbody>
                <For each={r().preview}>
                  {(row) => (
                    <tr>
                      <For each={previewColumns()}>
                        {(c) => <td class={tdClass}>{row[c] ?? ""}</td>}
                      </For>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Card>
      </Show>
    </div>
  );
}

function RunDetail() {
  const params = useParams();
  const qc = useQueryClient();
  const [confirm, setConfirm] = createSignal(false),
    [error, setError] = createSignal<string>();
  const query = createQuery(() => ({
    queryKey: tenantKey("data-import", params.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/data-imports/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
    refetchInterval: (q) => {
      const s = q.state.data?.status;
      return s === "analyzing" || s === "applying" ? 2000 : false;
    },
  }));
  const action = createMutation(() => ({
    mutationFn: (kind: "analyze" | "apply") => {
      const opts = { params: { header: tenantHeader(), path: { id: params.id ?? "" } } };
      return unwrap(
        kind === "apply"
          ? api.POST("/admin/v1/data-imports/{id}/apply", opts)
          : api.POST("/admin/v1/data-imports/{id}/analyze", { ...opts, body: {} }),
      );
    },
    onSuccess: () => {
      setConfirm(false);
      setError(undefined);
      void query.refetch();
      void qc.invalidateQueries({ queryKey: tenantKey("data-imports") });
    },
    onError: (e: unknown) => {
      setConfirm(false);
      setError(contentError(e));
    },
  }));
  return (
    <>
      <PageHeader
        title={t("data.imports")}
        back={{ href: "/data/imports", label: t("data.history") }}
      />
      <Show when={error()}>
        <Alert tone="error" class="mb-4">
          {error()}
        </Alert>
      </Show>
      <QueryState query={query}>
        {(run: Run) => (
          <div class="flex max-w-5xl flex-col gap-5">
            <div role="status" class="flex flex-wrap items-center gap-2">
              <Badge tone={tone[run.status]}>{t(`data.status_${run.status}`)}</Badge>
              <span class="text-sm text-muted-foreground">
                {kindLabel(run.kind)} · {formatDateTime(run.created_at)}
              </span>
            </div>
            <Show when={run.error}>
              <Alert tone="error">{run.error}</Alert>
            </Show>
            <Show when={run.status === "applying" || run.status === "applied"}>
              <div role="status" class="rounded-lg border border-border p-4">
                <ProgressBar
                  showLabel
                  tone={run.status === "applied" ? "success" : "info"}
                  label={`${t("data.progress")}: ${run.progress.done}/${run.progress.total} · ${t("data.createdCount")} ${run.progress.created} · ${t("data.updatedCount")} ${run.progress.updated}`}
                  max={Math.max(1, run.progress.total)}
                  value={run.progress.done}
                />
                <Show when={Object.keys(run.progress.outcomes).length}>
                  <dl class="mt-4 grid grid-cols-2 gap-4 sm:grid-cols-4">
                    <Counts counts={run.progress.outcomes} />
                  </dl>
                </Show>
              </div>
            </Show>
            <Show when={run.report}>{(report) => <Report report={report()} />}</Show>
            <div class="flex flex-wrap gap-2">
              <Show when={run.status === "analyzed" && (run.report?.records ?? 0) > 0}>
                <Button
                  variant="confirm"
                  disabled={action.isPending}
                  onClick={() => setConfirm(true)}
                >
                  {t("data.apply")}
                </Button>
              </Show>
              <Show when={run.status === "analyzed" || run.status === "failed"}>
                <Button loading={action.isPending} onClick={() => action.mutate("analyze")}>
                  {t("data.rerun")}
                </Button>
              </Show>
            </div>
            <ConfirmDialog
              open={confirm()}
              onOpenChange={setConfirm}
              title={t("data.apply")}
              description={t("data.applyConfirm", { count: String(run.report?.records ?? 0) })}
              confirmLabel={t("data.apply")}
              cancelLabel={t("common.cancel")}
              pending={action.isPending}
              onConfirm={() => action.mutate("apply")}
            />
          </div>
        )}
      </QueryState>
    </>
  );
}

function NewImport() {
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  const navigate = useNavigate(),
    markets = useMarkets(),
    qc = useQueryClient();
  const [kind, setKind] = createSignal<ImportKind>("customers"),
    [market, setMarket] = createSignal(""),
    [file, setFile] = createSignal<File>(),
    [headers, setHeaders] = createSignal<string[]>([]),
    [mapping, setMapping] = createSignal<Record<string, string>>({}),
    [progress, setProgress] = createSignal(0),
    [error, setError] = createSignal<string>();
  const fileId = createUniqueId();
  const marketId = () =>
    market() ||
    markets.data?.items.find((m) => m.is_default)?.id ||
    markets.data?.items[0]?.id ||
    "";
  const pick = async (f: File | undefined) => {
    setFile(f);
    setError(undefined);
    const head = f ? headerRow(await f.slice(0, 64 * 1024).text()) : [];
    setHeaders(head);
    setMapping(guessMapping(kind(), head));
  };
  const changeKind = (k: ImportKind) => {
    setKind(k);
    setMapping(guessMapping(k, headers()));
  };
  const missing = () => (file() ? missingRequired(kind(), mapping()) : []);
  const start = createMutation(() => ({
    mutationFn: async () => {
      const f = file();
      if (!f?.size || f.size > MAX_IMPORT_BYTES) throw new Error(t("data.fileRequired"));
      setProgress(0);
      const header = idempotencyKey();
      const result = await unwrap(
        api.POST("/admin/v1/data-imports", {
          params: { header },
          body: {
            kind: kind(),
            market_id: marketId(),
            upload_size: f.size,
            mapping: fullMapping(kind(), mapping()),
          },
        }),
      );
      await putFile(result.upload.url, result.upload.headers, f, setProgress);
      const tenant = { "X-Tenant-Id": header["X-Tenant-Id"] };
      await unwrap(
        api.POST("/admin/v1/data-imports/{id}/analyze", {
          params: { header: tenant, path: { id: result.import.id } },
          body: {},
        }),
      );
      return result.import.id;
    },
    onSuccess: (id: string) => {
      void qc.invalidateQueries({ queryKey: tenantKey("data-imports") });
      if (alive) navigate(`/data/imports/${id}`);
    },
    onError: (e: unknown) => {
      setError(
        e instanceof Error && e.message === t("data.fileRequired") ? e.message : contentError(e),
      );
    },
  }));
  return (
    <Card class="mb-6 max-w-2xl" title={t("data.newImport")}>
      <form
        class="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          start.mutate();
        }}
      >
        <fieldset disabled={start.isPending} class="flex min-w-0 flex-col gap-4">
          <div class="grid gap-4 sm:grid-cols-2">
            <SelectField
              label={t("data.kind")}
              value={kind()}
              options={KINDS.map((k) => ({ value: k, label: kindLabel(k) }))}
              onChange={(v) => changeKind(KINDS.find((k) => k === v) ?? "customers")}
            />
            <QueryState query={markets}>
              {(data) => (
                <SelectField
                  label={t("data.market")}
                  value={marketId()}
                  onChange={setMarket}
                  options={data.items.map((m) => ({ value: m.id, label: m.code.toUpperCase() }))}
                />
              )}
            </QueryState>
          </div>
          <p class="text-sm text-muted-foreground">{t(`data.hint_${kind()}`)}</p>
          <FormGroup label={t("data.file")} for={fileId}>
            <input
              id={fileId}
              type="file"
              accept=".csv,text/csv"
              required
              onChange={(e) => void pick(e.currentTarget.files?.[0])}
              class={fileInputClass}
            />
          </FormGroup>
          <Show when={headers().length}>
            <fieldset class="flex flex-col gap-2">
              <legend class={`${labelClass} mb-1`}>{t("data.mapping")}</legend>
              <p class="text-sm text-muted-foreground">{t("data.mappingHint")}</p>
              <div class="grid gap-4 sm:grid-cols-2">
                <For each={IMPORT_FIELDS[kind()]}>
                  {(field) => (
                    <SelectField
                      label={`${field.name}${field.required ? " *" : ""}`}
                      value={mapping()[field.name] ?? ""}
                      options={[
                        { value: "", label: t("data.noColumn") },
                        ...headers().map((h) => ({ value: h, label: h })),
                      ]}
                      onChange={(v) => {
                        const next = { ...mapping() };
                        next[field.name] = v;
                        setMapping(next);
                      }}
                    />
                  )}
                </For>
              </div>
            </fieldset>
          </Show>
        </fieldset>
        <Show when={missing().length}>
          <Alert tone="error">{t("data.missing", { fields: missing().join(", ") })}</Alert>
        </Show>
        <Show when={start.isPending}>
          <div role="status">
            <ProgressBar
              showLabel
              tone="info"
              label={t("data.uploading")}
              max={1}
              value={progress()}
            />
          </div>
        </Show>
        <Show when={error()}>
          <Alert tone="error">{error()}</Alert>
        </Show>
        <div class="border-t border-border pt-4">
          <Button
            type="submit"
            variant="confirm"
            disabled={!marketId() || missing().length > 0}
            loading={start.isPending}
          >
            {t("data.start")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function ImportList() {
  const list = createQuery(() => ({
    queryKey: tenantKey("data-imports"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/data-imports", { params: { header: tenantHeader() } })),
  }));
  return (
    <>
      <PageHeader title={t("data.imports")} />
      <NewImport />
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState icon="upload" title={t("data.empty")} />}
          >
            <Card title={t("data.history")} count={data.items.length} padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t("data.history")}>
                  <thead>
                    <tr>
                      <Th>{t("data.created")}</Th>
                      <Th>{t("data.kind")}</Th>
                      <Th>{t("data.status")}</Th>
                      <Th class="text-right">{t("data.records")}</Th>
                      <Th class="text-right">{t("data.invalid")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(run) => (
                        <tr>
                          <td class={tdClass}>
                            <A
                              href={`/data/imports/${run.id}`}
                              class="figures font-semibold text-heading hover:text-accent-700 hover:underline"
                            >
                              {formatDateTime(run.created_at)}
                            </A>
                          </td>
                          <td class={tdClass}>{kindLabel(run.kind)}</td>
                          <td class={tdClass}>
                            <Badge tone={tone[run.status]}>{t(`data.status_${run.status}`)}</Badge>
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {run.report?.records ?? "—"}
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {run.report?.invalid_rows ?? "—"}
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
    </>
  );
}

/** CSV imports of customers, historical orders and newsletter subscribers (WP13b). */
export default function DataImports() {
  const { me, can } = useMembership(),
    params = useParams();
  return (
    <QueryState query={me}>
      {() => (
        <Show
          when={can("admin")}
          fallback={
            <PermissionDenied
              title={t("common.forbiddenTitle")}
              description={t("common.forbiddenDesc")}
            />
          }
        >
          <Show when={params.id} fallback={<ImportList />}>
            <RunDetail />
          </Show>
        </Show>
      )}
    </QueryState>
  );
}
