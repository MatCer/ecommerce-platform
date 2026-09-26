import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  EmptyState,
  ErrorState,
  FieldGroup,
  PermissionDenied,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { useSearchParams } from "@solidjs/router";
import {
  createInfiniteQuery,
  createMutation,
  createQuery,
  useQueryClient,
} from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";

type Config = Schemas["AdPlatformConfig"];
type Platform = Schemas["AdPlatform"];
type Settings = Schemas["AdPlatformSettings"];
type Credentials = Schemas["AdPlatformCredentials"];
type SettingField = keyof Settings;
type CredentialField = keyof Credentials;
type Form = {
  enabled: boolean;
  test_mode: boolean;
  market_ids: string[];
  settings: Record<string, string>;
  credentials: Record<string, string>;
};

const PLATFORMS = ["meta", "ga4", "google_ads", "sklik"] as const;
const STATUSES = [
  "pending",
  "retrying",
  "paused",
  "sending",
  "succeeded",
  "dead",
  "cancelled",
  "skipped",
] as const;
const STATUS_TONE: Record<(typeof STATUSES)[number], Tone> = {
  pending: "neutral",
  retrying: "warning",
  paused: "warning",
  sending: "info",
  succeeded: "success",
  dead: "error",
  cancelled: "neutral",
  skipped: "neutral",
};
const EVENTS = [
  "page_view",
  "view_item",
  "add_to_cart",
  "begin_checkout",
  "purchase",
  "refund",
] as const;
const SETTING_FIELDS = [
  "pixel_id",
  "test_event_code",
  "measurement_id",
  "customer_id",
  "conversion_action_id",
  "login_customer_id",
  "sem_id",
] as const;
const CREDENTIAL_FIELDS = [
  "access_token",
  "api_secret",
  "client_id",
  "client_secret",
  "refresh_token",
] as const;
const NOTICES = [
  "sklik_no_browser_ids",
  "sklik_czk_only",
  "sklik_no_test_channel",
  "google_ads_purchases_only",
] as const;

const str = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v) ?? "";
const unavailable = (err: unknown) => err instanceof ApiError && err.status === 503;
const known = <T extends string>(list: readonly T[], v: string): T | undefined =>
  list.find((x) => x === v);

function platformName(p: string): string {
  const k = known(PLATFORMS, p);
  return k ? t(`adTracking.platforms.${k}`) : p;
}
function eventName(e: string): string {
  const k = known(EVENTS, e);
  return k ? t(`adTracking.events.${k}`) : e;
}
function fieldLabel(f: string): string {
  const s = known(SETTING_FIELDS, f);
  if (s) return t(`adTracking.fields.${s}`);
  const c = known(CREDENTIAL_FIELDS, f);
  return c ? t(`adTracking.fields.${c}`) : f;
}

function StatusBadge(props: { status: string }) {
  const k = known(STATUSES, props.status);
  return (
    <Badge tone={k ? STATUS_TONE[k] : "neutral"}>
      {k ? t(`adTracking.statuses.${k}`) : props.status}
    </Badge>
  );
}

function StateBadge(props: { config: Config }) {
  const c = () => props.config;
  return (
    <span class="flex flex-wrap gap-1">
      <Show when={c().enabled} fallback={<Badge tone="neutral">{t("adTracking.state.off")}</Badge>}>
        <Show when={c().paused} fallback={<Badge tone="success">{t("adTracking.state.on")}</Badge>}>
          <Badge tone="warning">{t("adTracking.state.paused")}</Badge>
        </Show>
      </Show>
      <Show when={c().test_mode}>
        <Badge tone="warning">{t("adTracking.state.test")}</Badge>
      </Show>
    </span>
  );
}

export default function AdTracking() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const markets = useMarkets();
  const [params, setParams] = useSearchParams();
  const platformFilter = () => known(PLATFORMS, str(params.platform));
  const statusFilter = () => known(STATUSES, str(params.status));

  const [editing, setEditing] = createSignal<Config | null>(null);
  const [form, setForm] = createSignal<Form>({
    enabled: false,
    test_mode: false,
    market_ids: [],
    settings: {},
    credentials: {},
  });
  const [error, setError] = createSignal<string>();

  const list = createQuery(() => ({
    queryKey: tenantKey("ad-platforms"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/ad-platforms", { params: { header: tenantHeader() } })),
    enabled: can("admin"),
  }));

  const deliveries = createInfiniteQuery(() => ({
    queryKey: tenantKey("ad-deliveries", platformFilter() ?? "", statusFilter() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/ad-platforms/deliveries", {
          params: {
            header: tenantHeader(),
            query: {
              platform: platformFilter(),
              status: statusFilter(),
              cursor: pageParam,
              limit: 50,
            },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: can("admin") && list.isSuccess,
  }));
  const rows = () => deliveries.data?.pages.flatMap((p) => p.items) ?? [];

  const refresh = () =>
    Promise.all([
      qc.invalidateQueries({ queryKey: tenantKey("ad-platforms") }),
      qc.invalidateQueries({ queryKey: tenantKey("ad-deliveries") }),
    ]);
  const toastError = (err: unknown) =>
    showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

  const openForm = (c: Config) => {
    const settings: Record<string, string> = {};
    for (const f of c.setting_fields) settings[f] = c.settings[f as SettingField] ?? "";
    setForm({
      enabled: c.enabled,
      test_mode: c.test_mode,
      market_ids: [...c.market_ids],
      settings,
      credentials: {},
    });
    setError(undefined);
    setEditing(c);
  };

  const patch = (platform: Platform, body: Schemas["AdPlatformUpdate"]) =>
    unwrap(
      api.PATCH("/admin/v1/ad-platforms/{platform}", {
        params: { header: tenantHeader(), path: { platform } },
        body,
      }),
    );

  const save = createMutation(() => ({
    mutationFn: async () => {
      const target = editing();
      if (!target) return;
      const f = form();
      const settings: Settings = {};
      for (const [k, v] of Object.entries(f.settings))
        if (v.trim()) settings[k as SettingField] = v.trim();
      const entered = Object.entries(f.credentials).filter(([, v]) => v.trim());
      const credentials: Credentials = {};
      for (const [k, v] of entered) credentials[k as CredentialField] = v.trim();
      await patch(target.platform, {
        enabled: f.enabled,
        test_mode: f.test_mode,
        market_ids: f.market_ids,
        settings,
        ...(entered.length ? { credentials } : {}),
      });
    },
    onSuccess: async () => {
      setEditing(null);
      await refresh();
      showToast({ title: t("adTracking.saved"), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const pause = createMutation(() => ({
    mutationFn: (c: Config) => patch(c.platform, { paused: !c.paused }),
    onSuccess: async (c) => {
      await refresh();
      showToast({
        title: c.paused ? t("adTracking.paused") : t("adTracking.resumed"),
        closeLabel: t("common.close"),
      });
    },
    onError: toastError,
  }));

  const test = createMutation(() => ({
    mutationFn: (platform: Platform) =>
      unwrap(
        api.POST("/admin/v1/ad-platforms/{platform}/test", {
          params: { header: tenantHeader(), path: { platform } },
        }),
      ),
    onSuccess: (r, platform) =>
      showToast({
        title: r.ok
          ? t("adTracking.testOk", { platform: platformName(platform) })
          : t("adTracking.testFailed", { platform: platformName(platform) }),
        description: r.response_code ? `${r.message} (HTTP ${r.response_code})` : r.message,
        tone: r.ok ? "success" : "error",
        closeLabel: t("common.close"),
      }),
    onError: toastError,
  }));

  const toggleMarket = (id: string, on: boolean) =>
    setForm((f) => ({
      ...f,
      market_ids: on ? [...f.market_ids, id] : f.market_ids.filter((m) => m !== id),
    }));
  const marketNames = (ids: string[]) =>
    ids.map((id) => markets.data?.items.find((m) => m.id === id)?.name ?? id).join(", ") || "—";

  return (
    <Show
      when={can("admin")}
      fallback={
        <>
          <PageHeader title={t("adTracking.title")} />
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        </>
      }
    >
      <PageHeader title={t("adTracking.title")} description={t("adTracking.lead")} />
      <Show
        when={!unavailable(list.error)}
        fallback={
          <ErrorState
            title={t("adTracking.unavailableTitle")}
            description={t("adTracking.unavailableDesc")}
          />
        }
      >
        <QueryState query={list}>
          {(data) => (
            <div class="flex flex-col gap-8">
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t("adTracking.platformsLabel")}>
                  <thead>
                    <tr>
                      <Th>{t("adTracking.platform")}</Th>
                      <Th>{t("adTracking.status")}</Th>
                      <Th>{t("adTracking.markets")}</Th>
                      <Th>{t("adTracking.forwards")}</Th>
                      <Th>{t("adTracking.credentials")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(c) => (
                        <tr class="align-top" data-platform={c.platform}>
                          <td class={`${tdClass} py-2 font-medium`}>{platformName(c.platform)}</td>
                          <td class={`${tdClass} py-2`}>
                            <StateBadge config={c} />
                          </td>
                          <td class={`${tdClass} py-2 text-xs`}>{marketNames(c.market_ids)}</td>
                          <td class={`${tdClass} max-w-64 py-2 text-xs text-muted-foreground`}>
                            {c.events.map(eventName).join(", ")}
                          </td>
                          <td
                            class={`${tdClass} py-2 text-xs whitespace-nowrap text-muted-foreground`}
                          >
                            <Show
                              when={c.credential_fields.length > 0}
                              fallback={t("adTracking.noCredentialsNeeded")}
                            >
                              {c.credentials_hint
                                ? t("adTracking.credentialsEnding", { hint: c.credentials_hint })
                                : t("adTracking.credentialsMissing")}
                            </Show>
                          </td>
                          <td class={`${tdClass} py-1 text-right whitespace-nowrap`}>
                            <Button category="tertiary" onClick={() => openForm(c)}>
                              {t("adTracking.configure")}
                              <span class="sr-only">: {platformName(c.platform)}</span>
                            </Button>
                            <Button
                              category="tertiary"
                              disabled={!c.complete}
                              loading={test.isPending && test.variables === c.platform}
                              onClick={() => test.mutate(c.platform)}
                            >
                              {t("adTracking.test")}
                              <span class="sr-only">: {platformName(c.platform)}</span>
                            </Button>
                            <Show when={c.enabled}>
                              <Button
                                category="tertiary"
                                loading={
                                  pause.isPending && pause.variables?.platform === c.platform
                                }
                                onClick={() => pause.mutate(c)}
                              >
                                {c.paused ? t("adTracking.resume") : t("adTracking.pause")}
                                <span class="sr-only">: {platformName(c.platform)}</span>
                              </Button>
                            </Show>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
              <p class="max-w-3xl text-xs text-muted-foreground">{t("adTracking.consentNote")}</p>

              <section aria-labelledby="ad-deliveries" class="flex flex-col gap-3">
                <div class="flex flex-col gap-0.5">
                  <h2 id="ad-deliveries" class="text-sm font-semibold">
                    {t("adTracking.deliveries")}
                  </h2>
                  <p class="text-xs text-muted-foreground">{t("adTracking.deliveriesDesc")}</p>
                </div>
                <div class="flex flex-wrap items-end gap-2">
                  <SelectField
                    class="w-56 max-w-full"
                    label={t("adTracking.platform")}
                    value={platformFilter() ?? ""}
                    options={[
                      { value: "", label: t("adTracking.allPlatforms") },
                      ...PLATFORMS.map((p) => ({ value: p, label: platformName(p) })),
                    ]}
                    onChange={(v) => setParams({ platform: v || undefined })}
                  />
                  <SelectField
                    class="w-48 max-w-full"
                    label={t("adTracking.status")}
                    value={statusFilter() ?? ""}
                    options={[
                      { value: "", label: t("adTracking.allStatuses") },
                      ...STATUSES.map((s) => ({ value: s, label: t(`adTracking.statuses.${s}`) })),
                    ]}
                    onChange={(v) => setParams({ status: v || undefined })}
                  />
                  <Button
                    loading={deliveries.isRefetching && !deliveries.isFetchingNextPage}
                    onClick={() => void deliveries.refetch()}
                  >
                    {t("adTracking.refresh")}
                  </Button>
                </div>
                <QueryState query={deliveries}>
                  {() => (
                    <Show
                      when={rows().length > 0}
                      fallback={
                        <EmptyState
                          title={t("adTracking.noDeliveries")}
                          description={t("adTracking.noDeliveriesDesc")}
                        />
                      }
                    >
                      <div class="overflow-x-auto">
                        <table class={tableClass} aria-labelledby="ad-deliveries">
                          <thead>
                            <tr>
                              <Th>{t("adTracking.time")}</Th>
                              <Th>{t("adTracking.platform")}</Th>
                              <Th>{t("adTracking.event")}</Th>
                              <Th>{t("adTracking.status")}</Th>
                              <Th class="text-right">{t("adTracking.attempts")}</Th>
                              <Th class="text-right">{t("adTracking.response")}</Th>
                              <Th>{t("adTracking.lastError")}</Th>
                            </tr>
                          </thead>
                          <tbody>
                            <For each={rows()}>
                              {(d) => (
                                <tr data-status={d.status}>
                                  <td
                                    class={`${tdClass} figures text-xs whitespace-nowrap text-faint-foreground`}
                                  >
                                    {formatDateTime(d.occurred_at)}
                                  </td>
                                  <td class={`${tdClass} text-xs`}>{platformName(d.platform)}</td>
                                  <td class={`${tdClass} text-xs`}>{eventName(d.event_name)}</td>
                                  <td class={tdClass}>
                                    <StatusBadge status={d.status} />
                                  </td>
                                  <td class={`${tdClass} figures text-right`}>{d.attempts}</td>
                                  <td class={`${tdClass} figures text-right`}>
                                    {d.response_code ?? "—"}
                                  </td>
                                  <td
                                    class={`${tdClass} max-w-72 truncate text-xs`}
                                    title={d.last_error ?? ""}
                                  >
                                    {d.last_error ?? "—"}
                                  </td>
                                </tr>
                              )}
                            </For>
                          </tbody>
                        </table>
                      </div>
                      <Show when={deliveries.hasNextPage}>
                        <div>
                          <Button
                            loading={deliveries.isFetchingNextPage}
                            onClick={() => void deliveries.fetchNextPage()}
                          >
                            {t("common.loadMore")}
                          </Button>
                        </div>
                      </Show>
                    </Show>
                  )}
                </QueryState>
              </section>
            </div>
          )}
        </QueryState>
      </Show>

      <Dialog
        open={editing() !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        title={t("adTracking.configureTitle", {
          platform: platformName(editing()?.platform ?? ""),
        })}
      >
        <Show when={editing()}>
          {(c) => (
            <form
              class="flex flex-col gap-3"
              onSubmit={(e) => {
                e.preventDefault();
                save.mutate();
              }}
            >
              <For each={c().notices}>
                {(n) => {
                  const k = known(NOTICES, n);
                  return (
                    <p class="rounded-md bg-muted p-2 text-xs text-muted-foreground">
                      {k ? t(`adTracking.notices.${k}`) : n}
                    </p>
                  );
                }}
              </For>
              <For each={c().setting_fields}>
                {(f) => (
                  <TextField
                    label={fieldLabel(f)}
                    inputClass="figures"
                    value={form().settings[f] ?? ""}
                    onChange={(v) =>
                      setForm((s) => ({ ...s, settings: { ...s.settings, [f]: v } }))
                    }
                    maxLength={64}
                  />
                )}
              </For>
              <Show when={c().credential_fields.length > 0}>
                <FieldGroup
                  legend={t("adTracking.credentials")}
                  description={
                    c().has_credentials
                      ? t("adTracking.credentialsKeep")
                      : t("adTracking.credentialsHint")
                  }
                >
                  <div class="flex flex-col gap-3">
                    <For each={c().credential_fields}>
                      {(f) => (
                        <TextField
                          label={fieldLabel(f)}
                          type="password"
                          autocomplete="off"
                          inputClass="figures"
                          value={form().credentials[f] ?? ""}
                          onChange={(v) =>
                            setForm((s) => ({
                              ...s,
                              credentials: { ...s.credentials, [f]: v },
                            }))
                          }
                          maxLength={2048}
                        />
                      )}
                    </For>
                  </div>
                </FieldGroup>
              </Show>
              <FieldGroup
                legend={t("adTracking.markets")}
                description={t("adTracking.marketsHint")}
              >
                <div class="flex flex-col gap-1.5">
                  <For each={markets.data?.items ?? []}>
                    {(m) => (
                      <Checkbox
                        label={`${m.name} (${m.currency})`}
                        checked={form().market_ids.includes(m.id)}
                        onChange={(on) => toggleMarket(m.id, on)}
                      />
                    )}
                  </For>
                </div>
              </FieldGroup>
              <Checkbox
                label={t("adTracking.testMode")}
                description={t("adTracking.testModeHint")}
                checked={form().test_mode}
                onChange={(test_mode) => setForm((f) => ({ ...f, test_mode }))}
              />
              <Checkbox
                label={t("adTracking.enabled")}
                description={t("adTracking.enabledHint")}
                checked={form().enabled}
                onChange={(enabled) => setForm((f) => ({ ...f, enabled }))}
              />
              <Show when={error()}>
                <p role="alert" class="text-xs font-medium text-error-700">
                  {error()}
                </p>
              </Show>
              <div class="flex justify-end gap-2">
                <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
                <Button type="submit" variant="confirm" loading={save.isPending}>
                  {t("common.save")}
                </Button>
              </div>
            </form>
          )}
        </Show>
      </Dialog>
    </Show>
  );
}
