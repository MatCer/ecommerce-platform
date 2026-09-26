import { Badge, Button, buttonClass, ConfirmDialog, EmptyState } from "@platform/ui";
import { A, useLocation } from "@solidjs/router";
import { createMutation } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError, firstContent, useContentPages } from "../lib/content-api.ts";

export default function ContentPages() {
  const location = useLocation();
  const blog = () => location.pathname.startsWith("/content/blog");
  const base = () => (blog() ? "/content/blog" : "/content/pages");
  const pages = useContentPages();
  const [deleting, setDeleting] = createSignal<Schemas["PageSummary"]>();
  const [error, setError] = createSignal<string>();
  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/pages/{id}", { params: { header: tenantHeader(), path: { id } } }),
      ),
    onSuccess: () => {
      setDeleting(undefined);
      void pages.refetch();
    },
    onError: (e: unknown) => {
      setDeleting(undefined);
      setError(contentError(e));
    },
  }));
  const newLink = () => (
    <A class={buttonClass({ variant: "confirm" })} href={`${base()}/new`}>
      {t(blog() ? "content.newPost" : "content.newPage")}
    </A>
  );
  const status = (p: Schemas["PageSummary"]) =>
    p.status === "published" && p.published_at && new Date(p.published_at).getTime() > Date.now()
      ? "scheduled"
      : p.status;
  return (
    <>
      <PageHeader title={t(blog() ? "content.blog" : "content.pages")} actions={newLink()} />
      <Show when={error()}>
        <p role="alert">{error()}</p>
      </Show>
      <QueryState query={pages}>
        {(data) => {
          const rows = () =>
            data.items.filter((p) => (blog() ? p.kind === "blog_post" : p.kind !== "blog_post"));
          return (
            <Show
              when={rows().length}
              fallback={
                <EmptyState
                  title={t("content.emptyPages")}
                  description={t("content.emptyPagesDesc")}
                  action={newLink()}
                />
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t(blog() ? "content.blog" : "content.pages")}>
                  <thead>
                    <tr>
                      <Th>{t("content.title")}</Th>
                      <Th>{t("content.slug")}</Th>
                      <Th>{t("content.kind")}</Th>
                      <Th>{t("content.status")}</Th>
                      <Th>{t("content.updated")}</Th>
                      <Th>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(p) => (
                        <tr>
                          <td class={tdClass}>
                            <A class="text-accent-700 hover:underline" href={`${base()}/${p.id}`}>
                              {firstContent(p.title)}
                            </A>
                          </td>
                          <td class={tdClass}>{firstContent(p.slug)}</td>
                          <td class={tdClass}>
                            <Badge>
                              {p.legal_type
                                ? t(`content.${p.legal_type}`)
                                : t(p.kind === "legal" ? "content.legalKind" : `content.${p.kind}`)}
                            </Badge>
                          </td>
                          <td class={tdClass}>
                            <Badge tone={status(p) === "published" ? "success" : "neutral"}>
                              {t(`content.${status(p)}`)}
                            </Badge>
                          </td>
                          <td class={tdClass}>{formatDateTime(p.updated_at)}</td>
                          <td class={tdClass}>
                            <Button
                              aria-label={`${t("common.delete")}: ${firstContent(p.title)}`}
                              onClick={() => setDeleting(p)}
                            >
                              {t("common.delete")}
                            </Button>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Show>
          );
        }}
      </QueryState>
      <ConfirmDialog
        open={!!deleting()}
        onOpenChange={(v) => !v && setDeleting(undefined)}
        title={t("content.deletePage")}
        description={t("content.deletePageDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const p = deleting();
          if (p) remove.mutate(p.id);
        }}
      />
    </>
  );
}
