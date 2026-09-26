import {
  Alert,
  Badge,
  Button,
  Card,
  ConfirmDialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey } from "../lib/me.ts";

type Redirect = Schemas["Redirect"];

/**
 * Redirects (spec §9.5): old shop paths answered with 301/302 by the edge. Imports create
 * them from old product URLs; merchants add or remove their own here. Newest first, with
 * "load more" over the cursor pages.
 */
export default function Redirects() {
  const qc = useQueryClient();
  const [pages, setPages] = createSignal(1);
  const list = createQuery(() => ({
    queryKey: tenantKey("redirects", pages()),
    queryFn: async () => {
      const header = tenantHeader();
      const items: Redirect[] = [];
      let cursor: string | undefined;
      for (let i = 0; i < pages(); i++) {
        const page = await unwrap(
          api.GET("/admin/v1/redirects", { params: { header, query: { limit: 100, cursor } } }),
        );
        items.push(...page.items);
        cursor = page.next_cursor ?? undefined;
        if (!cursor) break;
      }
      return { items, more: Boolean(cursor) };
    },
  }));
  const [from, setFrom] = createSignal("");
  const [to, setTo] = createSignal("");
  const [code, setCode] = createSignal("301");
  const [error, setError] = createSignal<string>();
  const [deleting, setDeleting] = createSignal<Redirect>();
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("redirects") });
  const creation = submission();

  const create = createMutation(() => ({
    mutationFn: () => {
      const body = { from_path: from().trim(), to_path: to().trim(), code: Number(code()) };
      return unwrap(
        api.POST("/admin/v1/redirects", { params: { header: creation.header(body) }, body }),
      );
    },
    onSuccess: async () => {
      creation.done();
      setFrom("");
      setTo("");
      setError(undefined);
      await refresh();
      showToast({ title: t("common.created"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/redirects/{id}", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      setDeleting(undefined);
      await refresh();
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => {
      setDeleting(undefined);
      setError(contentError(e));
    },
  }));

  return (
    <>
      <PageHeader title={t("redirects.title")} description={t("redirects.description")} />
      <Card class="mb-6" title={t("redirects.new")}>
        <form
          aria-label={t("redirects.new")}
          class="grid gap-4 sm:grid-cols-[1fr_1fr_12rem_auto] sm:items-start"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate();
          }}
        >
          <TextField
            label={t("redirects.from")}
            placeholder="/stary-produkt"
            required
            value={from()}
            onChange={setFrom}
            inputClass="figures"
          />
          <TextField
            label={t("redirects.to")}
            placeholder="/p/novy-produkt"
            required
            value={to()}
            onChange={setTo}
            inputClass="figures"
          />
          <SelectField
            label={t("redirects.code")}
            value={code()}
            options={[
              { value: "301", label: t("redirects.permanent") },
              { value: "302", label: t("redirects.temporary") },
            ]}
            onChange={setCode}
          />
          <Button type="submit" variant="confirm" class="sm:mt-7" loading={create.isPending}>
            {t("common.create")}
          </Button>
        </form>
        <Show when={error()}>
          <Alert tone="error" class="mt-4">
            {error()}
          </Alert>
        </Show>
      </Card>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <EmptyState
                icon="external-link"
                title={t("redirects.empty")}
                description={t("redirects.emptyDesc")}
              />
            }
          >
            <Card
              padding="none"
              footer={
                data.more ? (
                  <Button onClick={() => setPages(pages() + 1)}>{t("redirects.more")}</Button>
                ) : undefined
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass} aria-label={t("redirects.title")}>
                  <thead>
                    <tr>
                      <Th>{t("redirects.from")}</Th>
                      <Th>{t("redirects.to")}</Th>
                      <Th>{t("redirects.code")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(r) => (
                        <tr>
                          <td
                            class={`${tdClass} font-mono text-xs font-semibold break-all text-heading`}
                          >
                            {r.from_path}
                          </td>
                          <td class={`${tdClass} font-mono text-xs break-all`}>{r.to_path}</td>
                          <td class={tdClass}>
                            <Badge tone={r.code === 301 ? "info" : "neutral"}>{r.code}</Badge>
                          </td>
                          <td class={`${tdClass} text-right`}>
                            <Button
                              variant="danger"
                              category="tertiary"
                              size="small"
                              onClick={() => setDeleting(r)}
                            >
                              {t("common.delete")}
                              <span class="sr-only">: {r.from_path}</span>
                            </Button>
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
        open={deleting() !== undefined}
        onOpenChange={(o) => !o && setDeleting(undefined)}
        title={t("redirects.deleteTitle", { path: deleting()?.from_path ?? "" })}
        description={t("redirects.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const r = deleting();
          if (r) remove.mutate(r.id);
        }}
      />
    </>
  );
}
