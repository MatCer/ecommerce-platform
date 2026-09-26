import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  ConfirmDialog,
  EmptyState,
  FormGroup,
  fileInputClass,
  labelClass,
  PermissionDenied,
  ProgressBar,
  Radio,
  SelectField,
  TextField,
  type Tone,
} from "@platform/ui";
import { A, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, createUniqueId, For, onCleanup, Show } from "solid-js";
import { ImportReport } from "../components/ImportReport.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";
import { putFile } from "../lib/upload.ts";

const SOURCES = { heureka: "Heureka XML", google: "Google Merchant XML" };
const PROGRESS = [
  "done",
  "total",
  "created",
  "updated",
  "failed",
  "images_downloaded",
  "images_failed",
  "redirects_created",
  "redirects_skipped",
] as const;
const runTone = (status: string): Tone =>
  status === "applied" ? "success" : status === "failed" ? "error" : "info";
function RunDetail() {
  const qc = useQueryClient();
  const params = useParams();
  const [confirm, setConfirm] = createSignal(false),
    [error, setError] = createSignal<string>();
  const query = createQuery(() => ({
    queryKey: tenantKey("import", params.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/imports/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
    refetchInterval: (q) => {
      const s = q.state.data?.status;
      return s === "analyzing" || s === "applying" ? 2000 : false;
    },
  }));
  createEffect(() => {
    const status = query.data?.status;
    if (status) void qc.invalidateQueries({ queryKey: tenantKey("imports") });
  });
  const action = createMutation(() => ({
    mutationFn: (kind: "analyze" | "apply") =>
      unwrap(
        api.POST(
          kind === "apply" ? "/admin/v1/imports/{id}/apply" : "/admin/v1/imports/{id}/analyze",
          { params: { header: tenantHeader(), path: { id: params.id ?? "" } } },
        ),
      ),
    onSuccess: () => {
      setConfirm(false);
      setError(undefined);
      void query.refetch();
    },
    onError: (e: unknown) => {
      setConfirm(false);
      setError(contentError(e));
    },
  }));
  return (
    <>
      <PageHeader
        title={t("content.imports")}
        back={{ href: "/imports", label: t("content.history") }}
      />
      <Show when={error()}>
        <Alert tone="error" class="mb-4">
          {error()}
        </Alert>
      </Show>
      <QueryState query={query}>
        {(run) => (
          <div class="flex max-w-4xl flex-col gap-5">
            <div role="status" class="flex flex-wrap items-center gap-2">
              <Badge tone={runTone(run.status)}>{t(`content.${run.status}`)}</Badge>
              <span class="text-sm text-muted-foreground">
                {SOURCES[run.source]} · {formatDateTime(run.created_at)}
              </span>
            </div>
            <Show when={run.error}>
              <Alert tone="error">{run.error}</Alert>
            </Show>
            <Show when={run.status === "applying"}>
              <div role="status">
                <ProgressBar
                  showLabel
                  tone="info"
                  label={`${t("content.progress")}: ${run.progress.done}/${run.progress.total}`}
                  max={Math.max(1, run.progress.total)}
                  value={run.progress.done}
                />
              </div>
            </Show>
            <Show when={run.report}>{(report) => <ImportReport report={report()} />}</Show>
            <Show when={run.status === "applied" || run.status === "failed"}>
              <section aria-label={t("content.finalCounts")}>
                <Card title={t("content.finalCounts")}>
                  <dl class="grid grid-cols-2 gap-4 sm:grid-cols-3">
                    <For each={PROGRESS}>
                      {(key) => (
                        <div class="flex flex-col gap-1">
                          <dt class="text-sm text-muted-foreground">
                            {key === "updated" ? t("content.updatedCount") : t(`content.${key}`)}
                          </dt>
                          <dd class="figures text-lg font-semibold text-heading">
                            {run.progress[key]}
                          </dd>
                        </div>
                      )}
                    </For>
                  </dl>
                </Card>
              </section>
            </Show>
            <div class="flex flex-wrap gap-2">
              <Show when={run.status === "analyzed"}>
                <Button
                  variant="confirm"
                  disabled={action.isPending}
                  onClick={() => setConfirm(true)}
                >
                  {t("content.apply")}
                </Button>
              </Show>
              <Show when={run.status === "analyzed" || run.status === "failed"}>
                <Button loading={action.isPending} onClick={() => action.mutate("analyze")}>
                  {t("content.rerun")}
                </Button>
              </Show>
            </div>
          </div>
        )}
      </QueryState>
      <ConfirmDialog
        open={confirm()}
        onOpenChange={setConfirm}
        title={t("content.apply")}
        description={t("content.applyConfirm")}
        confirmLabel={t("content.apply")}
        cancelLabel={t("common.cancel")}
        pending={action.isPending}
        onConfirm={() => action.mutate("apply")}
      />
    </>
  );
}
function ImportList() {
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  const navigate = useNavigate(),
    markets = useMarkets();
  const list = createQuery(() => ({
    queryKey: tenantKey("imports"),
    queryFn: () => unwrap(api.GET("/admin/v1/imports", { params: { header: tenantHeader() } })),
  }));
  const [source, setSource] = createSignal<Schemas["Source"]>("heureka"),
    [market, setMarket] = createSignal(""),
    [mode, setMode] = createSignal("file"),
    [file, setFile] = createSignal<File>(),
    [url, setUrl] = createSignal(""),
    [activate, setActivate] = createSignal(false),
    [progress, setProgress] = createSignal(0),
    [error, setError] = createSignal<string>();
  const marketId = () =>
    market() ||
    markets.data?.items.find((m) => m.is_default)?.id ||
    markets.data?.items[0]?.id ||
    "";
  const qc = useQueryClient();
  const fileId = createUniqueId();
  const start = createMutation(() => ({
    mutationFn: async () => {
      setError(undefined);
      setProgress(0);
      const key = tenantKey("imports");
      const header = tenantHeader(),
        f = file();
      if (mode() === "file" && (!f?.size || f.size > 100 * 1024 * 1024))
        throw new Error(t("content.fileRequired"));
      const body: Schemas["NewImport"] = {
        source: source(),
        market_id: marketId(),
        activate: activate(),
        ...(mode() === "file" ? { upload_size: f?.size } : { url: url().trim() }),
      };
      const result = await unwrap(api.POST("/admin/v1/imports", { params: { header }, body }));
      if (mode() === "file") {
        if (!result.upload || !f) throw new Error(t("content.fileRequired"));
        await putFile(result.upload.url, result.upload.headers, f, setProgress);
        await unwrap(
          api.POST("/admin/v1/imports/{id}/analyze", {
            params: { header, path: { id: result.run.id } },
          }),
        );
      }
      return { id: result.run.id, key };
    },
    onSuccess: ({ id, key }) => {
      void qc.invalidateQueries({ queryKey: key });
      if (alive) navigate(`/imports/${id}`);
    },
    onError: (e: unknown) => {
      setError(
        e instanceof Error && e.message === t("content.fileRequired") ? e.message : contentError(e),
      );
      void list.refetch();
    },
  }));
  return (
    <>
      <PageHeader title={t("content.imports")} />
      <Card class="mb-6 max-w-2xl" title={t("content.newImport")}>
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
                label={t("content.source")}
                value={source()}
                options={Object.entries(SOURCES).map(([value, label]) => ({ value, label }))}
                onChange={(v) => setSource(v === "google" ? "google" : "heureka")}
              />
              <QueryState query={markets}>
                {(data) => (
                  <SelectField
                    label={t("content.market")}
                    value={marketId()}
                    onChange={setMarket}
                    options={data.items.map((m) => ({ value: m.id, label: m.code.toUpperCase() }))}
                  />
                )}
              </QueryState>
            </div>
            <fieldset class="flex flex-wrap gap-x-6 gap-y-2">
              <legend class={`${labelClass} mb-2`}>{t("content.mode")}</legend>
              <Radio
                name="import-mode"
                value="file"
                label={t("content.fileMode")}
                checked={mode() === "file"}
                onChange={() => setMode("file")}
              />
              <Radio
                name="import-mode"
                value="url"
                label={t("content.urlMode")}
                checked={mode() === "url"}
                onChange={() => setMode("url")}
              />
            </fieldset>
            <Show
              when={mode() === "file"}
              fallback={
                <TextField
                  label={t("content.url")}
                  type="url"
                  required
                  value={url()}
                  onChange={setUrl}
                />
              }
            >
              <FormGroup label={t("content.file")} for={fileId}>
                <input
                  id={fileId}
                  type="file"
                  accept=".xml,application/xml,text/xml"
                  required
                  onChange={(e) => setFile(e.currentTarget.files?.[0])}
                  class={fileInputClass}
                />
              </FormGroup>
            </Show>
          </fieldset>
          <Checkbox
            label={t("content.activate")}
            description={t("content.activateHint")}
            checked={activate()}
            onChange={setActivate}
          />
          <Show when={start.isPending}>
            <div role="status">
              <ProgressBar
                showLabel
                tone="info"
                label={t("content.fileMode")}
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
              disabled={!marketId()}
              loading={start.isPending}
            >
              {t("content.startImport")}
            </Button>
          </div>
        </form>
      </Card>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState icon="upload" title={t("content.emptyImports")} />}
          >
            <Card title={t("content.history")} count={data.items.length} padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t("content.history")}>
                  <thead>
                    <tr>
                      <Th>{t("content.created")}</Th>
                      <Th>{t("content.source")}</Th>
                      <Th>{t("content.status")}</Th>
                      <Th class="text-right">{t("content.items")}</Th>
                      <Th class="text-right">{t("content.new_products")}</Th>
                      <Th class="text-right">{t("content.updated_products")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(run) => (
                        <tr>
                          <td class={tdClass}>
                            <A
                              href={`/imports/${run.id}`}
                              class="figures font-semibold text-heading hover:text-accent-700 hover:underline"
                            >
                              {formatDateTime(run.created_at)}
                            </A>
                          </td>
                          <td class={tdClass}>{SOURCES[run.source]}</td>
                          <td class={tdClass}>
                            <Badge tone={runTone(run.status)}>{t(`content.${run.status}`)}</Badge>
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {run.report?.items ?? "—"}
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {run.report?.new_products ?? "—"}
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {run.report?.updated_products ?? "—"}
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
export default function Imports() {
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
