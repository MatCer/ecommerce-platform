import {
  Badge,
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorState,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { RevisionChanges, ThemeAiEditor } from "../components/ThemeAiEditor.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, tenantId, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import {
  hexOf,
  PENDING,
  parseReport,
  statusTone,
  type TokenGroups,
  tokenErrors,
  tokenGroups,
} from "../lib/themes.ts";

type Revision = Schemas["RevisionSummary"];

/** An API error with the server's reasons (archive problems come one per line). */
function reasons(e: unknown): string {
  if (
    e instanceof ApiError &&
    e.detail &&
    (e.code === "invalid_archive" || e.code === "builds_in_progress")
  )
    return `${errorMessage(e)}\n${e.detail}`;
  return errorMessage(e);
}

/**
 * Themes (WP23, spec §12.3): the shop's theme revisions, their gate reports, a sandboxed preview
 * (A21), publish and rollback, reset to the default theme, archive upload/download for power
 * users, the design-token editor (a token-only revision, fast-path gates) and AI edits (WP24).
 */
export default function Themes() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const [selected, setSelected] = createSignal<string>();
  const [error, setError] = createSignal<string>();
  const [publishing, setPublishing] = createSignal<Revision>();
  const [preview, setPreview] = createSignal<{ number: number; url: string }>();

  const list = createQuery(() => ({
    queryKey: tenantKey("theme-revisions"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/themes/revisions", { params: { header: tenantHeader() } })),
    // Builds take a minute or two: poll while one is moving.
    refetchInterval: (q: { state: { data?: { items: Revision[] } } }) =>
      q.state.data?.items.some((r) => PENDING.has(r.status)) ? 3000 : false,
  }));
  const items = () => list.data?.items ?? [];
  const active = createMemo(() => items().find((r) => r.active));
  const current = () => selected() ?? items()[0]?.id;
  const detail = createQuery(() => ({
    queryKey: tenantKey(
      "theme-revision",
      current(),
      items().find((r) => r.id === current())?.status,
    ),
    enabled: Boolean(current()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/themes/revisions/{id}", {
          params: { header: tenantHeader(), path: { id: current() ?? "" } },
        }),
      ),
  }));
  const activeTokens = createQuery(() => ({
    queryKey: tenantKey("theme-revision", active()?.id, "tokens"),
    enabled: Boolean(active()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/themes/revisions/{id}", {
          params: { header: tenantHeader(), path: { id: active()?.id ?? "" } },
        }),
      ),
  }));
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("theme-revisions") });
  const created = async (r: Revision) => {
    setError(undefined);
    setSelected(r.id);
    await refresh();
    showToast({ title: t("themes.queued", { number: r.number }), closeLabel: t("common.close") });
  };
  const header = () => ({ params: { header: tenantHeader() } });

  const fork = createMutation(() => ({
    mutationFn: () => unwrap(api.POST("/admin/v1/themes/revisions/fork", header())),
    onSuccess: created,
    onError: (e: unknown) => setError(reasons(e)),
  }));
  const reset = createMutation(() => ({
    mutationFn: () => unwrap(api.POST("/admin/v1/themes/revisions/reset", header())),
    onSuccess: created,
    onError: (e: unknown) => setError(reasons(e)),
  }));
  const upload = createMutation(() => ({
    mutationFn: (file: File) =>
      unwrap(
        api.POST("/admin/v1/themes/revisions/upload", {
          params: { header: tenantHeader() },
          body: [],
          // The raw archive is the body (the schema's byte array).
          bodySerializer: () => file,
          headers: { "Content-Type": "application/gzip" },
        }),
      ),
    onSuccess: created,
    onError: (e: unknown) => setError(reasons(e)),
  }));
  const publish = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/themes/revisions/{id}/publish", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async (r: Revision) => {
      setPublishing(undefined);
      setError(undefined);
      await refresh();
      showToast({
        title: t("themes.published", { number: r.number }),
        closeLabel: t("common.close"),
      });
    },
    onError: (e: unknown) => {
      setPublishing(undefined);
      setError(reasons(e));
    },
  }));
  const openPreview = createMutation(() => ({
    mutationFn: (r: Revision) =>
      unwrap(
        api.POST("/admin/v1/themes/revisions/{id}/preview", {
          params: { header: tenantHeader(), path: { id: r.id } },
        }),
      ).then((link) => ({ number: r.number, url: link.url })),
    onSuccess: setPreview,
    onError: (e: unknown) => setError(reasons(e)),
  }));
  const download = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.GET("/admin/v1/themes/revisions/{id}/source", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: (d: Schemas["Download"]) => window.location.assign(d.url),
    onError: (e: unknown) => setError(reasons(e)),
  }));
  let fileInput: HTMLInputElement | undefined;

  return (
    <>
      <PageHeader
        title={t("themes.title")}
        description={t("themes.description")}
        actions={
          <Show when={can("admin")}>
            <Show
              when={items().some((r) => r.origin === "custom")}
              fallback={
                <Button variant="primary" loading={fork.isPending} onClick={() => fork.mutate()}>
                  {t("themes.fork")}
                </Button>
              }
            >
              <Button loading={reset.isPending} onClick={() => reset.mutate()}>
                {t("themes.reset")}
              </Button>
            </Show>
            <input
              ref={fileInput}
              type="file"
              accept=".tar.gz,.tgz,application/gzip"
              class="sr-only"
              aria-label={t("themes.uploadLabel")}
              tabIndex={-1}
              onChange={(e) => {
                const f = e.currentTarget.files?.[0];
                if (f) upload.mutate(f);
                e.currentTarget.value = "";
              }}
            />
            <Button loading={upload.isPending} onClick={() => fileInput?.click()}>
              {t("themes.upload")}
            </Button>
          </Show>
        }
      />
      <Show when={error()}>
        <div class="mb-4">
          <ErrorState title={t("themes.actionFailed")} description={error()} />
        </div>
      </Show>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState title={t("themes.empty")} description={t("themes.emptyHint")} />}
          >
            <div class="grid gap-6 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
              <section aria-labelledby="revisions-h">
                <h2 id="revisions-h" class="mb-2 text-sm font-semibold">
                  {t("themes.revisions")}
                </h2>
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>#</Th>
                      <Th>{t("themes.status")}</Th>
                      <Th>{t("themes.change")}</Th>
                      <Th>{t("themes.created")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(r) => (
                        <tr classList={{ "bg-muted": r.id === current() }}>
                          <td class={`${tdClass} figures`}>
                            <button
                              type="button"
                              class="font-semibold text-accent-700 hover:underline"
                              aria-pressed={r.id === current()}
                              onClick={() => setSelected(r.id)}
                            >
                              {t("themes.revision", { number: r.number })}
                            </button>
                          </td>
                          <td class={tdClass}>
                            <span class="flex flex-wrap gap-1">
                              <Badge tone={statusTone(r.status)}>
                                {t(`themes.statuses.${r.status as "ready"}`)}
                              </Badge>
                              <Show when={r.active}>
                                <Badge tone="success">{t("themes.live")}</Badge>
                              </Show>
                            </span>
                          </td>
                          <td class={tdClass}>{t(`themes.changes.${r.change as "fork"}`)}</td>
                          <td class={`${tdClass} text-muted-foreground`}>
                            {formatDateTime(r.created_at)}
                          </td>
                          <td class={`${tdClass} text-right`}>
                            <span class="inline-flex flex-wrap justify-end gap-1">
                              <Show when={r.artifact_id}>
                                <Button
                                  variant="ghost"
                                  loading={
                                    openPreview.isPending && openPreview.variables?.id === r.id
                                  }
                                  onClick={() => openPreview.mutate(r)}
                                >
                                  {t("themes.preview")}
                                </Button>
                              </Show>
                              <Show
                                when={
                                  can("admin") &&
                                  !r.active &&
                                  (r.status === "ready" || r.status === "superseded")
                                }
                              >
                                <Button
                                  variant={r.status === "ready" ? "primary" : "secondary"}
                                  onClick={() => setPublishing(r)}
                                >
                                  {r.status === "ready"
                                    ? t("themes.publish")
                                    : t("themes.rollback")}
                                </Button>
                              </Show>
                              <Show when={can("admin") && r.has_source}>
                                <Button variant="ghost" onClick={() => download.mutate(r.id)}>
                                  {t("themes.download")}
                                </Button>
                              </Show>
                            </span>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </section>
              <section aria-labelledby="report-h" class="min-w-0">
                <Show when={detail.data}>{(d) => <Report detail={d()} />}</Show>
              </section>
            </div>
            <Show when={preview()}>
              {(p) => (
                <section aria-labelledby="preview-h" class="mt-8">
                  <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
                    <h2 id="preview-h" class="text-sm font-semibold">
                      {t("themes.previewOf", { number: p().number })}
                    </h2>
                    <span class="flex gap-2">
                      <a
                        class="text-sm text-accent-700 hover:underline"
                        href={p().url}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {t("themes.openTab")}
                      </a>
                      <Button variant="ghost" onClick={() => setPreview(undefined)}>
                        {t("common.close")}
                      </Button>
                    </span>
                  </div>
                  {/* A21: the preview origin is sandboxed; it is never the admin's own origin. */}
                  <iframe
                    title={t("themes.previewOf", { number: p().number })}
                    src={p().url}
                    sandbox="allow-scripts allow-same-origin allow-forms"
                    referrerpolicy="no-referrer"
                    class="h-[42rem] w-full rounded-md border border-border bg-card"
                  />
                </section>
              )}
            </Show>
            <ThemeAiEditor
              canEdit={can("admin")}
              onShowRevision={(id) => {
                setSelected(id);
                document.getElementById("revisions-h")?.scrollIntoView({ behavior: "smooth" });
              }}
            />
          </Show>
        )}
      </QueryState>
      {/* Outside QueryState: its children are rebuilt whenever the (polled) list changes, which
          would discard an unsaved draft. */}
      <Show when={can("admin") && active()}>
        <TokenEditor
          tokens={activeTokens.data?.tokens}
          base={active()?.id}
          tenant={tenantId()}
          onCreated={created}
          onError={(e) => setError(reasons(e))}
        />
      </Show>
      <ConfirmDialog
        open={Boolean(publishing())}
        onOpenChange={(o) => !o && setPublishing(undefined)}
        title={
          publishing()?.status === "superseded"
            ? t("themes.rollbackTitle", { number: publishing()?.number ?? 0 })
            : t("themes.publishTitle", { number: publishing()?.number ?? 0 })
        }
        description={t("themes.publishDesc")}
        confirmLabel={
          publishing()?.status === "superseded" ? t("themes.rollback") : t("themes.publish")
        }
        cancelLabel={t("common.cancel")}
        pending={publish.isPending}
        onConfirm={() => {
          const r = publishing();
          if (r) publish.mutate(r.id);
        }}
      />
    </>
  );
}

function Report(props: { detail: Schemas["RevisionDetail"] }) {
  const r = () => props.detail.revision;
  const report = createMemo(() => parseReport(props.detail.checks));
  const ms = (v: number | null) => (v === null ? "–" : `${Math.round(v)}`);
  return (
    <div class="flex flex-col gap-4">
      <h2 id="report-h" class="flex flex-wrap items-center gap-2 text-sm font-semibold">
        {t("themes.report", { number: r().number })}
        <Badge tone={statusTone(r().status)}>{t(`themes.statuses.${r().status as "ready"}`)}</Badge>
        <Show when={report().pipeline === "tokens"}>
          <Badge tone="neutral">{t("themes.fastPath")}</Badge>
        </Show>
      </h2>
      <Show when={PENDING.has(r().status)}>
        <p role="status" class="text-sm text-muted-foreground">
          {t("themes.running")}
        </p>
      </Show>
      <Show when={r().parent_id && r().has_source}>
        <RevisionChanges id={r().id} />
      </Show>
      <Show when={report().failures.length}>
        <div
          role="alert"
          class="border-l-2 border-error-600 bg-error-50 px-3 py-2 text-sm text-error-700"
        >
          <p class="font-semibold">{t("themes.failures")}</p>
          <ul class="mt-1 list-disc pl-5">
            <For each={report().failures}>
              {(f) => <li class="break-words whitespace-pre-wrap">{f}</li>}
            </For>
          </ul>
        </div>
      </Show>
      <Show when={report().steps.length}>
        <table class={tableClass}>
          <caption class="sr-only">{t("themes.steps")}</caption>
          <thead>
            <tr>
              <Th>{t("themes.step")}</Th>
              <Th>{t("themes.status")}</Th>
              <Th class="text-right">ms</Th>
            </tr>
          </thead>
          <tbody>
            <For each={report().steps}>
              {(s) => (
                <tr>
                  <td class={tdClass}>{s.name}</td>
                  <td class={tdClass}>
                    <Badge
                      tone={
                        s.status === "passed"
                          ? "success"
                          : s.status === "failed"
                            ? "error"
                            : "neutral"
                      }
                    >
                      {t(`themes.stepStatuses.${s.status}`)}
                    </Badge>
                  </td>
                  <td class={`${tdClass} figures text-right`}>{ms(s.ms)}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
      <Show when={report().pages.length}>
        <div class="overflow-x-auto">
          <table class={tableClass}>
            <caption class="mb-1 text-left text-xs text-muted-foreground">
              {t("themes.budgetCaption")}
            </caption>
            <thead>
              <tr>
                <Th>{t("themes.page")}</Th>
                <Th class="text-right">LCP ms</Th>
                <Th class="text-right">TBT ms</Th>
                <Th class="text-right">CLS</Th>
                <Th class="text-right">JS kB</Th>
                <Th class="text-right">{t("themes.jsRum")}</Th>
                <Th class="text-right">{t("themes.calls")}</Th>
                <Th>axe</Th>
              </tr>
            </thead>
            <tbody>
              <For each={report().pages}>
                {(p) => (
                  <tr classList={{ "text-error-700": p.failures.length > 0 }}>
                    <td class={`${tdClass} figures`}>{p.path}</td>
                    <td class={`${tdClass} figures text-right`}>{ms(p.lcpMs)}</td>
                    <td class={`${tdClass} figures text-right`}>{ms(p.tbtMs)}</td>
                    <td class={`${tdClass} figures text-right`}>
                      {p.cls === null ? "–" : p.cls.toFixed(3)}
                    </td>
                    <td class={`${tdClass} figures text-right`}>{p.jsKb ?? "–"}</td>
                    <td class={`${tdClass} figures text-right`}>{p.jsRumKb ?? "–"}</td>
                    <td class={`${tdClass} figures text-right`}>{p.calls ?? "–"}</td>
                    <td class={tdClass}>{p.axe.length ? p.axe.join(", ") : "0"}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
      <Show when={props.detail.screenshots.length}>
        <ul class="grid grid-cols-2 gap-3 sm:grid-cols-3" aria-label={t("themes.screenshots")}>
          <For each={props.detail.screenshots}>
            {(s) => (
              <li>
                <a href={s.url} target="_blank" rel="noopener noreferrer" class="block">
                  <img
                    src={s.url}
                    alt={t("themes.shotAlt", { name: s.name })}
                    loading="lazy"
                    class="aspect-[3/4] w-full rounded-md border border-border object-cover object-top"
                  />
                  <span class="mt-1 block text-xs text-muted-foreground">{s.name}</span>
                </a>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

function TokenEditor(props: {
  tokens: unknown;
  base: string | undefined;
  tenant: string | null;
  onCreated: (r: Revision) => Promise<void>;
  onError: (e: unknown) => void;
}) {
  const [draft, setDraft] = createSignal<TokenGroups>({ colors: {}, fonts: {}, radius: {} });
  const [dirty, setDirty] = createSignal(false);
  let draftBase = props.base;
  let draftTenant = props.tenant;
  // A refetch preserves edits, while a tenant change must discard the previous tenant's draft.
  createEffect(() => {
    const tokens = props.tokens;
    const tenant = props.tenant;
    const base = props.base;
    if (tenant !== draftTenant) {
      draftTenant = tenant;
      setDirty(false);
    }
    if (!dirty()) {
      draftBase = base;
      setDraft(tokenGroups(tokens));
    }
  });
  const conflict = () => dirty() && props.base !== draftBase;
  const errors = createMemo(() => tokenErrors(draft()));
  const set = (group: keyof TokenGroups, key: string, value: string) => {
    setDirty(true);
    setDraft((d) => ({ ...d, [group]: { ...d[group], [key]: value } }));
  };
  const save = createMutation(() => ({
    mutationFn: () => {
      if (props.tenant !== draftTenant || conflict()) throw new Error(t("themes.draftConflict"));
      const body = { base_revision_id: draftBase ?? null, tokens: draft() };
      return unwrap(
        api.POST("/admin/v1/themes/revisions/tokens", { params: { header: tenantHeader() }, body }),
      );
    },
    onSuccess: (r) => {
      setDirty(false);
      return props.onCreated(r);
    },
    onError: props.onError,
  }));
  const message = (k: string) => {
    const e = errors()[k];
    return e ? t(`themes.invalid.${e}`) : undefined;
  };
  return (
    <section aria-labelledby="tokens-h" class="mt-10 max-w-5xl">
      <h2 id="tokens-h" class="text-sm font-semibold">
        {t("themes.tokens")}
      </h2>
      <p class="mb-3 text-sm text-muted-foreground">{t("themes.tokensHint")}</p>
      <Show when={conflict()}>
        <p role="alert">{t("themes.draftConflict")}</p>
        <Button
          type="button"
          onClick={() => {
            setDirty(false);
            draftBase = props.base;
            setDraft(tokenGroups(props.tokens));
          }}
        >
          {t("themes.loadLatestTokens")}
        </Button>
      </Show>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!conflict() && Object.keys(errors()).length === 0) save.mutate();
        }}
      >
        <fieldset class="mb-4">
          <legend class="mb-2 text-xs font-semibold uppercase text-muted-foreground">
            {t("themes.colors")}
          </legend>
          <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            <For each={Object.keys(draft().colors)}>
              {(k) => (
                <div class="flex items-end gap-2">
                  <input
                    type="color"
                    aria-label={t("themes.pick", { name: k })}
                    value={hexOf(draft().colors[k] ?? "") ?? "#000000"}
                    onInput={(e) => set("colors", k, e.currentTarget.value)}
                    class="h-control w-10 shrink-0 cursor-pointer rounded-md border border-input bg-card"
                  />
                  <TextField
                    class="grow"
                    label={k}
                    value={draft().colors[k] ?? ""}
                    onChange={(v) => set("colors", k, v)}
                    error={message(`colors.${k}`)}
                    inputClass="figures"
                  />
                </div>
              )}
            </For>
          </div>
        </fieldset>
        <fieldset class="mb-4">
          <legend class="mb-2 text-xs font-semibold uppercase text-muted-foreground">
            {t("themes.typography")}
          </legend>
          <div class="grid gap-3 sm:grid-cols-2">
            <For each={Object.keys(draft().fonts)}>
              {(k) => (
                <TextField
                  label={k}
                  value={draft().fonts[k] ?? ""}
                  onChange={(v) => set("fonts", k, v)}
                  error={message(`fonts.${k}`)}
                />
              )}
            </For>
          </div>
        </fieldset>
        <fieldset class="mb-4">
          <legend class="mb-2 text-xs font-semibold uppercase text-muted-foreground">
            {t("themes.radius")}
          </legend>
          <div class="grid gap-3 sm:grid-cols-4">
            <For each={Object.keys(draft().radius)}>
              {(k) => (
                <TextField
                  label={k}
                  value={draft().radius[k] ?? ""}
                  onChange={(v) => set("radius", k, v)}
                  error={message(`radius.${k}`)}
                  inputClass="figures"
                />
              )}
            </For>
          </div>
        </fieldset>
        <Button
          type="submit"
          variant="primary"
          loading={save.isPending}
          disabled={conflict() || Object.keys(errors()).length > 0}
        >
          {t("themes.saveTokens")}
        </Button>
      </form>
    </section>
  );
}
