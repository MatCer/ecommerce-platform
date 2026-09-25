import { Button, EmptyState, showToast } from "@platform/ui";
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
            <Button loading={regenerate.isPending} onClick={() => regenerate.mutate()}>
              {t("content.regenerate")}
            </Button>
          </Show>
        }
      />
      <Show when={error()}>
        <p role="alert" class="text-error-700">
          {error()}
        </p>
      </Show>
      <QueryState query={query}>
        {(data) => (
          <Show when={data.items.length} fallback={<EmptyState title={t("content.noEntries")} />}>
            <div class="overflow-x-auto">
              <table class={tableClass} aria-label={t("content.feeds")}>
                <thead>
                  <tr>
                    <Th>{t("content.market")}</Th>
                    <Th>{t("content.channel")}</Th>
                    <Th>{t("content.url")}</Th>
                    <Th>{t("content.items")}</Th>
                    <Th>{t("content.size")}</Th>
                    <Th>{t("content.generated")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(f) => (
                      <tr>
                        <td class={tdClass}>{f.market_code}</td>
                        <td class={tdClass}>{CHANNELS[f.channel]}</td>
                        <td class={`${tdClass} max-w-80 break-all`}>
                          <Show when={f.url} fallback={t("content.noDomain")}>
                            {(url) => (
                              <>
                                <a
                                  href={url()}
                                  class="text-accent-700 hover:underline"
                                  target="_blank"
                                  rel="noreferrer"
                                >
                                  {url()}
                                </a>
                                <Button
                                  aria-label={`${t("content.copy")}: ${f.market_code} ${CHANNELS[f.channel]}`}
                                  onClick={() => void copy(url())}
                                >
                                  {t("content.copy")}
                                </Button>
                              </>
                            )}
                          </Show>
                        </td>
                        <td class={tdClass}>{f.items ?? "—"}</td>
                        <td class={tdClass}>
                          {f.bytes == null ? "—" : `${f.bytes.toLocaleString()} B`}
                        </td>
                        <td class={tdClass}>
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
          </Show>
        )}
      </QueryState>
    </>
  );
}
