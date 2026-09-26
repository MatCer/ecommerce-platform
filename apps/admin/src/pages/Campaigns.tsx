import { Badge, Button, ConfirmDialog, EmptyState, showToast } from "@platform/ui";
import { A, useNavigate } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { campaignTone } from "../lib/marketing.ts";
import { tenantKey } from "../lib/me.ts";
import { useSegments } from "../lib/queries.ts";

type Campaign = Schemas["Campaign"];

/** The time that matters for the campaign's status. */
function campaignWhen(c: Campaign): string {
  const at =
    c.status === "scheduled"
      ? c.scheduled_at
      : c.status === "sent" || c.status === "cancelled"
        ? (c.finished_at ?? c.started_at)
        : c.started_at;
  return at ? formatDateTime(at) : "—";
}

/** Campaigns (WP18): newsletters to a segment (or everyone) with delivery numbers. */
export default function Campaigns() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const segments = useSegments();
  const [deleting, setDeleting] = createSignal<Campaign | null>(null);
  const list = createQuery(() => ({
    queryKey: tenantKey("campaigns"),
    queryFn: () => unwrap(api.GET("/admin/v1/campaigns", { params: { header: tenantHeader() } })),
    // Sending campaigns update their numbers as batches go out.
    refetchInterval: (query) =>
      query.state.data?.items.some((c) => c.status === "sending") ? 5000 : false,
  }));
  const segmentName = (id: string | null | undefined) =>
    id
      ? (segments.data?.items.find((s) => s.id === id)?.name ?? "—")
      : t("marketing.allSubscribers");

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/campaigns/{id}", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      setDeleting(null);
      await qc.invalidateQueries({ queryKey: tenantKey("campaigns") });
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setDeleting(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  const newButton = () => (
    <Button variant="confirm" onClick={() => navigate("/marketing/campaigns/new")}>
      {t("marketing.newCampaign")}
    </Button>
  );

  return (
    <>
      <PageHeader
        title={t("marketing.campaigns")}
        description={t("marketing.campaignsDesc")}
        actions={newButton()}
      />
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <EmptyState
                title={t("marketing.noCampaigns")}
                description={t("marketing.noCampaignsDesc")}
                action={newButton()}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("marketing.name")}</Th>
                    <Th>{t("marketing.status")}</Th>
                    <Th>{t("marketing.segment")}</Th>
                    <Th>{t("marketing.when")}</Th>
                    <Th class="text-right">{t("marketing.stat_sent")}</Th>
                    <Th class="text-right">{t("marketing.stat_accepted")}</Th>
                    <Th class="text-right">{t("marketing.stat_clicked")}</Th>
                    <Th class="text-right">{t("marketing.stat_unsubscribed")}</Th>
                    <Th class="text-right">{t("marketing.stat_bounced")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(c) => (
                      <tr>
                        <td class={`${tdClass} font-medium`}>
                          <A
                            href={`/marketing/campaigns/${c.id}`}
                            class="text-accent-700 hover:underline"
                          >
                            {c.name}
                          </A>
                        </td>
                        <td class={tdClass}>
                          <Badge tone={campaignTone[c.status]}>
                            {t(`marketing.cstatus_${c.status}`)}
                          </Badge>
                        </td>
                        <td class={`${tdClass} text-xs`}>{segmentName(c.segment_id)}</td>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {campaignWhen(c)}
                        </td>
                        <td class={`${tdClass} figures text-right`}>{c.stats.sent}</td>
                        <td class={`${tdClass} figures text-right`}>{c.stats.accepted}</td>
                        <td class={`${tdClass} figures text-right`}>{c.stats.clicked}</td>
                        <td class={`${tdClass} figures text-right`}>{c.stats.unsubscribed}</td>
                        <td class={`${tdClass} figures text-right`}>{c.stats.bounced}</td>
                        <td class={`${tdClass} text-right`}>
                          <Show when={c.status === "draft"}>
                            <Button category="tertiary" onClick={() => setDeleting(c)}>
                              {t("common.delete")}
                              <span class="sr-only">: {c.name}</span>
                            </Button>
                          </Show>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
        )}
      </QueryState>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("marketing.deleteCampaign", { name: deleting()?.name ?? "" })}
        description={t("marketing.deleteCampaignDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const c = deleting();
          if (c) remove.mutate(c.id);
        }}
      />
    </>
  );
}
