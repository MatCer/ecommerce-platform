/** AI settings: this month's usage against the allowance, and the translation glossary. */
import { Alert, Badge, Button, Card, ProgressBar, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Index, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, t } from "../i18n/index.ts";
import { useAiUsage } from "../lib/ai.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES } from "../lib/product-form.ts";

type Entry = { term: string; translations: Record<string, string> };

const FEATURES = [
  "product_description",
  "category_description",
  "seo",
  "translate",
  "bulk_plan",
] as const;

function featureLabel(f: string): string {
  const known = FEATURES.find((x) => x === f);
  return known ? t(`ai.feature_${known}`) : f;
}

function usd(micros: number): string {
  return `$${(micros / 1_000_000).toFixed(2)}`;
}

function Usage(props: { u: Schemas["UsageSummary"] }) {
  const nf = () => new Intl.NumberFormat();
  const share = () =>
    props.u.tokens_quota > 0 ? Math.min(1, props.u.tokens_used / props.u.tokens_quota) : 1;
  const over = () => props.u.tokens_used >= props.u.tokens_quota;
  return (
    <Card labelledBy="ai-usage-h" class="max-w-3xl" title={t("ai.usage")} padding="none">
      <div class="flex flex-col gap-4 p-4">
        <dl class="grid max-w-md grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm [&_dt]:text-muted-foreground">
          <dt class="text-muted-foreground">{t("ai.provider")}</dt>
          <dd>
            <Show when={props.u.provider === "fake"} fallback={props.u.provider}>
              <Badge tone="info">{t("ai.demo")}</Badge>
            </Show>
          </dd>
          <dt class="text-muted-foreground">{t("ai.model")}</dt>
          <dd class="font-mono text-xs">{props.u.model}</dd>
          <dt class="text-muted-foreground">{t("ai.cost")}</dt>
          <dd class="figures">{usd(props.u.cost_micros)}</dd>
        </dl>
        <div class="flex max-w-md flex-col gap-2">
          <p class="figures text-sm">
            {t("ai.tokens", {
              used: nf().format(props.u.tokens_used),
              quota: nf().format(props.u.tokens_quota),
            })}
          </p>
          <ProgressBar
            label={t("ai.usage")}
            max={1}
            value={share()}
            tone={share() >= 0.95 ? "error" : share() >= 0.8 ? "warning" : "neutral"}
          />
        </div>
        <Show when={over()}>
          <Alert tone="error">{t("ai.quotaExceeded")}</Alert>
        </Show>
      </div>
      <Show when={props.u.by_feature.length > 0}>
        <div class="overflow-x-auto border-t border-border">
          <table class={tableClass} aria-label={t("ai.byFeature")}>
            <thead>
              <tr>
                <Th>{t("ai.feature")}</Th>
                <Th class="text-right">{t("ai.calls")}</Th>
                <Th class="text-right">{t("ai.tokensCol")}</Th>
                <Th class="text-right">{t("ai.cost")}</Th>
              </tr>
            </thead>
            <tbody>
              <For each={props.u.by_feature}>
                {(f) => (
                  <tr>
                    <td class={tdClass}>{featureLabel(f.feature)}</td>
                    <td class={`${tdClass} figures text-right`}>{nf().format(f.calls)}</td>
                    <td class={`${tdClass} figures text-right`}>{nf().format(f.tokens)}</td>
                    <td class={`${tdClass} figures text-right`}>{usd(f.cost_micros)}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
    </Card>
  );
}

function GlossaryEditor() {
  const query = createQuery(() => ({
    queryKey: tenantKey("ai-glossary"),
    queryFn: () => unwrap(api.GET("/admin/v1/ai/glossary", { params: { header: tenantHeader() } })),
  }));
  const [entries, setEntries] = createSignal<Entry[]>([]),
    [loaded, setLoaded] = createSignal(""),
    [error, setError] = createSignal<string>();
  createEffect(() => {
    const key = JSON.stringify(tenantKey("ai-glossary"));
    if (query.data && loaded() !== key) {
      setEntries(
        query.data.entries.map((e) => ({ term: e.term, translations: { ...e.translations } })),
      );
      setLoaded(key);
    }
  });
  const set = (i: number, patch: Partial<Entry>) =>
    setEntries(entries().map((e, j) => (i === j ? { ...e, ...patch } : e)));
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/ai/glossary", {
          params: { header: tenantHeader() },
          body: {
            entries: entries()
              .filter((e) => e.term.trim())
              .map((e) => ({
                term: e.term.trim(),
                translations: Object.fromEntries(
                  Object.entries(e.translations)
                    .map(([l, v]) => [l, v.trim()])
                    .filter(([, v]) => v),
                ),
              })),
          },
        }),
      ),
    onMutate: () => setError(undefined),
    onSuccess: (g) => {
      setEntries(g.entries.map((e) => ({ term: e.term, translations: { ...e.translations } })));
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(errorMessage(e)),
  }));
  return (
    <Card
      labelledBy="ai-glossary-h"
      class="max-w-5xl"
      title={t("ai.glossary")}
      description={t("ai.glossaryDesc")}
    >
      <QueryState query={query}>
        {() => (
          <form
            class="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <Show
              when={entries().length > 0}
              fallback={
                <p class="rounded-md border border-dashed border-border p-4 text-center text-sm text-muted-foreground">
                  {t("ai.glossaryEmpty")}
                </p>
              }
            >
              <ul class="flex flex-col gap-2" aria-label={t("ai.glossary")}>
                <Index each={entries()}>
                  {(e, i) => (
                    <li class="grid items-end gap-2 border-b border-border pb-3 sm:grid-cols-[2fr_1fr_1fr_1fr_auto]">
                      <TextField
                        label={t("ai.term", { n: i + 1 })}
                        value={e().term}
                        maxLength={100}
                        onChange={(term) => set(i, { term })}
                      />
                      <For each={CONTENT_LOCALES}>
                        {(l) => (
                          <TextField
                            label={t("ai.fixed", { locale: l.toUpperCase(), n: i + 1 })}
                            value={e().translations[l] ?? ""}
                            maxLength={100}
                            placeholder={e().term}
                            onChange={(v) =>
                              set(i, { translations: { ...e().translations, [l]: v } })
                            }
                          />
                        )}
                      </For>
                      <Button
                        category="tertiary"
                        icon="remove"
                        onClick={() => setEntries(entries().filter((_, j) => j !== i))}
                        aria-label={t("ai.removeTerm", { term: e().term || String(i + 1) })}
                      >
                        {t("common.remove")}
                      </Button>
                    </li>
                  )}
                </Index>
              </ul>
            </Show>
            <Show when={error()}>
              <Alert tone="error">{error()}</Alert>
            </Show>
            <div>
              <Button
                icon="plus"
                onClick={() => setEntries([...entries(), { term: "", translations: {} }])}
              >
                {t("ai.addTerm")}
              </Button>
            </div>
            <div class="border-t border-border pt-4">
              <Button type="submit" variant="confirm" loading={save.isPending}>
                {t("common.save")}
              </Button>
            </div>
          </form>
        )}
      </QueryState>
    </Card>
  );
}

export default function AiSettings() {
  const usage = useAiUsage();
  return (
    <>
      <PageHeader title={t("ai.settingsTitle")} description={t("ai.settingsDesc")} />
      <div class="flex flex-col gap-6">
        <QueryState query={usage}>{(u) => <Usage u={u} />}</QueryState>
        <GlossaryEditor />
      </div>
    </>
  );
}
