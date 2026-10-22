import { Badge, Button, Card, EmptyState, linkClass, showToast } from "@platform/ui";
import { A, useNavigate } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { For, type JSX, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, locale, say, t } from "../i18n/index.ts";
import { templateLabel } from "../lib/analytics.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { readoutMessage, STATUS_TONE, stopMessage } from "../lib/experiments.ts";
import { detectionParams, measureText } from "../lib/insights.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Insight = Schemas["Insight"];

const day = (iso: string) =>
  new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeZone: "UTC" }).format(
    new Date(`${iso}T00:00:00Z`),
  );

const visits = (score: number) =>
  new Intl.NumberFormat(locale()).format(Math.max(1, Math.round(score)));

function Heading(props: { insight: Insight }) {
  const content = (): JSX.Element => {
    const d = props.insight.detection;
    switch (d.kind) {
      case "funnel_drop":
        return t(`insights.transitions.${d.transition}`);
      case "slow_page":
        return t("insights.slowHeading", {
          template: templateLabel(d.template, t),
          metric: d.metric,
        });
      case "low_add_to_cart": {
        const name = props.insight.product_name;
        return name ? (
          <A href={`/products/${d.product_id}`} class={linkClass}>
            {name}
          </A>
        ) : (
          t("insights.deletedProduct")
        );
      }
      case "zero_result_search":
        return t("insights.searchHeading", { query: d.query });
    }
  };
  return <>{content()}</>;
}

function InsightExperiment(props: { id: string; labelId: string }) {
  const detail = createQuery(() => ({
    queryKey: tenantKey("experiment", props.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/experiments/{id}", {
          params: { header: tenantHeader(), path: { id: props.id } },
        }),
      ),
  }));
  return (
    <section
      aria-labelledby={props.labelId}
      class="flex flex-col gap-2 rounded-md border border-border bg-subtle p-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <h3 id={props.labelId} class="text-sm font-semibold text-heading">
          {t("insights.experiment.title")}
        </h3>
        <Show when={detail.data?.experiment.status}>
          {(status) => (
            <Badge tone={STATUS_TONE[status()]}>{t(`experiments.status.${status()}`)}</Badge>
          )}
        </Show>
      </div>
      <Show when={detail.data} fallback={<QueryState query={detail}>{() => null}</QueryState>}>
        {(d) => (
          <>
            <p aria-live="polite" class="text-sm text-foreground">
              {say(readoutMessage(d(), locale()))}
            </p>
            <Show when={stopMessage(d().experiment, locale())}>
              {(m) => <p class="text-xs text-muted-foreground">{say(m())}</p>}
            </Show>
            <A href={`/experiments/${props.id}`} class={`${linkClass} w-fit text-sm`}>
              {t("insights.experiment.open")}
            </A>
          </>
        )}
      </Show>
    </section>
  );
}

function InsightCard(props: {
  insight: Insight;
  admin: boolean;
  dismissing: boolean;
  onDismiss: () => void;
  onOpenEditor?: (prompt: string, insightId: string) => void;
}) {
  const headingId = () => `insight-${props.insight.id}`;
  const d = () => props.insight.detection;
  return (
    <Card
      labelledBy={headingId()}
      title={<Heading insight={props.insight} />}
      actions={
        <>
          <Show when={props.onOpenEditor && props.insight.hypothesis}>
            {(h) => (
              <Button
                size="small"
                aria-describedby={headingId()}
                onClick={() => props.onOpenEditor?.(h().theme_prompt, props.insight.id)}
              >
                {t("insights.openInEditor")}
              </Button>
            )}
          </Show>
          <Button
            size="small"
            category="tertiary"
            aria-describedby={headingId()}
            loading={props.dismissing}
            onClick={() => props.onDismiss()}
          >
            {t("insights.dismiss")}
          </Button>
        </>
      }
    >
      <div class="flex flex-col gap-3">
        <p class="text-sm text-foreground">
          {t(`insights.${d().kind}`, detectionParams(d(), locale()))}
        </p>
        <p class="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
          <span class="figures">{t("insights.affected", { n: visits(props.insight.score) })}</span>
          <Show
            when={props.insight.status === "resolved"}
            fallback={
              <span>{t("insights.since", { date: day(props.insight.first_detected_on) })}</span>
            }
          >
            <Badge>{t("insights.resolved")}</Badge>
            <span>{t("insights.lastDetected", { date: day(props.insight.last_detected_on) })}</span>
          </Show>
          <Show when={props.insight.status === "open" && props.insight.reopened}>
            {(r) => (
              <>
                <Badge tone="warning">{t("insights.reopened")}</Badge>
                <span>
                  {t("insights.reopenedDetail", {
                    date: day(r().on),
                    n: visits(r().dismissed_score),
                  })}
                </span>
              </>
            )}
          </Show>
        </p>
        <Show when={props.insight.hypothesis}>
          {(h) => (
            <section
              aria-labelledby={`${headingId()}-ai`}
              class="flex flex-col gap-2 rounded-md border border-border bg-subtle p-3"
            >
              <div class="flex flex-wrap items-center gap-2">
                <h3 id={`${headingId()}-ai`} class="text-sm font-semibold text-heading">
                  {t("insights.hypothesis")}
                </h3>
                <Badge tone="info">{t("ai.aiLabel")}</Badge>
              </div>
              <p class="text-sm break-words whitespace-pre-wrap">{h().text}</p>
              <figure class="flex flex-col gap-1">
                <figcaption class="text-xs text-muted-foreground">{t("insights.draft")}</figcaption>
                <blockquote class="border-l-2 border-border pl-3 text-sm break-words whitespace-pre-wrap">
                  {h().theme_prompt}
                </blockquote>
              </figure>
              <p class="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                <span>{t("insights.written", { date: day(h().generated_at.slice(0, 10)) })}</span>
                <span>{t("insights.model", { model: h().model })}</span>
              </p>
            </section>
          )}
        </Show>
        <Show when={props.admin && props.insight.experiment_id}>
          {(id) => <InsightExperiment id={id()} labelId={`${headingId()}-experiment`} />}
        </Show>
        <Show when={props.insight.theme_change}>
          {(c) => (
            <section
              aria-labelledby={`${headingId()}-change`}
              class="flex flex-col gap-2 rounded-md border border-border bg-subtle p-3"
            >
              <h3 id={`${headingId()}-change`} class="text-sm font-semibold text-heading">
                {t("insights.change.title")}
              </h3>
              <p class="text-xs text-muted-foreground">
                {t("insights.change.published", {
                  number: c().revision_number,
                  date: day(c().published_at.slice(0, 10)),
                })}
              </p>
              <dl class="grid gap-3 sm:grid-cols-2">
                <For each={["before", "after"] as const}>
                  {(side) => (
                    <div class="flex flex-col gap-0.5">
                      <dt class="text-xs text-muted-foreground">
                        {t(`insights.change.${side}`, {
                          from: day(c()[side].from),
                          to: day(c()[side].to),
                        })}
                      </dt>
                      <dd class="figures text-sm text-heading">
                        <Show
                          when={c()[side].measure}
                          fallback={t("insights.change.collecting", { date: day(c()[side].to) })}
                        >
                          {(m) => measureText(d(), m(), locale(), t)}
                        </Show>
                      </dd>
                    </div>
                  )}
                </For>
              </dl>
            </section>
          )}
        </Show>
      </div>
    </Card>
  );
}

export default function Insights() {
  const { can } = useMembership();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const key = () => tenantKey("insights");
  const query = createQuery(() => ({
    queryKey: key(),
    queryFn: () => unwrap(api.GET("/admin/v1/insights", { params: { header: tenantHeader() } })),
  }));
  const dismiss = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/insights/{id}/dismiss", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: (_: Insight, id: string) => {
      qc.setQueryData<Schemas["InsightList"]>(key(), (old) =>
        old ? { items: old.items.filter((i) => i.id !== id) } : old,
      );
      showToast({ title: t("insights.dismissed"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) =>
      showToast({ title: errorMessage(e), tone: "error", closeLabel: t("common.close") }),
  }));
  const openEditor = (aiPrompt: string, insightId: string) =>
    navigate("/themes", { state: { aiPrompt, insightId } });

  return (
    <>
      <PageHeader title={t("insights.title")} description={t("insights.description")} />
      <QueryState query={query}>
        {(list) => (
          <Show
            when={list.items.length > 0}
            fallback={
              <Card aria-label={t("insights.list")} class="max-w-5xl">
                <EmptyState
                  icon="check-circle"
                  title={t("insights.empty")}
                  description={t("insights.emptyHint")}
                />
              </Card>
            }
          >
            <ul class="flex max-w-5xl flex-col gap-4" aria-label={t("insights.list")}>
              <For each={list.items}>
                {(insight) => (
                  <li>
                    <InsightCard
                      insight={insight}
                      admin={can("admin")}
                      dismissing={dismiss.isPending && dismiss.variables === insight.id}
                      onDismiss={() => dismiss.mutate(insight.id)}
                      onOpenEditor={can("admin") ? openEditor : undefined}
                    />
                  </li>
                )}
              </For>
            </ul>
          </Show>
        )}
      </QueryState>
    </>
  );
}
