import { Alert, Button, Card, EmptyState, linkClass, showToast } from "@platform/ui";
import { createMutation, createQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

const CHANNELS = { google: "Google Merchant", heureka: "Heureka", zbozi: "Zboží" };
export default function ExportFeeds() {
  const query = createQuery(() => ({
    queryKey: tenantKey("feeds"),
    queryFn: () => unwrap(api.GET("/admin/v1/feeds", { params: { header: tenantHeader() } })),
  }));
  const [error, setError] = createSignal<string>();
  const { can } = useMembership();
  const regenerate = createMutation(() => ({
    mutationFn: () =>
      unwrap(api.POST("/admin/v1/feeds/regenerate", { params: { header: tenantHeader() } })),
    onSuccess: () => {
      setError(undefined);
      void query.refetch();
      showToast({ title: t("content.queued"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const copy = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url);
      showToast({ title: t("content.copied"), closeLabel: t("common.close") });
    } catch (e) {
      setError(contentError(e));
    }
  };
  return (
    <>
      <PageHeader
        title={t("content.feeds")}
        actions={
          <Show when={can("admin")}>
            <Button icon="retry" loading={regenerate.isPending} onClick={() => regenerate.mutate()}>
              {t("content.regenerate")}
            </Button>
          </Show>
        }
      />
      <Show when={error()}>
        <Alert tone="error" class="mb-4">
          {error()}
        </Alert>
      </Show>
      <QueryState query={query}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState icon="export" title={t("content.noEntries")} />}
          >
            <Card padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t("content.feeds")}>
                  <thead>
                    <tr>
                      <Th>{t("content.market")}</Th>
                      <Th>{t("content.channel")}</Th>
                      <Th>{t("content.url")}</Th>
                      <Th class="text-right">{t("content.items")}</Th>
                      <Th class="text-right">{t("content.size")}</Th>
                      <Th class="text-right">{t("content.generated")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(f) => (
                        <tr>
                          <td class={`${tdClass} font-mono text-xs uppercase`}>{f.market_code}</td>
                          <td class={`${tdClass} font-semibold text-heading`}>
                            {CHANNELS[f.channel]}
                          </td>
                          <td class={`${tdClass} max-w-96`}>
                            <Show when={f.url} fallback={t("content.noDomain")}>
                              {(url) => (
                                <div class="flex items-center gap-2">
                                  <a
                                    href={url()}
                                    class={`min-w-0 break-all ${linkClass}`}
                                    target="_blank"
                                    rel="noreferrer"
                                  >
                                    {url()}
                                  </a>
                                  <Button
                                    category="tertiary"
                                    size="small"
                                    aria-label={`${t("content.copy")}: ${f.market_code} ${CHANNELS[f.channel]}`}
                                    onClick={() => void copy(url())}
                                  >
                                    {t("content.copy")}
                                  </Button>
                                </div>
                              )}
                            </Show>
                          </td>
                          <td class={`${tdClass} figures text-right`}>{f.items ?? "—"}</td>
                          <td class={`${tdClass} figures text-right whitespace-nowrap`}>
                            {f.bytes == null ? "—" : `${f.bytes.toLocaleString()} B`}
                          </td>
                          <td class={`${tdClass} figures text-right text-muted-foreground`}>
                            {f.generated_at
                              ? formatDateTime(f.generated_at)
                              : t("content.notGenerated")}
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
    </>
  );
}
