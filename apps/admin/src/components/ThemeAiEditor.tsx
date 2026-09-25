/**
 * AI theme editing (WP24, spec §12.3): the merchant describes a change, an agent edits a copy of
 * the live theme, writes a functional check and runs the builder's gates (up to three repairs).
 * The run's progress, the agent's summary, the diff and the last check report are shown here;
 * accepting makes its revision publishable through the normal preview/publish flow above.
 */
import { Badge, Button, Spinner, TextField } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createMemo, createSignal, For, on, Show } from "solid-js";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { diffLines, parseReport, RUN_ACTIVE, runTone } from "../lib/themes.ts";
import { QueryState, Th, tableClass, tdClass } from "./Page.tsx";

type Run = Schemas["AiThemeRunSummary"];

const lineClass = {
  file: "font-semibold text-foreground",
  hunk: "text-muted-foreground",
  add: "bg-success-50 text-success-700",
  del: "bg-error-50 text-error-700",
  ctx: "text-foreground",
} as const;

/** A unified diff, coloured per line (plain text only). */
export function DiffView(props: { diff: string; label: string }) {
  const lines = createMemo(() => diffLines(props.diff));
  return (
    <pre
      class="max-h-[32rem] overflow-auto rounded-md border border-border bg-card p-2 text-xs leading-5"
      aria-label={props.label}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable region must be reachable by keyboard
      tabIndex={0}
    >
      <For each={lines()}>
        {(l) => <div class={`${lineClass[l.kind]} whitespace-pre-wrap break-all`}>{l.text || " "}</div>}
      </For>
    </pre>
  );
}

/** The changes of a revision against its parent, loaded when opened. */
export function RevisionChanges(props: { id: string }) {
  const [open, setOpen] = createSignal(false);
  const diff = createQuery(() => ({
    queryKey: tenantKey("theme-revision-diff", props.id),
    enabled: open(),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/themes/revisions/{id}/diff", {
          params: { header: tenantHeader(), path: { id: props.id } },
        }),
      ),
  }));
  return (
    <details class="text-sm" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary class="cursor-pointer text-accent-700 hover:underline">
        {t("themes.ai.showChanges")}
      </summary>
      <div class="mt-2">
        <Show when={open()}>
          <QueryState query={diff}>
            {(d) => (
              <Show
                when={d.diff}
                fallback={<p class="text-muted-foreground">{t("themes.ai.noDiff")}</p>}
              >
                <DiffView diff={d.diff} label={t("themes.ai.diff")} />
              </Show>
            )}
          </QueryState>
        </Show>
      </div>
    </details>
  );
}

export function ThemeAiEditor(props: { canEdit: boolean; onShowRevision: (id: string) => void }) {
  const qc = useQueryClient();
  const [prompt, setPrompt] = createSignal("");
  const [selected, setSelected] = createSignal<string>();
  const [error, setError] = createSignal<unknown>();

  const runs = createQuery(() => ({
    queryKey: tenantKey("theme-ai-runs"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/themes/ai-runs", { params: { header: tenantHeader() } })),
    refetchInterval: (q: { state: { data?: { items: Run[] } } }) =>
      q.state.data?.items.some((r) => RUN_ACTIVE.has(r.status)) ? 3000 : false,
  }));
  const items = () => runs.data?.items ?? [];
  const current = () => selected() ?? items()[0]?.id;
  const detail = createQuery(() => ({
    queryKey: tenantKey("theme-ai-run", current()),
    enabled: Boolean(current()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/themes/ai-runs/{id}", {
          params: { header: tenantHeader(), path: { id: current() ?? "" } },
        }),
      ),
    refetchInterval: (q: { state: { data?: Schemas["AiThemeRunDetail"] } }) =>
      RUN_ACTIVE.has(q.state.data?.run.status ?? "") ? 2000 : false,
  }));
  const busy = () => items().some((r) => RUN_ACTIVE.has(r.status));
  // Each check run creates a revision and a run's end changes the list: keep the table above
  // (which polls only while one of its own revisions is pending) in step.
  createEffect(
    on(
      () => [detail.data?.run.checks_run, detail.data?.run.status],
      () => void qc.invalidateQueries({ queryKey: tenantKey("theme-revisions") }),
      { defer: true },
    ),
  );
  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: tenantKey("theme-ai-runs") });
    await qc.invalidateQueries({ queryKey: tenantKey("theme-ai-run") });
    await qc.invalidateQueries({ queryKey: tenantKey("theme-revisions") });
  };

  const start = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/themes/ai-runs", {
          params: { header: tenantHeader() },
          body: { prompt: prompt().trim() },
        }),
      ),
    onMutate: () => setError(undefined),
    onSuccess: async (r: Run) => {
      setPrompt("");
      setSelected(r.id);
      await refresh();
    },
    onError: (e: unknown) => setError(e),
  }));
  const act = createMutation(() => ({
    mutationFn: (v: { id: string; action: "cancel" | "accept" | "discard" }) => {
      const opts = { params: { header: tenantHeader(), path: { id: v.id } } };
      if (v.action === "cancel")
        return unwrap(api.POST("/admin/v1/themes/ai-runs/{id}/cancel", opts));
      if (v.action === "accept")
        return unwrap(api.POST("/admin/v1/themes/ai-runs/{id}/accept", opts));
      return unwrap(api.POST("/admin/v1/themes/ai-runs/{id}/discard", opts));
    },
    onMutate: () => setError(undefined),
    onSuccess: refresh,
    onError: (e: unknown) => setError(e),
  }));
  const quotaExceeded = () => {
    const e = error();
    return e instanceof ApiError && e.code === "ai_quota_exceeded";
  };

  return (
    <section aria-labelledby="ai-edit-h" class="mt-10 max-w-5xl">
      <h2 id="ai-edit-h" class="flex flex-wrap items-center gap-2 text-sm font-semibold">
        {t("themes.ai.title")}
        <Show when={runs.data?.provider === "fake"}>
          <Badge tone="info">{t("ai.demo")}</Badge>
        </Show>
      </h2>
      <p class="mb-3 text-sm text-muted-foreground">{t("themes.ai.hint")}</p>
      <Show when={props.canEdit && runs.data?.provider !== "disabled"}>
        <form
          class="mb-4 flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (prompt().trim() && !busy()) start.mutate();
          }}
        >
          <TextField
            label={t("themes.ai.prompt")}
            description={t("themes.ai.promptHint")}
            multiline
            rows={3}
            maxLength={4000}
            value={prompt()}
            onChange={setPrompt}
            disabled={start.isPending}
          />
          <div>
            <Button
              type="submit"
              variant="primary"
              loading={start.isPending}
              disabled={!prompt().trim() || busy()}
            >
              {t("themes.ai.start")}
            </Button>
          </div>
        </form>
      </Show>
      <Show when={error()}>
        <div role="alert" class="mb-4 rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
          <Show when={quotaExceeded()} fallback={errorMessage(error())}>
            {t("ai.quotaExceeded")}{" "}
            <A href="/settings/ai" class="underline">
              {t("ai.seeUsage")}
            </A>
          </Show>
        </div>
      </Show>
      <QueryState query={runs}>
        {() => (
      <Show when={items().length}>
        <table class={tableClass}>
          <caption class="mb-1 text-left text-xs text-muted-foreground">
            {t("themes.ai.runs")}
          </caption>
          <thead>
            <tr>
              <Th>{t("themes.ai.request")}</Th>
              <Th>{t("themes.status")}</Th>
              <Th>{t("themes.created")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={items()}>
              {(r) => (
                <tr classList={{ "bg-muted": r.id === current() }}>
                  <td class={`${tdClass} max-w-md`}>
                    <button
                      type="button"
                      class="line-clamp-2 text-left text-accent-700 hover:underline"
                      aria-pressed={r.id === current()}
                      onClick={() => setSelected(r.id)}
                    >
                      {r.prompt}
                    </button>
                  </td>
                  <td class={tdClass}>
                    <Badge tone={runTone(r.status)}>
                      {t(`themes.ai.statuses.${r.status as "queued"}`)}
                    </Badge>
                  </td>
                  <td class={`${tdClass} text-muted-foreground`}>{formatDateTime(r.created_at)}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
        )}
      </QueryState>
      <Show when={current()}>
        <div class="mt-6">
          <QueryState query={detail}>
            {(d) => (
              <RunView
                detail={d}
                canEdit={props.canEdit}
                pending={act.isPending}
                onAction={(action) => act.mutate({ id: d.run.id, action })}
                onShowRevision={props.onShowRevision}
              />
            )}
          </QueryState>
        </div>
      </Show>
    </section>
  );
}

function RunView(props: {
  detail: Schemas["AiThemeRunDetail"];
  canEdit: boolean;
  pending: boolean;
  onAction: (a: "cancel" | "accept" | "discard") => void;
  onShowRevision: (id: string) => void;
}) {
  const run = () => props.detail.run;
  const report = createMemo(() => parseReport(props.detail.report ?? {}));
  const active = () => RUN_ACTIVE.has(run().status);
  return (
    <article aria-labelledby="ai-run-h" class="flex flex-col gap-4">
      <h3 id="ai-run-h" class="flex flex-wrap items-center gap-2 text-sm font-semibold">
        <span class="break-words">{run().prompt}</span>
        <Badge tone={runTone(run().status)}>
          {t(`themes.ai.statuses.${run().status as "queued"}`)}
        </Badge>
      </h3>
      <p class="text-xs text-muted-foreground" aria-live="polite">
        {t("themes.ai.progress", {
          turns: run().turns,
          maxTurns: props.detail.limits.max_turns,
          checks: run().checks_run,
          maxChecks: props.detail.limits.max_checks,
        })}
        {" · "}
        {t("themes.ai.usage", { tokens: run().tokens })}
      </p>
      <Show when={active()}>
        <p role="status" class="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner size="sm" />
          {t("themes.ai.working")}
        </p>
      </Show>
      <Show when={run().error}>
        <div role="alert" class="rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
          {run().error}
        </div>
      </Show>
      <Show when={props.detail.summary}>
        <div>
          <h4 class="mb-1 text-xs font-semibold uppercase text-muted-foreground">
            {t("themes.ai.summary")}
          </h4>
          <p class="text-sm whitespace-pre-wrap break-words">{props.detail.summary}</p>
        </div>
      </Show>
      <div>
        <h4 class="mb-1 text-xs font-semibold uppercase text-muted-foreground">
          {t("themes.ai.steps")}
        </h4>
        <Show
          when={props.detail.steps.length}
          fallback={<p class="text-sm text-muted-foreground">{t("themes.ai.noSteps")}</p>}
        >
          <ol class="flex flex-col gap-1 text-sm">
            <For each={props.detail.steps}>
              {(s) => (
                <li class="flex flex-wrap items-baseline gap-2">
                  <Badge tone={s.ok ? "neutral" : "error"}>
                    {t(`themes.ai.tools.${s.tool as "read_file"}`)}
                  </Badge>
                  <Show when={s.path}>
                    <code class="text-xs">{s.path}</code>
                  </Show>
                  <Show when={!s.ok || s.tool === "run_checks"}>
                    <span
                      class="text-xs break-all"
                      classList={{ "text-error-700": !s.ok, "text-muted-foreground": s.ok }}
                    >
                      {s.detail}
                    </span>
                  </Show>
                </li>
              )}
            </For>
          </ol>
        </Show>
      </div>
      <Show when={props.detail.report}>
        <div>
          <h4 class="mb-1 text-xs font-semibold uppercase text-muted-foreground">
            {t("themes.ai.checks")}
          </h4>
          <Show
            when={report().failures.length}
            fallback={
              <p class="text-sm text-success-700">
                {t("themes.ai.checksPassed", { number: run().revision_number ?? 0 })}
              </p>
            }
          >
            <ul class="list-disc pl-5 text-sm text-error-700">
              <For each={report().failures}>
                {(f) => <li class="break-words whitespace-pre-wrap">{f}</li>}
              </For>
            </ul>
          </Show>
        </div>
      </Show>
      <Show when={props.detail.diff}>
        {(diff) => (
          <div>
            <h4 class="mb-1 text-xs font-semibold uppercase text-muted-foreground">
              {t("themes.ai.diff")}
            </h4>
            <DiffView diff={diff()} label={t("themes.ai.diff")} />
          </div>
        )}
      </Show>
      <Show when={run().status === "accepted" && run().revision_id}>
        {(id) => (
          <p role="status" class="flex flex-wrap items-center gap-2 text-sm text-success-700">
            {t("themes.ai.accepted", { number: run().revision_number ?? 0 })}
            <Button variant="ghost" onClick={() => props.onShowRevision(id())}>
              {t("themes.ai.showRevision", { number: run().revision_number ?? 0 })}
            </Button>
          </p>
        )}
      </Show>
      <Show when={run().status === "discarded"}>
        <p class="text-sm text-muted-foreground">{t("themes.ai.discarded")}</p>
      </Show>
      <Show when={props.canEdit}>
        <div class="flex flex-wrap gap-2">
          <Show when={active() && !run().cancel_requested}>
            <Button loading={props.pending} onClick={() => props.onAction("cancel")}>
              {t("themes.ai.cancel")}
            </Button>
          </Show>
          <Show when={run().status === "succeeded"}>
            <Button
              variant="primary"
              loading={props.pending}
              onClick={() => props.onAction("accept")}
            >
              {t("themes.ai.accept")}
            </Button>
            <Button loading={props.pending} onClick={() => props.onAction("discard")}>
              {t("themes.ai.discard")}
            </Button>
          </Show>
        </div>
      </Show>
    </article>
  );
}
