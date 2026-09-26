import {
  Badge,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  linkClass,
  showToast,
  type Tone,
} from "@platform/ui";
import { A, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, type JSX, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import {
  asRunStatus,
  kindLabel,
  reasonLabel,
  runTone,
  sourceLabel,
  statusLabel,
} from "./Flows.tsx";

const stepTone: Record<string, Tone> = { sent: "success", skipped: "neutral", failed: "error" };
const stepLabel = (s: string) =>
  s === "sent" || s === "skipped" || s === "failed" ? t(`flows.stepStatus_${s}`) : s;

/** One flow run (WP19): its state, every processed email and, for admins, cancellation. */
export default function FlowRun() {
  const params = useParams();
  const qc = useQueryClient();
  const { can } = useMembership();
  const [confirming, setConfirming] = createSignal(false);
  const run = createQuery(() => ({
    queryKey: tenantKey("flow-runs", params.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/flows/runs/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
  }));
  const cancel = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/flows/runs/{id}/cancel", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
    onSuccess: async (detail) => {
      qc.setQueryData(tenantKey("flow-runs", params.id), detail);
      await qc.invalidateQueries({ queryKey: tenantKey("flow-runs") });
      setConfirming(false);
      showToast({ title: t("flows.cancelled"), closeLabel: t("common.close") });
    },
    onError: (err) =>
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") }),
  }));

  return (
    <>
      <PageHeader
        title={t("flows.runTitle")}
        back={{ href: "/marketing/flows", label: t("flows.title") }}
        actions={
          <Show when={can("admin") && run.data?.run.status === "active"}>
            <Button variant="danger" onClick={() => setConfirming(true)}>
              {t("flows.cancel")}
            </Button>
          </Show>
        }
      />
      <QueryState query={run}>
        {(data) => {
          const r = data.run;
          const status = asRunStatus(r.status);
          return (
            <div class="grid items-start gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
              <aside class="min-w-0 xl:order-last">
                <Card title={t("flows.flow")}>
                  <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm [&_dt]:text-muted-foreground">
                    <Row label={t("flows.flow")}>{kindLabel(r.kind)}</Row>
                    <Row label={t("flows.status")}>
                      <Badge tone={status ? runTone[status] : "neutral"}>
                        {statusLabel(r.status)}
                      </Badge>
                    </Row>
                    <Row label={t("flows.source")}>
                      {sourceLabel(r.source_kind)}{" "}
                      <Show when={r.source_kind === "order"}>
                        <A href={`/orders/${r.source_id}`} class={linkClass}>
                          {t("flows.viewOrder")}
                        </A>
                      </Show>
                    </Row>
                    <Show when={r.status === "active"}>
                      <Row label={t("flows.nextStep")}>
                        {t("flows.step", { n: String(r.next_step + 1) })}
                      </Row>
                      <Row label={t("flows.due")}>{formatDateTime(r.due_at)}</Row>
                    </Show>
                    <Show when={r.exit_reason}>
                      <Row label={t("flows.outcome")}>{reasonLabel(r.exit_reason)}</Row>
                    </Show>
                    <Show when={r.attempts > 0}>
                      <Row label={t("flows.attempts")}>{String(r.attempts)}</Row>
                    </Show>
                    <Show when={r.last_error}>
                      <Row label={t("flows.lastError")}>
                        <code class="font-mono text-xs break-all text-error-700">
                          {r.last_error}
                        </code>
                      </Row>
                    </Show>
                  </dl>
                </Card>
              </aside>
              <Card labelledBy="run-steps" class="min-w-0" padding="none" title={t("flows.steps")}>
                <Show
                  when={data.steps.length > 0}
                  fallback={<EmptyState icon="document" title={t("flows.noSteps")} />}
                >
                  <div class="overflow-x-auto">
                    <table class={tableClass}>
                      <thead>
                        <tr>
                          <Th>{t("flows.steps")}</Th>
                          <Th>{t("flows.status")}</Th>
                          <Th>{t("flows.outcome")}</Th>
                          <Th>{t("flows.executed")}</Th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={data.steps}>
                          {(s) => (
                            <tr>
                              <td class={`${tdClass} font-semibold text-heading`}>
                                {t("flows.step", { n: String(s.step_number + 1) })}
                              </td>
                              <td class={tdClass}>
                                <Badge tone={stepTone[s.status] ?? "neutral"}>
                                  {stepLabel(s.status)}
                                </Badge>
                              </td>
                              <td class={tdClass}>{reasonLabel(s.reason) || "—"}</td>
                              <td
                                class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}
                              >
                                {formatDateTime(s.executed_at)}
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </Card>
            </div>
          );
        }}
      </QueryState>
      <ConfirmDialog
        open={confirming()}
        onOpenChange={setConfirming}
        title={t("flows.cancelTitle")}
        description={t("flows.cancelDesc")}
        confirmLabel={t("flows.cancel")}
        cancelLabel={t("common.close")}
        pending={cancel.isPending}
        danger
        onConfirm={() => cancel.mutate()}
      />
    </>
  );
}

function Row(props: { label: string; children: JSX.Element }) {
  return (
    <>
      <dt>{props.label}</dt>
      <dd>{props.children}</dd>
    </>
  );
}
