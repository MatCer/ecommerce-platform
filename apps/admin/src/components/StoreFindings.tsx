import { Alert, Badge, Button, Card, linkClass, showToast } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { For, Match, Show, Switch } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { mediaUrl } from "../lib/config.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { feedImporterHref, logoAction, nameAction } from "../lib/store-import.ts";
import { QueryState } from "./Page.tsx";

const COUNTS = ["product_urls", "pages_fetched", "pages_without_product", "pages_failed"] as const;

export function StoreFindings(props: { site: Schemas["SiteReport"]; runId: string }) {
  const qc = useQueryClient(),
    { me, current } = useMembership();
  const branding = createQuery(() => ({
    queryKey: tenantKey("email-branding"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/email-branding", { params: { header: tenantHeader() } })),
  }));
  const logo = createQuery(() => ({
    queryKey: tenantKey("asset", props.site.logo_asset_id),
    enabled: !!props.site.logo_asset_id,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/assets/{id}", {
          params: { header: tenantHeader(), path: { id: props.site.logo_asset_id ?? "" } },
        }),
      ),
    refetchInterval: (q) => {
      const s = q.state.data?.status;
      return s === "pending" || s === "processing" ? 2000 : false;
    },
  }));
  const apply = createMutation(() => ({
    mutationFn: (part: "name" | "logo") =>
      unwrap(
        api.POST("/admin/v1/imports/{id}/brand", {
          params: { header: tenantHeader(), path: { id: props.runId } },
          body: { [part]: true },
        }),
      ),
    onSuccess: (_brand, part) => {
      showToast({
        title: t(part === "name" ? "content.nameApplied" : "content.logoApplied"),
        closeLabel: t("common.close"),
      });
      void me.refetch();
      void qc.invalidateQueries({ queryKey: tenantKey("email-branding") });
    },
    onError: (e: unknown) =>
      showToast({ title: errorMessage(e), tone: "error", closeLabel: t("common.close") }),
  }));
  const applyButton = (part: "name" | "logo", label: string) => (
    <Button
      disabled={apply.isPending}
      loading={apply.isPending && apply.variables === part}
      onClick={() => apply.mutate(part)}
    >
      {label}
    </Button>
  );
  const inUse = () => <Badge tone="success">{t("content.inUse")}</Badge>;
  return (
    <div class="flex flex-col gap-5">
      <Show when={props.site.feed}>
        {(feed) => (
          <Alert
            tone="info"
            title={t("content.feedFound")}
            actions={
              <A class={linkClass} href={feedImporterHref(feed())}>
                {t("content.useFeedImporter")}
              </A>
            }
          >
            {t("content.feedFoundHint")}
          </Alert>
        )}
      </Show>
      <Card labelledBy="store-findings" title={t("content.site")}>
        <div class="flex flex-col gap-5">
          <Show when={nameAction(props.site.name, current()?.name)}>
            {(action) => (
              <div class="flex flex-wrap items-center justify-between gap-3">
                <dl class="flex min-w-0 flex-col gap-1">
                  <dt class="text-sm text-muted-foreground">{t("content.suggestedName")}</dt>
                  <dd class="break-words text-lg font-semibold text-heading">{props.site.name}</dd>
                  <dd class="break-words text-sm text-muted-foreground">
                    {t("content.currentName", { name: current()?.name ?? "" })}
                  </dd>
                </dl>
                <Show when={action() === "apply"} fallback={inUse()}>
                  {applyButton("name", t("content.useName"))}
                </Show>
              </div>
            )}
          </Show>
          <Show when={props.site.logo_asset_id}>
            <QueryState query={logo}>
              {(asset) => {
                const state = () =>
                  branding.isPending ? undefined : logoAction(asset, branding.data?.logo_asset_id);
                return (
                  <div class="flex flex-wrap items-center justify-between gap-3">
                    <div class="flex min-w-0 flex-col gap-2">
                      <span class="text-sm text-muted-foreground">
                        {t("content.suggestedLogo")}
                      </span>
                      <Show when={asset.variants[0]}>
                        {(v) => (
                          <img
                            src={mediaUrl(v().key)}
                            alt={t("content.suggestedLogo")}
                            class="max-h-24 max-w-full self-start rounded-md border border-border object-contain"
                          />
                        )}
                      </Show>
                    </div>
                    <Switch>
                      <Match when={state() === "in_use"}>{inUse()}</Match>
                      <Match when={state() === "failed"}>
                        <span role="status" class="text-sm text-error-700">
                          {t("content.logoFailed")}
                        </span>
                      </Match>
                      <Match when={state() === "processing"}>
                        <div class="flex flex-col gap-1">
                          <Button disabled>{t("content.useLogo")}</Button>
                          <span role="status" class="text-sm text-muted-foreground">
                            {t("content.logoProcessing")}
                          </span>
                        </div>
                      </Match>
                      <Match when={state() === "apply"}>
                        {applyButton("logo", t("content.useLogo"))}
                      </Match>
                    </Switch>
                  </div>
                );
              }}
            </QueryState>
          </Show>
          <dl class="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <For each={COUNTS}>
              {(key) => (
                <div class="flex flex-col gap-1">
                  <dt class="text-sm text-muted-foreground">{t(`content.${key}`)}</dt>
                  <dd class="figures text-lg font-semibold text-heading">{props.site[key]}</dd>
                </div>
              )}
            </For>
          </dl>
          <Show when={props.site.capped}>
            <Alert tone="info">
              {t("content.crawlCapped", { found: String(props.site.product_urls) })}
            </Alert>
          </Show>
          <Show when={props.site.timed_out}>
            <Alert tone="info">{t("content.crawlTimedOut")}</Alert>
          </Show>
          <Show when={props.site.product_urls === 0}>
            <Alert tone="warning">{t("content.noProductPages")}</Alert>
          </Show>
        </div>
      </Card>
    </div>
  );
}
