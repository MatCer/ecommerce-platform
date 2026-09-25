import { Badge, Button, Checkbox, EmptyState, showToast, TextField, type Tone } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Index, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { flowConfig, stepCount } from "../lib/flows.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Definition = Schemas["FlowDefinition"];
type Run = Schemas["FlowRun"];
type Kind = "abandoned_cart" | "watchdog" | "review_invite";
const KINDS: Kind[] = ["abandoned_cart", "watchdog", "review_invite"];
const isKind = (k: string): k is Kind => (KINDS as string[]).includes(k);

export type RunStatus = "active" | "completed" | "cancelled" | "failed";
const RUN_STATUSES: RunStatus[] = ["active", "completed", "cancelled", "failed"];
export const runTone: Record<RunStatus, Tone> = {
  active: "info",
  completed: "success",
  cancelled: "neutral",
  failed: "error",
};
export const asRunStatus = (s: string): RunStatus | undefined => RUN_STATUSES.find((x) => x === s);

/** Flow and trigger labels; unknown values (a newer API) show their raw code. */
export function kindLabel(kind: string): string {
  return isKind(kind) ? t(`flows.kind_${kind}`) : kind;
}
export function sourceLabel(source: string): string {
  return source === "cart" || source === "order" || source === "watch"
    ? t(`flows.source_${source}`)
    : source;
}
export function statusLabel(status: string): string {
  const s = asRunStatus(status);
  return s ? t(`flows.status_${s}`) : status;
}

const REASONS = [
  "manual",
  "all_steps",
  "order_placed",
  "unsubscribed",
  "consent_withdrawn",
  "cart_closed",
  "cart_missing",
  "disabled",
  "flow_disabled",
  "flow_cancelled",
  "schedule_exhausted",
  "order_missing",
  "order_not_delivered",
  "order_closed",
  "retry_exhausted",
  "fired",
  "watch_unsubscribed",
  "source_missing",
  "review_invites",
] as const;
type Reason = (typeof REASONS)[number];
export function reasonLabel(reason: string | null | undefined): string {
  if (!reason) return "";
  const known = REASONS.find((r): r is Reason => r === reason);
  return known ? t(`flows.reason_${known}`) : reason;
}

const toastError = (err: unknown) =>
  showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

/**
 * Automated emails (WP19): per-flow settings (admins), the dev-only test clock and the latest
 * runs. Staff see everything read-only.
 */
export default function Flows() {
  const { can } = useMembership();
  const flows = createQuery(() => ({
    queryKey: tenantKey("flows"),
    queryFn: () => unwrap(api.GET("/admin/v1/flows", { params: { header: tenantHeader() } })),
  }));
  const runs = createQuery(() => ({
    queryKey: tenantKey("flow-runs"),
    queryFn: () => unwrap(api.GET("/admin/v1/flows/runs", { params: { header: tenantHeader() } })),
  }));

  return (
    <>
      <PageHeader title={t("flows.title")} description={t("flows.description")} />
      <p class="mb-4 max-w-prose text-xs text-muted-foreground">{t("flows.consentNote")}</p>
      <QueryState query={flows}>
        {(data) => (
          <>
            <Show when={!can("admin")}>
              <p class="mb-3 text-sm text-muted-foreground">{t("flows.readOnly")}</p>
            </Show>
            <div class="grid gap-3 lg:grid-cols-3">
              <For each={data.items.filter((d) => isKind(d.kind))}>
                {(d) => <FlowCard definition={d} editable={can("admin")} />}
              </For>
            </div>
            <Show when={data.test_clock_now}>
              {(now) => <TestClock now={now()} editable={can("admin")} />}
            </Show>
          </>
        )}
      </QueryState>

      <section aria-labelledby="flow-runs" class="mt-8">
        <h2 id="flow-runs" class="text-base font-semibold">
          {t("flows.runsTitle")}
        </h2>
        <p class="mb-3 text-sm text-muted-foreground">{t("flows.runsDesc")}</p>
        <QueryState query={runs}>
          {(data) => (
            <Show
              when={data.items.length > 0}
              fallback={
                <EmptyState title={t("flows.emptyRuns")} description={t("flows.emptyRunsDesc")} />
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("flows.flow")}</Th>
                      <Th>{t("flows.source")}</Th>
                      <Th>{t("flows.status")}</Th>
                      <Th>{t("flows.due")}</Th>
                      <Th>{t("flows.outcome")}</Th>
                      <Th srOnly>{t("flows.open")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>{(r) => <RunRow run={r} />}</For>
                  </tbody>
                </table>
              </div>
            </Show>
          )}
        </QueryState>
      </section>
    </>
  );
}

function RunRow(props: { run: Run }) {
  const r = () => props.run;
  const status = () => asRunStatus(r().status);
  return (
    <tr>
      <td class={tdClass}>{kindLabel(r().kind)}</td>
      <td class={tdClass}>{sourceLabel(r().source_kind)}</td>
      <td class={tdClass}>
        <Badge tone={status() ? runTone[status() as RunStatus] : "neutral"}>
          {statusLabel(r().status)}
        </Badge>
      </td>
      <td class={`${tdClass} whitespace-nowrap`}>
        {r().status === "active" ? formatDateTime(r().due_at) : "—"}
      </td>
      <td class={tdClass}>{reasonLabel(r().exit_reason) || "—"}</td>
      <td class={`${tdClass} text-right`}>
        <A href={`/marketing/flows/runs/${r().id}`} class="text-accent-700 underline">
          {t("flows.open")}
          <span class="sr-only">
            : {kindLabel(r().kind)}, {formatDateTime(r().due_at)}
          </span>
        </A>
      </td>
    </tr>
  );
}

function FlowCard(props: { definition: Definition; editable: boolean }) {
  const qc = useQueryClient();
  const d = () => props.definition;
  const kind = () => d().kind as Kind;
  const [enabled, setEnabled] = createSignal(d().enabled);
  const [delays, setDelays] = createSignal<string[]>(
    Array.from(
      { length: stepCount(kind()) },
      (_, i) => d().config.delays_hours[i]?.toString() ?? "",
    ),
  );
  const [couponOn, setCouponOn] = createSignal(d().config.coupon_percent != null);
  const [coupon, setCoupon] = createSignal(String(d().config.coupon_percent ?? 10));
  const [error, setError] = createSignal<"flows.errDelays" | "flows.errCoupon" | null>(null);

  const save = createMutation(() => ({
    mutationFn: (body: Schemas["FlowDefinitionChange"]) =>
      unwrap(
        api.PUT("/admin/v1/flows/{kind}", {
          params: { header: tenantHeader(), path: { kind: kind() } },
          body,
        }),
      ),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: tenantKey("flows") });
      showToast({
        title: t("flows.saved", { flow: kindLabel(kind()) }),
        closeLabel: t("common.close"),
      });
    },
    onError: toastError,
  }));

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    // The watchdog has no schedule to edit: it keeps its stored delay.
    const config =
      kind() === "watchdog"
        ? d().config
        : flowConfig(kind(), delays(), kind() === "abandoned_cart" && couponOn() ? coupon() : null);
    if (typeof config === "string") {
      setError(config);
      return;
    }
    setError(null);
    save.mutate({ enabled: enabled(), config });
  };
  const id = () => `flow-${kind()}`;

  return (
    <form
      class="grid content-start gap-3 rounded-lg border border-border bg-card p-4"
      aria-labelledby={id()}
      onSubmit={submit}
      noValidate
    >
      <div class="flex flex-wrap items-center justify-between gap-2">
        <h2 id={id()} class="font-semibold">
          {kindLabel(kind())}
        </h2>
        <Badge tone={d().enabled ? "success" : "neutral"}>
          {d().enabled ? t("flows.enabled") : t("flows.disabled")}
        </Badge>
      </div>
      <p class="text-sm text-muted-foreground">{t(`flows.desc_${kind()}`)}</p>
      <Checkbox
        label={t("flows.enabled")}
        checked={enabled()}
        onChange={setEnabled}
        disabled={!props.editable}
      />
      <Show when={kind() !== "watchdog"}>
        <fieldset class="grid gap-2" aria-describedby={error() ? `${id()}-err` : undefined}>
          <legend class="sr-only">{kindLabel(kind())}</legend>
          <Index each={delays()}>
            {(value, i) => (
              <TextField
                label={
                  kind() === "abandoned_cart"
                    ? t("flows.stepDelay", { n: String(i + 1) })
                    : t("flows.reviewDelay")
                }
                description={
                  i === delays().length - 1
                    ? kind() === "abandoned_cart"
                      ? t("flows.cartDelayHint")
                      : t("flows.reviewDelayHint")
                    : undefined
                }
                value={value()}
                onChange={(v) => setDelays(delays().map((x, j) => (j === i ? v : x)))}
                inputMode="numeric"
                maxLength={4}
                disabled={!props.editable}
              />
            )}
          </Index>
        </fieldset>
      </Show>
      <Show when={kind() === "abandoned_cart"}>
        <Checkbox
          label={t("flows.coupon")}
          checked={couponOn()}
          onChange={setCouponOn}
          disabled={!props.editable}
        />
        <Show when={couponOn()}>
          <TextField
            label={t("flows.couponPercent")}
            description={t("flows.couponHint")}
            value={coupon()}
            onChange={setCoupon}
            inputMode="numeric"
            maxLength={2}
            disabled={!props.editable}
            error={error() === "flows.errCoupon" ? t("flows.errCoupon") : undefined}
          />
        </Show>
      </Show>
      <Show when={error() === "flows.errDelays"}>
        <p id={`${id()}-err`} role="alert" class="text-xs font-medium text-error-700">
          {t("flows.errDelays")}
        </p>
      </Show>
      <Show when={props.editable}>
        <div>
          <Button type="submit" variant="primary" loading={save.isPending}>
            {t("flows.save")}
            <span class="sr-only">: {kindLabel(kind())}</span>
          </Button>
        </div>
      </Show>
    </form>
  );
}

function TestClock(props: { now: string; editable: boolean }) {
  const qc = useQueryClient();
  const [hours, setHours] = createSignal("1");
  const [invalid, setInvalid] = createSignal(false);
  const advance = createMutation(() => ({
    mutationFn: (h: number) =>
      unwrap(
        api.POST("/admin/v1/flows/test-clock/advance", {
          params: { header: tenantHeader() },
          body: { hours: h },
        }),
      ),
    onSuccess: async (r) => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: tenantKey("flows") }),
        qc.invalidateQueries({ queryKey: tenantKey("flow-runs") }),
      ]);
      showToast({
        title: t("flows.clockAdvanced", { time: formatDateTime(r.now) }),
        closeLabel: t("common.close"),
      });
    },
    onError: toastError,
  }));
  return (
    <section
      aria-labelledby="flow-clock"
      class="mt-4 grid gap-2 rounded-lg border border-dashed border-border-strong bg-warning-50 p-4"
    >
      <h2 id="flow-clock" class="font-semibold">
        {t("flows.clockTitle")}
      </h2>
      <p class="max-w-prose text-sm">{t("flows.clockDesc")}</p>
      <p class="text-sm font-medium" data-testid="flow-clock-now">
        {t("flows.clockNow", { time: formatDateTime(props.now) })}
      </p>
      <Show when={props.editable}>
        <form
          class="flex flex-wrap items-end gap-2"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            const h = hours().trim();
            const ok = /^\d{1,4}$/.test(h) && Number(h) >= 1 && Number(h) <= 2160;
            setInvalid(!ok);
            if (ok) advance.mutate(Number(h));
          }}
        >
          <TextField
            label={t("flows.clockHours")}
            value={hours()}
            onChange={setHours}
            inputMode="numeric"
            maxLength={4}
            class="w-40"
            error={invalid() ? t("flows.errHours") : undefined}
          />
          <Button type="submit" loading={advance.isPending}>
            {t("flows.clockAdvance")}
          </Button>
        </form>
      </Show>
    </section>
  );
}
