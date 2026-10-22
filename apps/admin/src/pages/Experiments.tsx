import {
  Alert,
  Badge,
  Button,
  Card,
  ConfirmDialog,
  Dialog,
  EmptyState,
  FieldGroup,
  hintClass,
  LoadingState,
  linkClass,
  Radio,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { A, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
import { formatNumber, formatPercent } from "../lib/analytics.ts";
import { ApiError, api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import {
  changeMessage,
  flooredChance,
  HERO_FIELDS,
  type HeroFields,
  heroCopy,
  type Message,
  points,
  readoutMessage,
  stopMessage,
} from "../lib/experiments.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";

type Experiment = Schemas["Experiment"];
type Detail = Schemas["ExperimentDetail"];
type Action = "start" | "stop" | "promote";

const say = (m: Message) => t(m.key, m.params);

const STATUS_TONE: Record<Schemas["ExperimentStatus"], Tone> = {
  draft: "neutral",
  running: "info",
  stopped: "neutral",
};
const HERO_LABEL = {
  title: "experiments.form.heroTitle",
  subtitle: "experiments.form.heroSubtitle",
  cta: "experiments.form.heroCta",
} as const;
const DONE = {
  start: "experiments.actions.started",
  stop: "experiments.actions.stopped",
  promote: "experiments.actions.promoted",
} as const;
const USABLE_STATUSES = ["ready", "published", "superseded"] as const;
type Usable = Schemas["RevisionSummary"] & { status: (typeof USABLE_STATUSES)[number] };
const ARMS = ["control", "variant"] as const;

const localeName = (code: string) =>
  new Intl.DisplayNames([locale()], { type: "language" }).of(code) ?? code;
const fullName = (e: Experiment) =>
  t("experiments.name", { name: e.name, version: String(e.version) });
const usable = (r: Schemas["RevisionSummary"]): r is Usable =>
  Boolean(r.artifact_id) && (USABLE_STATUSES as readonly string[]).includes(r.status);
const cookiePolicyOutdated = (e: unknown) =>
  e instanceof ApiError && e.code === "cookie_policy_outdated";
const heroOf = (c: Schemas["ExperimentChange"]) => (c.kind === "hero" ? c.hero : undefined);
const promotable = (e: Experiment) =>
  e.status === "running" || (e.status === "stopped" && !e.promoted_at);

export default function Experiments() {
  const params = useParams();
  return (
    <Show when={params.id} keyed fallback={<ExperimentList />}>
      {(id) => <ExperimentDetail id={id} />}
    </Show>
  );
}

function ExperimentList() {
  const { can } = useMembership();
  const [creating, setCreating] = createSignal(false);
  const list = createQuery(() => ({
    queryKey: tenantKey("experiments"),
    queryFn: () => unwrap(api.GET("/admin/v1/experiments", { params: { header: tenantHeader() } })),
  }));
  const newButton = () => (
    <Show when={can("admin")}>
      <Button variant="confirm" icon="plus" onClick={() => setCreating(true)}>
        {t("experiments.new")}
      </Button>
    </Show>
  );

  return (
    <>
      <PageHeader
        title={t("experiments.title")}
        description={t("experiments.intro")}
        actions={newButton()}
      />
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <Card aria-label={t("experiments.title")} class="max-w-5xl">
                <EmptyState
                  title={t("experiments.emptyTitle")}
                  description={t("experiments.emptyBody")}
                  action={newButton()}
                />
              </Card>
            }
          >
            <Card padding="none" class="max-w-5xl">
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <caption class="sr-only">{t("experiments.title")}</caption>
                  <thead>
                    <tr>
                      <Th>{t("experiments.columns.name")}</Th>
                      <Th>{t("experiments.columns.change")}</Th>
                      <Th>{t("experiments.columns.status")}</Th>
                      <Th>{t("experiments.columns.started")}</Th>
                      <Th>{t("experiments.columns.stopped")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(e) => (
                        <tr>
                          <td class={tdClass}>
                            <A
                              href={`/experiments/${e.id}`}
                              class={`${linkClass} font-semibold break-all`}
                            >
                              {fullName(e)}
                            </A>
                          </td>
                          <td class={tdClass}>{say(changeMessage(e))}</td>
                          <td class={tdClass}>
                            <Badge tone={STATUS_TONE[e.status]}>
                              {t(`experiments.status.${e.status}`)}
                            </Badge>
                          </td>
                          <td class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}>
                            {e.started_at ? formatDateTime(e.started_at) : "—"}
                          </td>
                          <td class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}>
                            {e.stopped_at ? formatDateTime(e.stopped_at) : "—"}
                            <Show when={e.stop_reason}>
                              {(r) => (
                                <span class="block text-xs">
                                  {t(`experiments.stopShort.${r()}`)}
                                </span>
                              )}
                            </Show>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Card>
          </Show>
        )}
      </QueryState>
      <Show when={can("admin")}>
        <CreateDialog open={creating()} onOpenChange={setCreating} />
      </Show>
    </>
  );
}

function CreateDialog(props: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const formId = createUniqueId();
  const markets = useMarkets();
  const revisions = createQuery(() => ({
    queryKey: tenantKey("theme-revisions"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/themes/revisions", { params: { header: tenantHeader() } })),
    enabled: props.open,
  }));
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal<"hero" | "theme">("hero");
  const [hero, setHero] = createSignal<Record<string, HeroFields>>({});
  const [revision, setRevision] = createSignal("");
  const locales = () => [...new Set((markets.data?.items ?? []).flatMap((m) => m.locales))];
  const ready = () => (revisions.data?.items ?? []).filter(usable);
  const revisionId = () => revision() || ready()[0]?.id || "";
  const [checked, setChecked] = createSignal(false);
  const heroInput = () => heroCopy(hero(), locales());
  const missing = (l: string, f: keyof HeroFields) => {
    const input = heroInput();
    const message = checked() && "missing" in input ? input.missing[l]?.[f] : undefined;
    return message && say(message);
  };
  const text = (l: string, f: keyof HeroFields) => hero()[l]?.[f] ?? "";
  const setText = (l: string, f: keyof HeroFields, v: string) =>
    setHero((h) => ({ ...h, [l]: { title: "", subtitle: "", cta: "", ...h[l], [f]: v } }));
  const creation = submission();

  const create = createMutation(() => ({
    mutationFn: (copy: Schemas["HeroCopy"] | undefined) => {
      const body: Schemas["NewExperiment"] = {
        name: name().trim(),
        change:
          kind() === "hero"
            ? { kind: "hero", hero: copy ?? {} }
            : { kind: "theme", revision_id: revisionId() },
      };
      return unwrap(
        api.POST("/admin/v1/experiments", { params: { header: creation.header(body) }, body }),
      );
    },
    onSuccess: async (e: Experiment) => {
      creation.done();
      setName("");
      setHero({});
      setChecked(false);
      setRevision("");
      props.onOpenChange(false);
      await qc.invalidateQueries({ queryKey: tenantKey("experiments") });
      navigate(`/experiments/${e.id}`);
      showToast({ title: t("experiments.form.created"), closeLabel: t("common.close") });
    },
  }));
  const nameError = () =>
    create.error instanceof ApiError && create.error.code === "invalid_experiment_name"
      ? errorMessage(create.error)
      : undefined;

  return (
    <Dialog
      open={props.open}
      onOpenChange={props.onOpenChange}
      title={t("experiments.form.title")}
      closeLabel={t("common.close")}
      footer={
        <>
          <Button onClick={() => props.onOpenChange(false)}>{t("common.cancel")}</Button>
          <Button
            type="submit"
            form={formId}
            variant="confirm"
            loading={create.isPending}
            disabled={kind() === "theme" && !revisionId()}
          >
            {t("experiments.form.submit")}
          </Button>
        </>
      }
    >
      <form
        id={formId}
        class="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          const input = heroInput();
          if (kind() === "hero" && "missing" in input) {
            setChecked(true);
            e.currentTarget.querySelector<HTMLElement>("[aria-invalid=true]")?.focus();
            return;
          }
          create.mutate("copy" in input ? input.copy : undefined);
        }}
      >
        <TextField
          label={t("experiments.form.name")}
          description={t("experiments.form.nameHint")}
          value={name()}
          onChange={setName}
          error={nameError()}
          required
          maxLength={64}
          autocomplete="off"
        />
        <fieldset class="flex flex-col gap-2">
          <legend class="mb-1 text-sm font-semibold text-heading">
            {t("experiments.form.kind")}
          </legend>
          <Radio
            name={`${formId}-kind`}
            value="hero"
            checked={kind() === "hero"}
            onChange={() => setKind("hero")}
            label={t("experiments.form.hero")}
          />
          <Radio
            name={`${formId}-kind`}
            value="theme"
            checked={kind() === "theme"}
            onChange={() => setKind("theme")}
            label={t("experiments.form.theme")}
          />
        </fieldset>
        <Show
          when={kind() === "hero"}
          fallback={
            <Show
              when={!revisions.isPending}
              fallback={<LoadingState label={t("common.loading")} />}
            >
              <Show
                when={ready().length > 0}
                fallback={
                  <Show
                    when={revisions.isError}
                    fallback={
                      <Alert tone="info">
                        {t("experiments.form.noRevisions")}{" "}
                        <A href="/themes" class={linkClass}>
                          {t("experiments.form.themesLink")}
                        </A>
                      </Alert>
                    }
                  >
                    <Alert tone="error">{errorMessage(revisions.error)}</Alert>
                  </Show>
                }
              >
                <SelectField
                  label={t("experiments.form.revision")}
                  value={revisionId()}
                  onChange={setRevision}
                  options={ready().map((r) => ({
                    value: r.id,
                    label: `${t("themes.revision", { number: r.number })} · ${t(`themes.statuses.${r.status}`)}`,
                  }))}
                />
              </Show>
            </Show>
          }
        >
          <p class={hintClass}>{t("experiments.form.heroHint")}</p>
          <Show when={markets.isError}>
            <Alert tone="error">{errorMessage(markets.error)}</Alert>
          </Show>
          <For each={locales()}>
            {(l) => (
              <FieldGroup legend={localeName(l)}>
                <For each={HERO_FIELDS}>
                  {(f) => (
                    <TextField
                      label={t(HERO_LABEL[f])}
                      value={text(l, f)}
                      onChange={(v) => setText(l, f, v)}
                      error={missing(l, f)}
                      maxLength={200}
                    />
                  )}
                </For>
              </FieldGroup>
            )}
          </For>
        </Show>
        <Show when={create.error && !nameError()}>
          <Alert tone="error">{errorMessage(create.error)}</Alert>
        </Show>
      </form>
    </Dialog>
  );
}

function post(action: Action, id: string) {
  const init = { params: { header: tenantHeader(), path: { id } } };
  switch (action) {
    case "start":
      return unwrap(api.POST("/admin/v1/experiments/{id}/start", init));
    case "stop":
      return unwrap(api.POST("/admin/v1/experiments/{id}/stop", init));
    case "promote":
      return unwrap(api.POST("/admin/v1/experiments/{id}/promote", init));
  }
}

function ExperimentDetail(props: { id: string }) {
  const { can } = useMembership();
  const qc = useQueryClient();
  const key = () => tenantKey("experiment", props.id);
  const detail = createQuery(() => ({
    queryKey: key(),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/experiments/{id}", {
          params: { header: tenantHeader(), path: { id: props.id } },
        }),
      ),
  }));
  const [confirm, setConfirm] = createSignal<"stop" | "promote">();
  const action = createMutation(() => ({
    mutationFn: (a: Action) => post(a, props.id),
    onSuccess: (_: unknown, a: Action) =>
      showToast({ title: t(DONE[a]), closeLabel: t("common.close") }),
    onSettled: async (_d: unknown, _e: unknown, a: Action) => {
      setConfirm(undefined);
      await Promise.all([
        qc.invalidateQueries({ queryKey: tenantKey("experiments") }),
        qc.invalidateQueries({ queryKey: key() }),
        a === "promote" && qc.invalidateQueries({ queryKey: tenantKey("theme-revisions") }),
      ]);
    },
  }));
  const pending = (a: Action) => action.isPending && action.variables === a;

  return (
    <>
      <PageHeader
        title={detail.data ? fullName(detail.data.experiment) : t("experiments.title")}
        back={{ href: "/experiments", label: t("experiments.title") }}
      />
      {/* Mounted once, so a refetch updates the readout's live region instead of replacing it. */}
      <Show when={detail.data} fallback={<QueryState query={detail}>{() => null}</QueryState>}>
        {(d) => {
          const e = () => d().experiment;
          const promote = () => {
            const c = e().change;
            return c.kind === "theme"
              ? {
                  label: t("experiments.actions.promoteTheme", { number: c.revision_number }),
                  title: t("experiments.actions.promoteThemeTitle", { number: c.revision_number }),
                  body: t("experiments.actions.promoteThemeBody", { number: c.revision_number }),
                }
              : {
                  label: t("experiments.actions.promoteHero"),
                  title: t("experiments.actions.promoteHeroTitle"),
                  body: t("experiments.actions.promoteHeroBody"),
                };
          };
          return (
            <div class="flex max-w-5xl flex-col gap-4">
              <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
                <Badge tone={STATUS_TONE[e().status]}>
                  {t(`experiments.status.${e().status}`)}
                </Badge>
                <Show when={e().started_at}>
                  {(at) => (
                    <span class="figures">
                      {t("experiments.startedOn", { date: formatDateTime(at()) })}
                    </span>
                  )}
                </Show>
              </div>
              <Card labelledBy="experiment-change-h" title={t("experiments.changeTitle")}>
                <div class="flex flex-col gap-4">
                  <ChangeView experiment={e()} />
                  <Show when={stopMessage(e(), locale())}>
                    {(m) => <p class="text-sm text-foreground">{say(m())}</p>}
                  </Show>
                  <Show when={e().stop_reason !== "promoted" && e().promoted_at}>
                    {(at) => (
                      <p class="text-sm text-foreground">
                        {t("experiments.promotedOn", { date: formatDateTime(at()) })}
                      </p>
                    )}
                  </Show>
                  <Show when={action.error}>
                    {(err) => (
                      <Alert tone="error">
                        {errorMessage(err())}
                        <Show when={cookiePolicyOutdated(err())}>
                          {" "}
                          <A href="/content/legal" class={linkClass}>
                            {t("experiments.cookieLink")}
                          </A>
                        </Show>
                      </Alert>
                    )}
                  </Show>
                  <Show when={can("admin")}>
                    <Show when={e().change.kind === "component" && promotable(e())}>
                      <Alert tone="info">{t("experiments.actions.componentNote")}</Alert>
                    </Show>
                    <div class="flex flex-col gap-2 sm:flex-row sm:flex-wrap empty:hidden">
                      <Show when={e().status === "draft"}>
                        <Button
                          variant="confirm"
                          loading={pending("start")}
                          disabled={action.isPending}
                          onClick={() => action.mutate("start")}
                        >
                          {t("experiments.actions.start")}
                        </Button>
                      </Show>
                      <Show when={e().change.kind !== "component" && promotable(e())}>
                        <Button
                          variant="confirm"
                          loading={pending("promote")}
                          disabled={action.isPending}
                          onClick={() => setConfirm("promote")}
                        >
                          {promote().label}
                        </Button>
                      </Show>
                      <Show when={e().status === "running"}>
                        <Button
                          variant="danger"
                          category="secondary"
                          loading={pending("stop")}
                          disabled={action.isPending}
                          onClick={() => setConfirm("stop")}
                        >
                          {t("experiments.actions.stop")}
                        </Button>
                      </Show>
                    </div>
                  </Show>
                </div>
              </Card>
              <ReadoutCard detail={d()} />
              <ConfirmDialog
                open={confirm() === "stop"}
                onOpenChange={(o) => !o && setConfirm(undefined)}
                title={t("experiments.actions.stopTitle")}
                description={t("experiments.actions.stopBody")}
                confirmLabel={t("experiments.actions.stop")}
                cancelLabel={t("common.cancel")}
                danger
                pending={action.isPending}
                onConfirm={() => action.mutate("stop")}
              />
              <ConfirmDialog
                open={confirm() === "promote"}
                onOpenChange={(o) => !o && setConfirm(undefined)}
                title={promote().title}
                description={[
                  say(readoutMessage(d(), locale())),
                  promote().body,
                  e().status === "running" ? t("experiments.actions.promoteStops") : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
                confirmLabel={promote().label}
                cancelLabel={t("common.cancel")}
                pending={action.isPending}
                onConfirm={() => action.mutate("promote")}
              />
            </div>
          );
        }}
      </Show>
    </>
  );
}

function ChangeView(props: { experiment: Experiment }) {
  const change = () => props.experiment.change;
  return (
    <Show
      when={heroOf(change())}
      fallback={
        <p class="text-sm text-foreground">
          <Show when={change().kind === "theme"} fallback={say(changeMessage(props.experiment))}>
            <A href="/themes" class={linkClass}>
              {say(changeMessage(props.experiment))}
            </A>
          </Show>
        </p>
      }
    >
      {(hero) => {
        const locales = () => [
          ...new Set(HERO_FIELDS.flatMap((f) => Object.keys(hero()[f] ?? {}))),
        ];
        return (
          <div class="flex flex-col gap-3">
            <p class="text-sm text-foreground">{say(changeMessage(props.experiment))}</p>
            <For each={locales()}>
              {(l) => (
                <section class="flex flex-col gap-1">
                  <h3 class="text-sm font-semibold text-heading">{localeName(l)}</h3>
                  <dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
                    <For each={HERO_FIELDS}>
                      {(f) => (
                        <Show when={hero()[f]?.[l]}>
                          {(text) => (
                            <>
                              <dt class="text-muted-foreground">{t(HERO_LABEL[f])}</dt>
                              <dd class="break-words">{text()}</dd>
                            </>
                          )}
                        </Show>
                      )}
                    </For>
                  </dl>
                </section>
              )}
            </For>
          </div>
        );
      }}
    </Show>
  );
}

function ReadoutCard(props: { detail: Detail }) {
  const num = (n: number) => formatNumber(n, locale());
  const rate = (r: number | null | undefined) => (r == null ? "—" : formatPercent(r, locale()));
  return (
    <Card labelledBy="experiment-readout-h" title={t("experiments.readout.title")}>
      <div class="flex flex-col gap-3">
        <p aria-live="polite" class="text-base font-semibold text-heading">
          {say(readoutMessage(props.detail, locale()))}
        </p>
        <Show when={props.detail.readout}>
          {(r) => (
            <>
              <p class="text-sm text-muted-foreground">
                {t("experiments.readout.asOf", { date: formatDateTime(r().as_of) })}{" "}
                {t("experiments.readout.countedHint")}
              </p>
              <section
                aria-labelledby="experiment-readout-caption"
                // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable region must be reachable by keyboard
                tabIndex={0}
                class="overflow-x-auto rounded-md border border-border"
              >
                <table class={tableClass}>
                  <caption id="experiment-readout-caption" class="sr-only">
                    {t("experiments.readout.caption")}
                  </caption>
                  <thead>
                    <tr>
                      <Th>{t("experiments.readout.columns.version")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.assigned")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.counted")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.buyers")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.rate")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.best")}</Th>
                      <Th class="text-right">{t("experiments.readout.columns.loss")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={ARMS}>
                      {(side) => {
                        const a = () => r()[side];
                        return (
                          <tr>
                            <th
                              scope="row"
                              class={`${tdClass} text-left font-semibold whitespace-nowrap text-heading`}
                            >
                              {t(`experiments.readout.${side}`)}
                            </th>
                            <td class={`${tdClass} figures text-right`}>{num(a().assigned)}</td>
                            <td class={`${tdClass} figures text-right`}>{num(a().subjects)}</td>
                            <td class={`${tdClass} figures text-right`}>{num(a().converters)}</td>
                            <td class={`${tdClass} figures text-right`}>
                              {rate(a().conversion_rate)}
                            </td>
                            <td class={`${tdClass} figures text-right`}>
                              {flooredChance(a().p_best, locale())}
                            </td>
                            <td class={`${tdClass} figures text-right whitespace-nowrap`}>
                              {t("experiments.readout.points", {
                                n: points(a().expected_loss, locale()),
                              })}
                            </td>
                          </tr>
                        );
                      }}
                    </For>
                  </tbody>
                </table>
              </section>
              <p class="text-sm text-muted-foreground">{t("experiments.readout.lossHint")}</p>
            </>
          )}
        </Show>
      </div>
    </Card>
  );
}
