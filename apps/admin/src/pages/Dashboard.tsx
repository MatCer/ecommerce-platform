import {
  Alert,
  Badge,
  Card,
  Collapse,
  controlClass,
  EmptyState,
  FormGroup,
  SelectField,
  type Tone,
} from "@platform/ui";
import { A, useSearchParams } from "@solidjs/router";
import { createQuery } from "@tanstack/solid-query";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  type JSX,
  on,
  Show,
} from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { locale, t } from "../i18n/index.ts";
import {
  type DayValue,
  dailySeries,
  niceMax,
  PRESETS,
  resolveRange,
  type VitalRating,
  validRange,
  vitalRating,
} from "../lib/analytics.ts";
import { api, type Role, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";
import { useMarkets } from "../lib/queries.ts";

type Dashboard = Schemas["Dashboard"];

const str = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v) ?? "";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const STEPS = ["sessions", "view_item", "add_to_cart", "begin_checkout", "purchase"] as const;
const TEMPLATES = [
  "home",
  "category",
  "product",
  "search",
  "page",
  "blog",
  "checkout",
  "other",
] as const;
const METRICS = ["LCP", "INP", "CLS"] as const;
const RATING_TONE: Record<VitalRating, Tone> = {
  good: "success",
  needsImprovement: "warning",
  poor: "error",
};

const num = (n: number) => new Intl.NumberFormat(locale()).format(n);
const pct = (n: number) =>
  new Intl.NumberFormat(locale(), { style: "percent", maximumFractionDigits: 1 }).format(n);
const money = (minor: number, currency: string) => formatMoney(minor, currency, locale());
const day = (iso: string) =>
  new Intl.DateTimeFormat(locale(), { day: "numeric", month: "short", timeZone: "UTC" }).format(
    new Date(`${iso}T00:00:00Z`),
  );

function stepLabel(step: string): string {
  const known = STEPS.find((s) => s === step);
  return known ? t(`dashboard.funnelSteps.${known}`) : step;
}

function templateLabel(template: string): string {
  const known = TEMPLATES.find((s) => s === template);
  return known ? t(`dashboard.templates.${known}`) : template;
}

function isEmpty(d: Dashboard): boolean {
  return (
    d.sales.every((s) => s.orders === 0) &&
    d.traffic.page_requests === 0 &&
    d.traffic.consented_sessions === 0 &&
    d.top_searches.length === 0 &&
    d.zero_result_searches.length === 0 &&
    d.web_vitals.length === 0
  );
}

export default function DashboardPage() {
  const { current } = useMembership();
  const markets = useMarkets();
  const [params, setParams] = useSearchParams();
  const range = createMemo(() =>
    resolveRange({ range: str(params.range), from: str(params.from), to: str(params.to) }),
  );
  const market = () => (UUID.test(str(params.market)) ? str(params.market) : undefined);

  const dashboard = createQuery(() => ({
    queryKey: tenantKey("analytics", range().from, range().to, market() ?? ""),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/analytics/dashboard", {
          params: {
            header: tenantHeader(),
            query: { from: range().from, to: range().to, market_id: market() },
          },
        }),
      ),
  }));

  return (
    <Show when={current()}>
      {(m) => (
        <>
          <PageHeader
            title={t("dashboard.title")}
            description={t("dashboard.lead", { shop: m().name, role: t(`roles.${m().role}`) })}
          />
          <Filters
            range={range()}
            market={market() ?? ""}
            markets={markets.data?.items ?? []}
            onPreset={(p) => setParams({ range: p, from: undefined, to: undefined })}
            onCustom={(from, to) => setParams({ range: undefined, from, to })}
            onMarket={(id) => setParams({ market: id || undefined })}
          />
          <QueryState query={dashboard}>
            {(d) => (
              <Show when={!isEmpty(d)} fallback={<EmptyDashboard />}>
                <Overview data={d} />
              </Show>
            )}
          </QueryState>
        </>
      )}
    </Show>
  );
}

function Filters(props: {
  range: { preset: number | null; from: string; to: string };
  market: string;
  markets: readonly Schemas["Market"][];
  onPreset: (preset: string) => void;
  onCustom: (from: string, to: string) => void;
  onMarket: (id: string) => void;
}) {
  const [from, setFrom] = createSignal(props.range.from);
  const [to, setTo] = createSignal(props.range.to);
  // The URL is the source of truth: follow it when it changes (preset, back button).
  createEffect(
    on(
      () => [props.range.from, props.range.to],
      ([f, tt]) => {
        setFrom(f ?? "");
        setTo(tt ?? "");
      },
    ),
  );
  const invalid = () => !validRange(from(), to());
  const edit = (f: string, tt: string) => {
    setFrom(f);
    setTo(tt);
    if (validRange(f, tt)) props.onCustom(f, tt);
  };

  return (
    <fieldset class="mb-4 flex min-w-0 flex-wrap items-start gap-x-3 gap-y-2">
      <legend class="sr-only">{t("dashboard.filters")}</legend>
      <SelectField
        class="w-44"
        label={t("dashboard.period")}
        value={props.range.preset === null ? "custom" : String(props.range.preset)}
        options={[
          ...PRESETS.map((p) => ({ value: String(p), label: t(`dashboard.last${p}`) })),
          { value: "custom", label: t("dashboard.custom") },
        ]}
        onChange={(v) =>
          v === "custom" ? props.onCustom(props.range.from, props.range.to) : props.onPreset(v)
        }
      />
      <Show when={props.range.preset === null}>
        <DateInput
          label={t("dashboard.from")}
          value={from()}
          max={to()}
          invalid={invalid()}
          onChange={(v) => edit(v, to())}
        />
        <DateInput
          label={t("dashboard.to")}
          value={to()}
          min={from()}
          invalid={invalid()}
          onChange={(v) => edit(from(), v)}
        />
      </Show>
      <SelectField
        class="w-56"
        label={t("dashboard.market")}
        value={props.market}
        options={[
          { value: "", label: t("dashboard.allMarkets") },
          ...props.markets.map((mk) => ({ value: mk.id, label: `${mk.name} (${mk.currency})` })),
        ]}
        onChange={props.onMarket}
      />
      <div class="flex w-full flex-col gap-2">
        <Show when={props.range.preset === null && invalid()}>
          <div id="range-error" class="max-w-xl">
            <Alert tone="error">{t("errors.invalid_range")}</Alert>
          </div>
        </Show>
        <p class="text-xs text-muted-foreground">{t("dashboard.utcNote")}</p>
      </div>
    </fieldset>
  );
}

function DateInput(props: {
  label: string;
  value: string;
  min?: string;
  max?: string;
  invalid: boolean;
  onChange: (v: string) => void;
}) {
  const id = createUniqueId();
  return (
    <FormGroup class="w-40" label={props.label} for={id}>
      <input
        id={id}
        type="date"
        class={controlClass}
        value={props.value}
        min={props.min}
        max={props.max}
        required
        aria-invalid={props.invalid || undefined}
        aria-describedby={props.invalid ? "range-error" : undefined}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      />
    </FormGroup>
  );
}

function EmptyDashboard() {
  const { can } = useMembership();
  const steps: { href: string; label: () => string; min: Role }[] = [
    { href: "/categories", label: () => t("dashboard.stepCategories"), min: "staff" },
    { href: "/products/new", label: () => t("dashboard.stepProducts"), min: "staff" },
    { href: "/staff", label: () => t("dashboard.stepStaff"), min: "admin" },
    { href: "/account/security", label: () => t("dashboard.stepSecurity"), min: "staff" },
  ];
  return (
    <div class="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <Card>
        <EmptyState
          icon="list-task"
          title={t("dashboard.emptyTitle")}
          description={t("dashboard.emptyDesc")}
        />
      </Card>
      <Panel id="next-steps" title={t("dashboard.nextSteps")} padding="none">
        <ol class="flex flex-col">
          <For each={steps.filter((s) => can(s.min))}>
            {(s, i) => (
              <li class="border-b border-border last:border-b-0">
                <A href={s.href} class="flex h-row items-center gap-3 px-4 text-sm hover:bg-muted">
                  <span class="figures grid size-6 place-items-center rounded-full bg-subtle text-xs font-semibold text-muted-foreground">
                    {i() + 1}
                  </span>
                  <span class="text-accent-700">{s.label()}</span>
                </A>
              </li>
            )}
          </For>
        </ol>
      </Panel>
    </div>
  );
}

/** A dashboard panel: a Pajamas card that is also a named region (its title labels it). */
function Panel(props: {
  id: string;
  title: string;
  description?: string;
  actions?: JSX.Element;
  padding?: "none" | "normal";
  class?: string;
  children: JSX.Element;
}) {
  return (
    <section aria-labelledby={props.id} class={`flex min-w-0 flex-col ${props.class ?? ""}`}>
      <Card
        class="flex-1"
        title={<span id={props.id}>{props.title}</span>}
        description={props.description}
        actions={props.actions}
        padding={props.padding}
      >
        {props.children}
      </Card>
    </section>
  );
}

/** Nothing to show inside a full-bleed panel. */
function Nothing() {
  return <p class="p-4 text-sm text-muted-foreground">{t("dashboard.nothing")}</p>;
}

/** Single-stat tile (GitLab analytics style): caption, big figure, optional hint. */
function Stat(props: { label: string; value: string; hint?: string }) {
  return (
    <div class="flex min-w-0 flex-col gap-1">
      <dt class="text-sm text-muted-foreground">{props.label}</dt>
      <dd class="figures truncate text-2xl font-semibold tracking-tight text-heading">
        {props.value}
      </dd>
      <Show when={props.hint}>
        <dd class="text-xs text-muted-foreground">{props.hint}</dd>
      </Show>
    </div>
  );
}

const statGrid = "grid grid-cols-1 gap-x-6 gap-y-4 sm:grid-cols-3";

function Overview(props: { data: Dashboard }) {
  const d = () => props.data;
  const series = createMemo(() => dailySeries(d().daily_sales, d().from, d().to));
  const [metric, setMetric] = createSignal<"revenue" | "orders">("revenue");
  const ordersByDay = createMemo<DayValue[]>(() =>
    (series()[0]?.days ?? []).map((day, i) => ({
      date: day.date,
      revenue_minor: 0,
      orders: series().reduce((sum, s) => sum + (s.days[i]?.orders ?? 0), 0),
    })),
  );
  const funnelTop = () => d().traffic.funnel[0]?.sessions ?? 0;
  const templates = () => [...d().traffic.by_template].sort((a, b) => b.requests - a.requests);

  return (
    <div class="flex flex-col gap-4">
      <div class="grid gap-4 xl:grid-cols-2">
        <Panel
          id="sales"
          title={t("dashboard.sales")}
          description={d().sales.length > 1 ? t("dashboard.perCurrency") : undefined}
        >
          <Show
            when={d().sales.length > 0}
            fallback={
              <dl class={statGrid}>
                <Stat label={t("dashboard.revenue")} value="—" />
                <Stat label={t("dashboard.orders")} value={num(0)} />
                <Stat label={t("dashboard.aov")} value="—" />
              </dl>
            }
          >
            <div class="flex flex-col gap-4 divide-y divide-border">
              <For each={d().sales}>
                {(s) => (
                  <div class="flex flex-col gap-2 [&:not(:first-child)]:pt-4">
                    <Show when={d().sales.length > 1}>
                      <p class="col-label">{s.currency}</p>
                    </Show>
                    <dl class={statGrid}>
                      <Stat
                        label={t("dashboard.revenue")}
                        value={money(s.revenue_minor, s.currency)}
                      />
                      <Stat label={t("dashboard.orders")} value={num(s.orders)} />
                      <Stat label={t("dashboard.aov")} value={money(s.aov_minor, s.currency)} />
                    </dl>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Panel>

        <Panel id="traffic" title={t("dashboard.traffic")}>
          <dl class={statGrid}>
            <Stat
              label={t("dashboard.pageRequests")}
              value={num(d().traffic.page_requests)}
              hint={t("dashboard.pageRequestsHint")}
            />
            <Stat
              label={t("dashboard.consentedSessions")}
              value={num(d().traffic.consented_sessions)}
              hint={t("dashboard.consentedSessionsHint")}
            />
            <Stat
              label={t("dashboard.conversionRate")}
              value={
                d().traffic.conversion_rate == null ? "—" : pct(d().traffic.conversion_rate ?? 0)
              }
              hint={t("dashboard.conversionHint")}
            />
          </dl>
        </Panel>
      </div>

      <Show when={series().length > 0}>
        <Panel
          id="over-time"
          title={t("dashboard.overTime")}
          actions={
            <SelectField
              class="w-44"
              hideLabel
              label={t("dashboard.metric")}
              value={metric()}
              options={[
                { value: "revenue", label: t("dashboard.revenue") },
                { value: "orders", label: t("dashboard.orders") },
              ]}
              onChange={(v) => setMetric(v === "orders" ? "orders" : "revenue")}
            />
          }
        >
          <Show
            when={metric() === "revenue"}
            fallback={
              <ColumnChart
                title={t("dashboard.ordersDaily")}
                days={ordersByDay()}
                value={(x) => x.orders}
                format={num}
              />
            }
          >
            <div class="grid gap-6 xl:grid-cols-2">
              <For each={series()}>
                {(s) => (
                  <ColumnChart
                    title={t("dashboard.revenueDaily", { currency: s.currency })}
                    days={s.days}
                    value={(x) => x.revenue_minor}
                    format={(n) => money(n, s.currency)}
                  />
                )}
              </For>
            </div>
          </Show>
        </Panel>
      </Show>

      <div class="grid gap-4 lg:grid-cols-2">
        <Panel
          id="funnel"
          title={t("dashboard.funnelTitle")}
          description={t("dashboard.funnelDesc")}
        >
          <BarList
            ordered
            label={t("dashboard.funnelTitle")}
            items={d().traffic.funnel.map((f) => ({
              label: stepLabel(f.step),
              value: f.sessions,
              note:
                funnelTop() > 0
                  ? t("dashboard.funnelShare", { share: pct(f.sessions / funnelTop()) })
                  : undefined,
            }))}
          />
        </Panel>
        <Panel id="by-template" title={t("dashboard.byTemplate")}>
          <Show
            when={templates().length > 0}
            fallback={<p class="text-sm text-muted-foreground">{t("dashboard.nothing")}</p>}
          >
            <BarList
              label={t("dashboard.byTemplate")}
              items={templates().map((r) => ({
                label: templateLabel(r.template),
                value: r.requests,
              }))}
            />
          </Show>
        </Panel>
      </div>

      <Panel
        id="top-products"
        title={t("dashboard.topProducts")}
        padding={d().top_products.length > 0 ? "none" : "normal"}
      >
        <Show
          when={d().top_products.length > 0}
          fallback={<p class="text-sm text-muted-foreground">{t("dashboard.nothing")}</p>}
        >
          <div class="overflow-x-auto">
            <table class={tableClass} aria-labelledby="top-products">
              <thead>
                <tr>
                  <Th>{t("dashboard.product")}</Th>
                  <Th class="text-right">{t("dashboard.units")}</Th>
                  <Th class="text-right">{t("dashboard.revenue")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={d().top_products}>
                  {(p) => (
                    <tr class="hover:bg-subtle">
                      <td class={tdClass}>
                        <Show when={p.product_id} fallback={p.name}>
                          {(id) => (
                            <A
                              href={`/products/${id()}`}
                              class="font-semibold text-heading hover:text-accent-700 hover:underline"
                            >
                              {p.name}
                            </A>
                          )}
                        </Show>
                      </td>
                      <td class={`${tdClass} figures text-right`}>{num(p.units)}</td>
                      <td class={`${tdClass} figures text-right whitespace-nowrap`}>
                        {money(p.revenue_minor, p.currency)}
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Show>
      </Panel>

      <div class="grid gap-4 lg:grid-cols-2">
        <Panel id="top-searches" title={t("dashboard.topSearches")} padding="none">
          <SearchTable id="top-searches" rows={d().top_searches} />
        </Panel>
        <Panel id="zero-searches" title={t("dashboard.zeroSearches")} padding="none">
          <SearchTable id="zero-searches" rows={d().zero_result_searches} />
        </Panel>
      </div>

      <Panel
        id="web-vitals"
        title={t("dashboard.webVitals")}
        description={t("dashboard.webVitalsDesc")}
        padding="none"
      >
        <VitalsTable rows={d().web_vitals} />
      </Panel>
    </div>
  );
}

/**
 * One series of daily columns (single hue, one axis). The chart is an image with a text summary;
 * the exact values are in the table under it.
 * ponytail: one column per day; bucket by week if ranges near 366 days need to be readable.
 */
function ColumnChart(props: {
  title: string;
  days: readonly DayValue[];
  value: (d: DayValue) => number;
  format: (n: number) => string;
}) {
  const values = () => props.days.map(props.value);
  const max = () => niceMax(Math.max(0, ...values()));
  const peak = () => {
    const v = values();
    const i = v.indexOf(Math.max(...v));
    return props.days[i];
  };
  const summary = () => {
    const p = peak();
    const first = props.days[0];
    const last = props.days[props.days.length - 1];
    if (!p || !first || !last) return props.title;
    return t("dashboard.chartSummary", {
      title: props.title,
      from: day(first.date),
      to: day(last.date),
      total: props.format(values().reduce((a, b) => a + b, 0)),
      max: props.format(props.value(p)),
      maxDate: day(p.date),
    });
  };
  const tick = "figures text-[0.6875rem] leading-none text-faint-foreground";

  return (
    <figure class="flex min-w-0 flex-col gap-2">
      <figcaption class="text-sm font-semibold text-heading">{props.title}</figcaption>
      <div role="img" aria-label={summary()} class="grid grid-cols-[auto_1fr] gap-x-2">
        <div class={`${tick} flex h-40 flex-col justify-between text-right`} aria-hidden="true">
          <span>{props.format(max())}</span>
          <span>{props.format(max() / 2)}</span>
          <span>{props.format(0)}</span>
        </div>
        <div class="relative h-40 border-b border-border-strong" aria-hidden="true">
          <div class="absolute inset-x-0 top-0 border-t border-border" />
          <div class="absolute inset-x-0 top-1/2 border-t border-border" />
          <div
            class="absolute inset-0 flex items-end"
            classList={{ "gap-0.5": props.days.length <= 60, "gap-px": props.days.length > 60 }}
          >
            <For each={props.days}>
              {(d) => (
                <div class="flex h-full min-w-0 flex-1 items-end justify-center">
                  <div
                    class="w-full max-w-6 rounded-t-sm bg-accent-600"
                    style={{ height: `${(props.value(d) / max()) * 100}%` }}
                    title={`${day(d.date)}: ${props.format(props.value(d))}`}
                  />
                </div>
              )}
            </For>
          </div>
        </div>
        <div />
        <div class={`${tick} mt-1 flex justify-between`} aria-hidden="true">
          <span>{props.days[0] ? day(props.days[0].date) : ""}</span>
          <span>{props.days.at(-1) ? day(props.days.at(-1)?.date ?? "") : ""}</span>
        </div>
      </div>
      <Collapse summary={t("dashboard.showTable")}>
        <div class="max-h-72 overflow-auto rounded-md border border-border">
          <table class={tableClass}>
            <caption class="sr-only">{props.title}</caption>
            <thead>
              <tr>
                <Th>{t("dashboard.date")}</Th>
                <Th class="text-right">{props.title}</Th>
              </tr>
            </thead>
            <tbody>
              <For each={props.days}>
                {(d) => (
                  <tr>
                    <td class={`${tdClass} figures`}>{day(d.date)}</td>
                    <td class={`${tdClass} figures text-right`}>{props.format(props.value(d))}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Collapse>
    </figure>
  );
}

/** Horizontal bars with their values as text (the bars only repeat the numbers). */
function BarList(props: {
  label: string;
  ordered?: boolean;
  items: readonly { label: string; value: number; note?: string }[];
}) {
  const max = () => Math.max(1, ...props.items.map((i) => i.value));
  const rows = () => (
    <For each={props.items}>
      {(item) => (
        <li class="grid grid-cols-[minmax(6rem,11rem)_1fr_auto] items-center gap-3 text-sm">
          <span class="truncate" title={item.label}>
            {item.label}
          </span>
          <span class="h-2 rounded-full bg-subtle" aria-hidden="true">
            <span
              class="block h-full rounded-full bg-accent-600"
              style={{ width: `${(item.value / max()) * 100}%` }}
            />
          </span>
          <span class="text-right whitespace-nowrap">
            <span class="figures">{num(item.value)}</span>
            <Show when={item.note}>
              <span class="ml-2 text-xs text-muted-foreground">{item.note}</span>
            </Show>
          </span>
        </li>
      )}
    </For>
  );
  return (
    <Show
      when={props.ordered}
      fallback={
        <ul aria-label={props.label} class="flex flex-col gap-2">
          {rows()}
        </ul>
      }
    >
      <ol aria-label={props.label} class="flex flex-col gap-2">
        {rows()}
      </ol>
    </Show>
  );
}

function SearchTable(props: { id: string; rows: readonly Schemas["QueryCount"][] }) {
  return (
    <Show when={props.rows.length > 0} fallback={<Nothing />}>
      <table class={tableClass} aria-labelledby={props.id}>
        <thead>
          <tr>
            <Th>{t("dashboard.query")}</Th>
            <Th class="text-right">{t("dashboard.count")}</Th>
          </tr>
        </thead>
        <tbody>
          <For each={props.rows}>
            {(r) => (
              <tr>
                <td class={`${tdClass} break-all`}>{r.query}</td>
                <td class={`${tdClass} figures text-right`}>{num(r.count)}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </Show>
  );
}

function VitalsTable(props: { rows: readonly Schemas["VitalP75"][] }) {
  const byTemplate = createMemo(() => {
    const map = new Map<string, Map<string, Schemas["VitalP75"]>>();
    for (const r of props.rows) {
      const m = map.get(r.template) ?? new Map<string, Schemas["VitalP75"]>();
      m.set(r.metric, r);
      map.set(r.template, m);
    }
    return [...map];
  });
  const value = (r: Schemas["VitalP75"]) =>
    r.metric === "CLS"
      ? new Intl.NumberFormat(locale(), { maximumFractionDigits: 2 }).format(r.p75)
      : `${num(Math.round(r.p75))} ms`;

  return (
    <Show when={props.rows.length > 0} fallback={<Nothing />}>
      <div class="overflow-x-auto">
        <table class={tableClass} aria-labelledby="web-vitals">
          <thead>
            <tr>
              <Th>{t("dashboard.template")}</Th>
              <For each={METRICS}>{(metric) => <Th>{metric}</Th>}</For>
            </tr>
          </thead>
          <tbody>
            <For each={byTemplate()}>
              {([template, metrics]) => (
                <tr>
                  <td class={tdClass}>{templateLabel(template)}</td>
                  <For each={METRICS}>
                    {(metric) => (
                      <td class={`${tdClass} py-1.5`}>
                        <Show when={metrics.get(metric)} fallback="—">
                          {(r) => {
                            const rating = vitalRating(metric, r().p75);
                            return (
                              <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                                <span class="figures">{value(r())}</span>
                                <Show when={rating}>
                                  {(rt) => (
                                    <Badge tone={RATING_TONE[rt()]}>
                                      {t(`dashboard.ratings.${rt()}`)}
                                    </Badge>
                                  )}
                                </Show>
                                <span class="w-full text-xs text-faint-foreground">
                                  {t("dashboard.samples", { count: num(r().samples) })}
                                </span>
                              </div>
                            );
                          }}
                        </Show>
                      </td>
                    )}
                  </For>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </Show>
  );
}
