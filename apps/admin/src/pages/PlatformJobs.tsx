import {
  Button,
  ConfirmDialog,
  Dialog,
  EmptyState,
  PermissionDenied,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, unwrap } from "../lib/api.ts";
import { useMe } from "../lib/me.ts";
import { claims } from "../lib/session.ts";

type Job = Schemas["JobInfo"];
const STATUSES = ["dead", "queued", "running", "done"] as const;
type Status = (typeof STATUSES)[number];

/** Superadmin view of the job queue across all tenants (spec §13). */
export default function PlatformJobs() {
  const qc = useQueryClient();
  const me = useMe();
  const allowed = () => me.data?.is_superadmin === true;
  const [status, setStatus] = createSignal<Status>("dead");
  const [kindInput, setKindInput] = createSignal("");
  const [kind, setKind] = createSignal("");
  const [payload, setPayload] = createSignal<Job | null>(null);
  const [requeueing, setRequeueing] = createSignal<Job | null>(null);

  // Debounce typing into the filter.
  createEffect(
    on(
      kindInput,
      (v) => {
        const timer = setTimeout(() => setKind(v.trim()), 300);
        onCleanup(() => clearTimeout(timer));
      },
      { defer: true },
    ),
  );

  const key = () => ["platform-jobs", claims()?.sub];
  const jobs = createInfiniteQuery(() => ({
    queryKey: [...key(), status(), kind()],
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/platform/jobs", {
          params: {
            query: { status: status(), kind: kind() || undefined, cursor: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: allowed(),
  }));
  const rows = () => jobs.data?.pages.flatMap((p) => p.items) ?? [];

  const requeue = createMutation(() => ({
    mutationFn: (id: number) =>
      unwrap(api.POST("/admin/v1/platform/jobs/{id}/requeue", { params: { path: { id } } })),
    onSuccess: async () => {
      setRequeueing(null);
      await qc.invalidateQueries({ queryKey: key() });
      showToast({ title: t("jobs.requeued"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setRequeueing(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  return (
    <>
      <PageHeader title={t("jobs.title")} description={t("jobs.lead")} />
      <QueryState query={me}>
        {() => (
          <Show
            when={allowed()}
            fallback={
              <PermissionDenied
                title={t("common.forbiddenTitle")}
                description={t("errors.not_a_superadmin")}
              />
            }
          >
            <div class="mb-4 flex flex-wrap items-start gap-3">
              <SelectField
                class="w-40"
                label={t("jobs.status")}
                value={status()}
                options={STATUSES.map((s) => ({ value: s, label: t(`jobs.statuses.${s}`) }))}
                onChange={(v) => setStatus(STATUSES.find((s) => s === v) ?? "dead")}
              />
              <TextField
                class="w-72 max-w-full"
                label={t("jobs.kind")}
                type="search"
                value={kindInput()}
                onChange={setKindInput}
                description={t("jobs.kindHint")}
                maxLength={100}
              />
            </div>
            <QueryState query={jobs}>
              {() => (
                <Show
                  when={rows().length > 0}
                  fallback={
                    <EmptyState title={t("jobs.empty")} description={t("jobs.emptyDesc")} />
                  }
                >
                  <div class="overflow-x-auto">
                    <table class={tableClass}>
                      <thead>
                        <tr>
                          <Th>{t("jobs.id")}</Th>
                          <Th>{t("jobs.kind")}</Th>
                          <Th>{t("jobs.tenant")}</Th>
                          <Th class="text-right">{t("jobs.attempts")}</Th>
                          <Th>{t("jobs.lastError")}</Th>
                          <Th>
                            {status() === "done" || status() === "dead"
                              ? t("jobs.finished")
                              : t("jobs.scheduled")}
                          </Th>
                          <Th srOnly>{t("common.actions")}</Th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={rows()}>
                          {(j) => (
                            <tr>
                              <td class={`${tdClass} figures text-xs`}>{j.id}</td>
                              <td class={`${tdClass} figures text-xs`}>
                                {j.kind}
                                <span class="block text-faint-foreground">{j.queue}</span>
                              </td>
                              <td
                                class={`${tdClass} figures max-w-40 truncate text-xs`}
                                title={j.tenant_id ?? ""}
                              >
                                {j.tenant_id ?? t("jobs.noTenant")}
                              </td>
                              <td class={`${tdClass} figures text-right text-xs`}>
                                {j.attempts}/{j.max_attempts}
                              </td>
                              <td
                                class={`${tdClass} max-w-72 truncate text-xs`}
                                title={j.last_error ?? ""}
                              >
                                {j.last_error ?? "—"}
                              </td>
                              <td
                                class={`${tdClass} text-xs whitespace-nowrap text-faint-foreground`}
                              >
                                {formatDateTime(j.finished_at ?? j.run_at)}
                              </td>
                              <td class={`${tdClass} text-right whitespace-nowrap`}>
                                <Button variant="ghost" onClick={() => setPayload(j)}>
                                  {t("jobs.payload")}
                                  <span class="sr-only">: {j.id}</span>
                                </Button>
                                <Show when={j.status === "dead"}>
                                  <Button variant="ghost" onClick={() => setRequeueing(j)}>
                                    {t("jobs.requeue")}
                                    <span class="sr-only">: {j.id}</span>
                                  </Button>
                                </Show>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                  <Show when={jobs.hasNextPage}>
                    <div class="mt-3">
                      <Button
                        loading={jobs.isFetchingNextPage}
                        onClick={() => void jobs.fetchNextPage()}
                      >
                        {t("common.loadMore")}
                      </Button>
                    </div>
                  </Show>
                </Show>
              )}
            </QueryState>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={payload() !== null}
        onOpenChange={(o) => !o && setPayload(null)}
        title={t("jobs.payloadTitle", { id: String(payload()?.id ?? "") })}
      >
        <pre class="figures max-h-96 overflow-auto rounded-sm bg-muted p-2 text-xs">
          {JSON.stringify(payload()?.payload, null, 2)}
        </pre>
        <div class="flex justify-end">
          <Button onClick={() => setPayload(null)}>{t("common.close")}</Button>
        </div>
      </Dialog>

      <ConfirmDialog
        open={requeueing() !== null}
        onOpenChange={(o) => !o && setRequeueing(null)}
        title={t("jobs.requeueTitle", { id: String(requeueing()?.id ?? "") })}
        description={t("jobs.requeueDesc")}
        confirmLabel={t("jobs.requeue")}
        cancelLabel={t("common.cancel")}
        pending={requeue.isPending}
        onConfirm={() => {
          const j = requeueing();
          if (j) requeue.mutate(j.id);
        }}
      />
    </>
  );
}
