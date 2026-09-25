/**
 * Bulk edit by prompt (§12.2): the staff describes a change, the AI returns a plan over
 * allowlisted operations, the API resolves the products and shows a count + preview (the dry
 * run), and only a confirmation applies it (a job with progress).
 */
import { Badge, Button, ConfirmDialog, Spinner, TextField } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Match, Show, Switch } from "solid-js";
import { PageHeader, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, t } from "../i18n/index.ts";
import { minorToMajor, type Operation, useAiUsage, useBulkPlan } from "../lib/ai.ts";
import { ApiError, api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

function describe(op: Operation): string {
  switch (op.op) {
    case "set_field":
      return t("ai.op_set_field", {
        field: t(`ai.field_${op.field}`),
        locale: op.locale ? ` (${op.locale.toUpperCase()})` : "",
        value: op.value,
      });
    case "adjust_price":
      return op.percent !== null && op.percent !== undefined
        ? t("ai.op_adjust_percent", {
            market: op.market.toUpperCase(),
            percent: `${op.percent > 0 ? "+" : ""}${op.percent}`,
          })
        : t("ai.op_adjust_amount", {
            market: op.market.toUpperCase(),
            amount: minorToMajor(op.amount_minor ?? 0),
          });
    case "add_category":
      return t("ai.op_add_category", { category: op.category });
    case "remove_category":
      return t("ai.op_remove_category", { category: op.category });
    case "set_parameter":
      return t("ai.op_set_parameter", { parameter: op.parameter, value: op.value });
    case "set_status":
      return t("ai.op_set_status", { status: t(`status.${op.status}`) });
  }
}

function selection(s: Schemas["Selector"]): string[] {
  const out: string[] = [];
  if (s.categories?.length) out.push(t("ai.sel_categories", { list: s.categories.join(", ") }));
  if (s.brands?.length) out.push(t("ai.sel_brands", { list: s.brands.join(", ") }));
  if (s.statuses?.length)
    out.push(t("ai.sel_statuses", { list: s.statuses.map((x) => t(`status.${x}`)).join(", ") }));
  for (const p of s.parameters ?? [])
    out.push(t("ai.sel_parameter", { parameter: p.parameter, value: p.value }));
  if (s.price)
    out.push(
      t("ai.sel_price", {
        market: s.price.market.toUpperCase(),
        min: s.price.min_minor != null ? minorToMajor(s.price.min_minor) : "–",
        max: s.price.max_minor != null ? minorToMajor(s.price.max_minor) : "–",
      }),
    );
  return out.length ? out : [t("ai.sel_all")];
}

export default function AiBulkEdit() {
  const qc = useQueryClient();
  const usage = useAiUsage();
  const [prompt, setPrompt] = createSignal("");
  const [planId, setPlanId] = createSignal<string | null>(null);
  const [error, setError] = createSignal<unknown>();
  const [confirming, setConfirming] = createSignal(false);
  const plan = useBulkPlan(planId);
  const applying = submission();

  const create = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/ai/bulk-plans", {
          params: { header: tenantHeader() },
          body: { prompt: prompt().trim() },
        }),
      ),
    onMutate: () => setError(undefined),
    onSuccess: (p) => {
      qc.setQueryData(tenantKey("ai-plan", p.id), p);
      setPlanId(p.id);
    },
    onError: (e: unknown) => setError(e),
    onSettled: () => void qc.invalidateQueries({ queryKey: tenantKey("ai-usage") }),
  }));

  const apply = createMutation(() => ({
    mutationFn: () => {
      const id = planId() ?? "";
      return unwrap(
        api.POST("/admin/v1/ai/bulk-plans/{id}/apply", {
          params: { header: applying.header({ apply: id }), path: { id } },
        }),
      );
    },
    onMutate: () => setError(undefined),
    onSuccess: (p) => {
      applying.done();
      setConfirming(false);
      qc.setQueryData(tenantKey("ai-plan", p.id), p);
      void plan.refetch();
    },
    onError: (e: unknown) => {
      setConfirming(false);
      setError(e);
    },
  }));

  const quotaExceeded = () =>
    error() instanceof ApiError && (error() as ApiError).code === "ai_quota_exceeded";
  const planning = () => create.isPending || plan.data?.status === "pending";
  const reset = () => {
    setPlanId(null);
    setError(undefined);
  };

  return (
    <>
      <PageHeader
        title={t("ai.bulkTitle")}
        description={t("ai.bulkDesc")}
        actions={
          <Show when={usage.data?.provider === "fake"}>
            <Badge tone="info">{t("ai.demo")}</Badge>
          </Show>
        }
      />
      <div class="flex max-w-4xl flex-col gap-5">
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (!planning() && prompt().trim()) create.mutate();
          }}
        >
          <TextField
            label={t("ai.prompt")}
            description={t("ai.promptHint")}
            multiline
            rows={3}
            maxLength={2000}
            value={prompt()}
            onChange={setPrompt}
            disabled={planning()}
          />
          <div class="flex flex-wrap gap-2">
            <Button
              type="submit"
              variant="primary"
              loading={planning()}
              disabled={!prompt().trim()}
            >
              {t("ai.plan")}
            </Button>
            <Show when={planId()}>
              <Button onClick={reset}>{t("ai.newPlan")}</Button>
            </Show>
          </div>
        </form>

        <div aria-live="polite">
          <Show when={planning()}>
            <p role="status" class="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner size="sm" />
              {t("ai.planning")}
            </p>
          </Show>
        </div>

        <Show when={error()}>
          <div role="alert" class="rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
            <Show when={quotaExceeded()} fallback={errorMessage(error())}>
              {t("ai.quotaExceeded")}{" "}
              <A href="/settings/ai" class="underline">
                {t("ai.seeUsage")}
              </A>
            </Show>
          </div>
        </Show>

        <Show when={plan.data && plan.data.status !== "pending" ? plan.data : undefined}>
          {(p) => (
            <section aria-labelledby="plan-h" class="flex flex-col gap-4">
              <h2 id="plan-h" class="text-base font-semibold">
                {t("ai.explanation")}
              </h2>
              <Show when={p().plan}>
                {(pl) => (
                  <div class="grid gap-4 md:grid-cols-2">
                    <div class="flex flex-col gap-1">
                      <p class="text-sm">{pl().explanation}</p>
                      <h3 class="mt-2 text-sm font-medium">{t("ai.operations")}</h3>
                      <ul class="ml-4 list-disc text-sm">
                        <For each={pl().operations}>{(op) => <li>{describe(op)}</li>}</For>
                      </ul>
                    </div>
                    <div class="flex flex-col gap-1">
                      <h3 class="text-sm font-medium">{t("ai.selection")}</h3>
                      <ul class="ml-4 list-disc text-sm">
                        <For each={selection(pl().selector)}>{(s) => <li>{s}</li>}</For>
                      </ul>
                    </div>
                  </div>
                )}
              </Show>

              <Switch>
                <Match when={p().status === "rejected" || p().status === "failed"}>
                  <div role="alert" class="rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
                    <p class="font-medium">{t("ai.rejected")}</p>
                    <ul class="ml-4 list-disc">
                      <For each={p().errors}>
                        {(e) => (
                          <li>{e.startsWith("ai_") ? errorMessage(new ApiError(422, e)) : e}</li>
                        )}
                      </For>
                    </ul>
                  </div>
                </Match>
                <Match when={p().status === "ready"}>
                  <p class="text-sm font-medium">{t("ai.matching", { count: p().target_count })}</p>
                  <div class="overflow-x-auto">
                    <table class={tableClass} aria-label={t("ai.preview")}>
                      <caption class="mb-1 text-left text-xs text-muted-foreground">
                        {t("ai.preview")}
                      </caption>
                      <thead>
                        <tr>
                          <Th>{t("ai.product")}</Th>
                          <Th>{t("ai.what")}</Th>
                          <Th>{t("ai.before")}</Th>
                          <Th>{t("ai.after")}</Th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={p().sample}>
                          {(row) => (
                            <For each={row.changes}>
                              {(c, i) => (
                                <tr>
                                  <td class={tdClass}>
                                    <Show when={i() === 0}>
                                      <A
                                        href={`/products/${row.product_id}`}
                                        class="hover:underline"
                                      >
                                        {row.name}
                                      </A>
                                    </Show>
                                  </td>
                                  <td class={tdClass}>{c.what}</td>
                                  <td class={`${tdClass} figures text-muted-foreground`}>
                                    {c.before || "–"}
                                  </td>
                                  <td class={`${tdClass} figures font-medium`}>{c.after}</td>
                                </tr>
                              )}
                            </For>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                  <Show when={p().needs_fresh_auth}>
                    <p class="text-xs text-muted-foreground">{t("ai.freshAuthHint")}</p>
                  </Show>
                  <div>
                    <Button variant="primary" onClick={() => setConfirming(true)}>
                      {t("ai.apply", { count: p().target_count })}
                    </Button>
                  </div>
                </Match>
                <Match when={p().status === "applying"}>
                  <div class="flex flex-col gap-1" role="status">
                    <p class="text-sm">
                      {t("ai.applying", {
                        done: (p().progress.done ?? 0) + (p().progress.skipped ?? 0),
                        total: p().progress.total ?? p().target_count,
                      })}
                    </p>
                    <progress
                      class="h-2 w-full max-w-md accent-accent-600"
                      max={p().progress.total || 1}
                      value={(p().progress.done ?? 0) + (p().progress.skipped ?? 0)}
                      aria-label={t("ai.progress")}
                    />
                  </div>
                </Match>
                <Match when={p().status === "applied"}>
                  <p
                    role="status"
                    class="rounded-md bg-success-50 px-3 py-2 text-sm text-success-700"
                  >
                    {t("ai.applied", {
                      done: p().progress.done ?? 0,
                      skipped: p().progress.skipped ?? 0,
                    })}
                  </p>
                </Match>
              </Switch>
            </section>
          )}
        </Show>
      </div>
      <ConfirmDialog
        open={confirming()}
        onOpenChange={setConfirming}
        title={t("ai.confirmTitle", { count: plan.data?.target_count ?? 0 })}
        description={t("ai.confirmDesc")}
        confirmLabel={t("ai.apply", { count: plan.data?.target_count ?? 0 })}
        cancelLabel={t("common.cancel")}
        pending={apply.isPending}
        onConfirm={() => apply.mutate()}
      />
    </>
  );
}
