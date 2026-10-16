import { Badge, Button, Card, EmptyState, linkClass, showToast } from "@platform/ui";
import { A, useNavigate } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { For, type JSX, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, locale, t } from "../i18n/index.ts";
import { templateLabel } from "../lib/analytics.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { detectionParams } from "../lib/insights.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Insight = Schemas["Insight"];

const day = (iso: string) =>
  new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeZone: "UTC" }).format(
    new Date(`${iso}T00:00:00Z`),
  );

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
    }
  };
  return <>{content()}</>;
}

function InsightCard(props: {
  insight: Insight;
  dismissing: boolean;
  onDismiss: () => void;
  onOpenEditor?: (prompt: string) => void;
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
                onClick={() => props.onOpenEditor?.(h().theme_prompt)}
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
        <p class="flex flex-wrap gap-x-4 gap-y-1 text-sm text-muted-foreground">
          <span class="figures">
            {t("insights.affected", {
              n: new Intl.NumberFormat(locale()).format(
                Math.max(1, Math.round(props.insight.score)),
              ),
            })}
          </span>
          <span>{t("insights.since", { date: day(props.insight.first_detected_on) })}</span>
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
              <p class="text-xs text-muted-foreground">
                {t("insights.model", { model: h().model })}
              </p>
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
  const openEditor = (aiPrompt: string) => navigate("/themes", { state: { aiPrompt } });

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
