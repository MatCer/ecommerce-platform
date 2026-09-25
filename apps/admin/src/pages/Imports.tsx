import {
  Badge,
  Button,
  ConfirmDialog,
  EmptyState,
  PermissionDenied,
  SelectField,
  TextField,
} from "@platform/ui";
import { A, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
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
        <p role="alert" class="text-error-700">
          {error()}
        </p>
      </Show>
      <QueryState query={query}>
        {(run) => (
          <div class="flex max-w-4xl flex-col gap-5">
            <div role="status" class="flex flex-wrap items-center gap-2">
              <Badge
                tone={
                  run.status === "applied" ? "success" : run.status === "failed" ? "error" : "info"
                }
              >
                {t(`content.${run.status}`)}
              </Badge>
              <span class="text-sm">
                {SOURCES[run.source]} · {formatDateTime(run.created_at)}
              </span>
            </div>
            <Show when={run.error}>
              <p role="alert" class="text-error-700">
                {run.error}
              </p>
            </Show>
            <Show when={run.status === "applying"}>
              <div role="status">
                <label class="block text-sm" for="import-progress">
                  {t("content.progress")}: {run.progress.done}/{run.progress.total}
                </label>
                <progress
                  id="import-progress"
                  class="w-full accent-accent-600"
                  max={Math.max(1, run.progress.total)}
                  value={run.progress.done}
                />
              </div>
            </Show>
            <Show when={run.report}>{(report) => <ImportReport report={report()} />}</Show>
            <Show when={run.status === "applied" || run.status === "failed"}>
              <section aria-label={t("content.finalCounts")}>
                <h2 class="mb-3 font-semibold">{t("content.finalCounts")}</h2>
                <dl class="grid grid-cols-2 gap-3 sm:grid-cols-3">
                  <For each={PROGRESS}>
                    {(key) => (
                      <div>
                        <dt class="text-xs text-muted-foreground">
                          {key === "updated" ? t("content.updatedCount") : t(`content.${key}`)}
                        </dt>
                        <dd class="figures">{run.progress[key]}</dd>
                      </div>
                    )}
                  </For>
                </dl>
              </section>
            </Show>
            <div class="flex flex-wrap gap-2">
              <Show when={run.status === "analyzed"}>
                <Button
                  variant="primary"
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
    [progress, setProgress] = createSignal(0),
    [error, setError] = createSignal<string>();
  const marketId = () =>
    market() ||
    markets.data?.items.find((m) => m.is_default)?.id ||
    markets.data?.items[0]?.id ||
    "";
  const qc = useQueryClient();
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
      <form
        class="mb-6 flex max-w-2xl flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          start.mutate();
        }}
      >
        <h2 class="font-semibold">{t("content.newImport")}</h2>
        <fieldset disabled={start.isPending} class="flex min-w-0 flex-col gap-3">
          <div class="grid gap-3 sm:grid-cols-2">
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
          <fieldset class="flex flex-wrap gap-4">
            <legend class="mb-2 text-xs font-medium">{t("content.mode")}</legend>
            <label class="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="import-mode"
                value="file"
                checked={mode() === "file"}
                onChange={() => setMode("file")}
              />
              {t("content.fileMode")}
            </label>
            <label class="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="import-mode"
                value="url"
                checked={mode() === "url"}
                onChange={() => setMode("url")}
              />
              {t("content.urlMode")}
            </label>
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
            <label class="flex flex-col gap-2 text-sm">
              {t("content.file")}
              <input
                type="file"
                accept=".xml,application/xml,text/xml"
                required
                onChange={(e) => setFile(e.currentTarget.files?.[0])}
                class="max-w-full"
              />
            </label>
          </Show>
        </fieldset>
        <Show when={start.isPending}>
          <div role="status">
            <progress max={1} value={progress()} aria-label={t("content.fileMode")} />{" "}
            {Math.round(progress() * 100)}%
          </div>
        </Show>
        <Show when={error()}>
          <p role="alert" class="text-error-700">
            {error()}
          </p>
        </Show>
        <div>
          <Button type="submit" variant="primary" disabled={!marketId()} loading={start.isPending}>
            {t("content.startImport")}
          </Button>
        </div>
      </form>
      <h2 class="mb-3 font-semibold">{t("content.history")}</h2>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState title={t("content.emptyImports")} />}
          >
            <div class="overflow-x-auto">
              <table class={tableClass} aria-label={t("content.history")}>
                <thead>
                  <tr>
                    <Th>{t("content.created")}</Th>
                    <Th>{t("content.source")}</Th>
                    <Th>{t("content.status")}</Th>
                    <Th>{t("content.items")}</Th>
                    <Th>{t("content.new_products")}</Th>
                    <Th>{t("content.updated_products")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(run) => (
                      <tr>
                        <td class={tdClass}>
                          <A href={`/imports/${run.id}`} class="text-accent-700 hover:underline">
                            {formatDateTime(run.created_at)}
                          </A>
                        </td>
                        <td class={tdClass}>{SOURCES[run.source]}</td>
                        <td class={tdClass}>
                          <Badge>{t(`content.${run.status}`)}</Badge>
                        </td>
                        <td class={tdClass}>{run.report?.items ?? "—"}</td>
                        <td class={tdClass}>{run.report?.new_products ?? "—"}</td>
                        <td class={tdClass}>{run.report?.updated_products ?? "—"}</td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
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
