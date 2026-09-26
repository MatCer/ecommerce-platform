import { Button, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery } from "@tanstack/solid-query";
import { createEffect, createSignal, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { parseSynonyms, synonymsText } from "../lib/content-form.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
export default function SearchSynonyms() {
  const { can } = useMembership();
  const query = createQuery(() => ({
    queryKey: tenantKey("synonyms"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/search/synonyms", { params: { header: tenantHeader() } })),
  }));
  const [text, setText] = createSignal(""),
    [loaded, setLoaded] = createSignal(""),
    [error, setError] = createSignal<string>();
  createEffect(() => {
    const key = JSON.stringify(tenantKey("synonyms"));
    if (query.data && loaded() !== key) {
      setText(synonymsText(query.data.groups));
      setLoaded(key);
      setError(undefined);
    }
  });
  const parsed = () => parseSynonyms(text());
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/search/synonyms", {
          params: { header: tenantHeader() },
          body: { groups: parsed().groups },
        }),
      ),
    onSuccess: () => {
      void query.refetch();
      setError(undefined);
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  return (
    <>
      <PageHeader title={t("content.search")} description={t("content.synonymHelp")} />
      <QueryState query={query}>
        {() => (
          <form
            class="flex max-w-2xl flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (can("admin") && !parsed().error) save.mutate();
            }}
          >
            <TextField
              label={t("content.synonymGroups")}
              multiline
              rows={12}
              value={text()}
              onChange={setText}
              readOnly={!can("admin")}
              error={
                parsed().error
                  ? t("content.synonymError", { line: parsed().line ?? "—" })
                  : undefined
              }
            />
            <p class="text-sm text-muted-foreground">{t("content.synonymDelay")}</p>
            <Show when={!can("admin")}>
              <p class="text-sm">{t("content.readOnly")}</p>
            </Show>
            <Show when={error()}>
              <p role="alert" class="text-error-700">
                {error()}
              </p>
            </Show>
            <Show when={can("admin")}>
              <div>
                <Button
                  type="submit"
                  variant="confirm"
                  disabled={!!parsed().error}
                  loading={save.isPending}
                >
                  {t("common.save")}
                </Button>
              </div>
            </Show>
          </form>
        )}
      </QueryState>
    </>
  );
}
