import {
  Badge,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  SearchBox,
  SelectField,
  showToast,
  type Tone,
} from "@platform/ui";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, LOCALES, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";

type Subscriber = Schemas["Subscriber"];
type Status = Schemas["Status"];
const STATUSES: Status[] = ["subscribed", "pending", "unsubscribed", "bounced", "complained"];
const subscriberTone: Record<Status, Tone> = {
  subscribed: "success",
  pending: "info",
  unsubscribed: "neutral",
  bounced: "error",
  complained: "warning",
};

/**
 * Newsletter subscribers (WP18) with their consent evidence. Staff can look up and unsubscribe
 * people; owners/admins can export the filtered list (after a recent sign-in).
 */
export default function Subscribers() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const markets = useMarkets();
  const [q, setQ] = createSignal("");
  const [status, setStatus] = createSignal<Status | "">("");
  const [locale, setLocale] = createSignal("");
  const [market, setMarket] = createSignal("");
  const [leaving, setLeaving] = createSignal<Subscriber | null>(null);
  const [exporting, setExporting] = createSignal(false);

  const filters = () => ({
    q: q().trim() || undefined,
    status: status() || undefined,
    locale: locale() || undefined,
    market_id: market() || undefined,
  });
  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("subscribers", filters()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/subscribers", {
          params: { header: tenantHeader(), query: { ...filters(), limit: 50, cursor: pageParam } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];
  const marketName = (id: string) => markets.data?.items.find((m) => m.id === id)?.name ?? id;

  const unsubscribe = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/subscribers/{id}/unsubscribe", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      setLeaving(null);
      await qc.invalidateQueries({ queryKey: tenantKey("subscribers") });
      showToast({ title: t("marketing.unsubscribed"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setLeaving(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  // The API asks for a recent sign-in (401 reauth_required); the API client shows the
  // re-authentication dialog and retries, so this only sees the final outcome.
  const exportCsv = async () => {
    setExporting(true);
    try {
      const csv = await unwrap(
        api.GET("/admin/v1/subscribers/export", {
          params: { header: tenantHeader(), query: filters() },
          parseAs: "text",
        }),
      );
      const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = "subscribers.csv";
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (err) {
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    } finally {
      setExporting(false);
    }
  };

  return (
    <>
      <PageHeader
        title={t("marketing.subscribers")}
        description={t("marketing.subscribersDesc")}
        actions={
          <Show when={can("admin")}>
            <Button icon="export" loading={exporting()} onClick={() => void exportCsv()}>
              {t("marketing.export")}
            </Button>
          </Show>
        }
      />
      <div class="mb-4 grid items-end gap-x-2 gap-y-3 sm:grid-cols-2 lg:grid-cols-[minmax(15rem,1fr)_repeat(3,12rem)]">
        <SearchBox
          label={t("marketing.searchEmail")}
          placeholder={t("marketing.searchEmail")}
          clearLabel={t("common.clearSearch")}
          value={q()}
          onChange={setQ}
        />
        <SelectField
          label={t("marketing.status")}
          value={status()}
          options={[
            { value: "", label: t("marketing.all") },
            ...STATUSES.map((s) => ({ value: s, label: t(`marketing.status_${s}`) })),
          ]}
          onChange={(v) => setStatus(STATUSES.find((s) => s === v) ?? "")}
        />
        <SelectField
          label={t("marketing.language")}
          value={locale()}
          options={[
            { value: "", label: t("marketing.all") },
            ...LOCALES.map((l) => ({ value: l, label: t(`common.locale_${l}`) })),
          ]}
          onChange={setLocale}
        />
        <SelectField
          label={t("marketing.market")}
          value={market()}
          options={[
            { value: "", label: t("marketing.all") },
            ...(markets.data?.items ?? []).map((m) => ({ value: m.id, label: m.name })),
          ]}
          onChange={setMarket}
        />
      </div>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                icon="user"
                title={t("marketing.noSubscribers")}
                description={t("marketing.noSubscribersDesc")}
              />
            }
          >
            <Card
              padding="none"
              footer={
                <>
                  <p role="status" class="figures text-sm text-muted-foreground">
                    {t("marketing.total", { n: String(data.pages[0]?.total ?? 0) })}
                  </p>
                  <Show when={list.hasNextPage}>
                    <Button
                      class="ml-auto"
                      loading={list.isFetchingNextPage}
                      onClick={() => void list.fetchNextPage()}
                    >
                      {t("common.loadMore")}
                    </Button>
                  </Show>
                </>
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("marketing.email")}</Th>
                      <Th>{t("marketing.status")}</Th>
                      <Th>{t("marketing.language")}</Th>
                      <Th>{t("marketing.market")}</Th>
                      <Th>{t("marketing.source")}</Th>
                      <Th>{t("marketing.since")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(s) => (
                        <tr class="hover:bg-subtle">
                          <td class={`${tdClass} font-semibold text-heading`}>{s.email}</td>
                          <td class={tdClass}>
                            <Badge tone={subscriberTone[s.status]}>
                              {t(`marketing.status_${s.status}`)}
                            </Badge>
                          </td>
                          <td class={`${tdClass} font-mono text-xs uppercase`}>{s.locale}</td>
                          <td class={tdClass}>{marketName(s.market_id)}</td>
                          <td class={`${tdClass} text-muted-foreground`}>{s.source}</td>
                          <td
                            class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}
                            title={t("marketing.consentVersion", { v: s.text_version })}
                          >
                            {formatDateTime(s.confirmed_at ?? s.requested_at)}
                          </td>
                          <td class={`${tdClass} text-right`}>
                            <Show when={s.status === "subscribed" || s.status === "pending"}>
                              <Button
                                category="tertiary"
                                size="small"
                                onClick={() => setLeaving(s)}
                              >
                                {t("marketing.unsubscribe")}
                                <span class="sr-only">: {s.email}</span>
                              </Button>
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

      <ConfirmDialog
        open={leaving() !== null}
        onOpenChange={(o) => !o && setLeaving(null)}
        title={t("marketing.unsubscribeTitle", { email: leaving()?.email ?? "" })}
        description={t("marketing.unsubscribeDesc")}
        confirmLabel={t("marketing.unsubscribe")}
        cancelLabel={t("common.cancel")}
        danger
        pending={unsubscribe.isPending}
        onConfirm={() => {
          const s = leaving();
          if (s) unsubscribe.mutate(s.id);
        }}
      />
    </>
  );
}
