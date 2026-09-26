/**
 * AI theme editing (WP24, spec §12.3): the merchant describes a change, an agent edits a copy of
 * the live theme, writes a functional check and runs the builder's gates (up to three repairs).
 * The run's progress, the agent's summary, the diff and the last check report are shown here;
 * accepting makes its revision publishable through the normal preview/publish flow above.
 */
import { Alert, Badge, Button, Card, Icon, linkClass, Spinner, TextField } from "@platform/ui";
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
  file: "font-semibold text-heading",
  hunk: "text-muted-foreground",
  add: "bg-success-50 text-success-700",
  del: "bg-error-50 text-error-700",
  ctx: "text-foreground",
} as const;

/** A unified diff, coloured per line (plain text only). */
export function DiffView(props: { diff: string; label: string }) {
  const lines = createMemo(() => diffLines(props.diff));
  return (
    // A scrollable region: focusable so keyboard users can scroll it.
    <section
      aria-label={props.label}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable region must be reachable by keyboard
      tabIndex={0}
      class="max-h-[32rem] overflow-auto rounded-md border border-border bg-background p-2"
    >
      <pre class="font-mono text-xs leading-5">
        <For each={lines()}>
          {(l) => (
            <div class={`${lineClass[l.kind]} whitespace-pre-wrap break-all`}>{l.text || " "}</div>
          )}
        </For>
      </pre>
    </section>
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
    // Native disclosure styled like the Pajamas Collapse (which cannot report its toggle yet).
    <details class="group text-sm" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary class="flex min-h-control cursor-pointer list-none items-center gap-1 rounded-md font-semibold text-heading [&::-webkit-details-marker]:hidden">
        <Icon
          name="chevron-right"
          class="text-muted-foreground transition-transform group-open:rotate-90"
        />
        {t("themes.ai.showChanges")}
      </summary>
      <div class="pt-2 pl-5">
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
    <Card
      labelledBy="ai-edit-h"
      class="mt-4 max-w-5xl"
      title={
        <span class="flex flex-wrap items-center gap-2">
          {t("themes.ai.title")}
          <Show when={runs.data?.provider === "fake"}>
            <Badge tone="info">{t("ai.demo")}</Badge>
          </Show>
        </span>
      }
      description={t("themes.ai.hint")}
    >
      <Show when={props.canEdit && runs.data?.provider !== "disabled"}>
        <form
          class="mb-4 flex flex-col gap-4"
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
              variant="confirm"
              loading={start.isPending}
              disabled={!prompt().trim() || busy()}
            >
              {t("themes.ai.start")}
            </Button>
          </div>
        </form>
      </Show>
      <Show when={error()}>
        <Alert tone="error" class="mb-4">
          <Show when={quotaExceeded()} fallback={errorMessage(error())}>
            {t("ai.quotaExceeded")}{" "}
            <A href="/settings/ai" class={linkClass}>
              {t("ai.seeUsage")}
            </A>
          </Show>
        </Alert>
      </Show>
      <QueryState query={runs}>
        {() => (
          <Show when={items().length}>
            <div class="overflow-x-auto rounded-md border border-border">
              <table class={tableClass}>
                <caption class="border-b border-border px-3 py-2 text-left text-sm font-semibold text-heading">
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
                      <tr
                        classList={{
                          "bg-subtle": r.id === current(),
                          "hover:bg-subtle": r.id !== current(),
                        }}
                      >
                        <td class={`${tdClass} max-w-md`}>
                          <button
                            type="button"
                            class="line-clamp-2 rounded-sm text-left font-semibold text-heading hover:text-accent-700 hover:underline aria-pressed:text-accent-700"
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
                        <td class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}>
                          {formatDateTime(r.created_at)}
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
        )}
      </QueryState>
      <Show when={current()}>
        <div class="mt-4 rounded-md border border-border p-4">
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
    </Card>
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
      <h3
        id="ai-run-h"
        class="flex flex-wrap items-center gap-2 text-base font-semibold text-heading"
      >
        <span class="break-words">{run().prompt}</span>
        <Badge tone={runTone(run().status)}>
          {t(`themes.ai.statuses.${run().status as "queued"}`)}
        </Badge>
      </h3>
      <p class="figures -mt-2 text-xs text-muted-foreground" aria-live="polite">
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
        <Alert tone="error">{run().error}</Alert>
      </Show>
      <Show when={props.detail.summary}>
        <div>
          <h4 class="mb-2 text-sm font-semibold text-heading">{t("themes.ai.summary")}</h4>
          <p class="text-sm whitespace-pre-wrap break-words">{props.detail.summary}</p>
        </div>
      </Show>
      <div>
        <h4 class="mb-2 text-sm font-semibold text-heading">{t("themes.ai.steps")}</h4>
        <Show
          when={props.detail.steps.length}
          fallback={<p class="text-sm text-muted-foreground">{t("themes.ai.noSteps")}</p>}
        >
          <ol class="flex flex-col gap-1.5 text-sm">
            <For each={props.detail.steps}>
              {(s) => (
                <li class="flex flex-wrap items-baseline gap-2">
                  <Badge tone={s.ok ? "neutral" : "error"}>
                    {t(`themes.ai.tools.${s.tool as "read_file"}`)}
                  </Badge>
                  <Show when={s.path}>
                    <code class="font-mono text-xs">{s.path}</code>
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
          <h4 class="mb-2 text-sm font-semibold text-heading">{t("themes.ai.checks")}</h4>
          <Show
            when={report().failures.length}
            fallback={
              <p class="flex items-center gap-1.5 text-sm text-success-700">
                <Icon name="check-circle" class="text-success-600" />
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
            <h4 class="mb-2 text-sm font-semibold text-heading">{t("themes.ai.diff")}</h4>
            <DiffView diff={diff()} label={t("themes.ai.diff")} />
          </div>
        )}
      </Show>
      <Show when={run().status === "accepted" && run().revision_id}>
        {(id) => (
          <Alert
            tone="success"
            actions={
              <Button onClick={() => props.onShowRevision(id())}>
                {t("themes.ai.showRevision", { number: run().revision_number ?? 0 })}
              </Button>
            }
          >
            {t("themes.ai.accepted", { number: run().revision_number ?? 0 })}
          </Alert>
        )}
      </Show>
      <Show when={run().status === "discarded"}>
        <p class="text-sm text-muted-foreground">{t("themes.ai.discarded")}</p>
      </Show>
      <Show when={props.canEdit}>
        <div class="flex flex-wrap gap-2 border-t border-border pt-4 empty:hidden">
          <Show when={active() && !run().cancel_requested}>
            <Button loading={props.pending} onClick={() => props.onAction("cancel")}>
              {t("themes.ai.cancel")}
            </Button>
          </Show>
          <Show when={run().status === "succeeded"}>
            <Button
              variant="confirm"
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
