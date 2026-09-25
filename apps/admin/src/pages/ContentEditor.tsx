import { Button, SelectField, showToast, Tabs, TextField } from "@platform/ui";
import { useLocation, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { BlockEditor } from "../components/BlockEditor.tsx";
import { ContentAsset } from "../components/ContentAsset.tsx";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { pageInput, slugify } from "../lib/content-form.ts";
import { tenantKey } from "../lib/me.ts";
import { fromLocalInput, toLocalInput } from "../lib/money.ts";
import { CONTENT_LOCALES } from "../lib/product-form.ts";

type Translation = Schemas["PageTranslation"];
export default function ContentEditor() {
  const params = useParams(),
    location = useLocation(),
    navigate = useNavigate(),
    qc = useQueryClient();
  const blog = () => location.pathname.startsWith("/content/blog");
  const base = () => (blog() ? "/content/blog" : "/content/pages");
  const blank = (): Schemas["PageInput"] => ({
    kind: blog() ? "blog_post" : "page",
    status: "draft",
    translations: CONTENT_LOCALES.map((locale) => ({ locale, title: "", slug: "", blocks: [] })),
  });
  const [draft, setDraft] = createSignal(blank()),
    [loaded, setLoaded] = createSignal(""),
    [error, setError] = createSignal<string>(),
    [locale, setLocale] = createSignal("cs");
  const [editedSlugs, setEditedSlugs] = createSignal<Set<string>>(new Set());
  const page = createQuery(() => ({
    queryKey: tenantKey("content-page", params.id),
    enabled: !!params.id,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/pages/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
  }));
  createEffect(() => {
    const key = JSON.stringify(tenantKey("edit", params.id ?? location.pathname));
    if (loaded() === key) return;
    if (!params.id) {
      setDraft(blank());
      setEditedSlugs(new Set<string>());
      setLoaded(key);
    } else if (page.data) {
      const p = page.data;
      setDraft({
        ...p,
        translations: [
          ...p.translations,
          ...CONTENT_LOCALES.filter((l) => !p.translations.some((tr) => tr.locale === l)).map(
            (locale) => ({ locale, title: "", slug: "", blocks: [] }),
          ),
        ],
      });
      setEditedSlugs(new Set(p.translations.map((tr) => tr.locale)));
      setLoaded(key);
    }
    setError(undefined);
  });
  const translation = (l: string): Translation =>
    draft().translations.find((tr) => tr.locale === l) ?? {
      locale: l,
      title: "",
      slug: "",
      blocks: [],
    };
  const update = (l: string, patch: Partial<Translation>) =>
    setDraft((d) => ({
      ...d,
      translations: d.translations.map((tr) => (tr.locale === l ? { ...tr, ...patch } : tr)),
    }));
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  const creation = submission();
  const save = createMutation(() => ({
    mutationFn: async () => {
      setError(undefined);
      const d = draft();
      const body = pageInput(d);
      const header = params.id ? tenantHeader() : creation.header(body);
      const key = tenantKey("content-pages");
      const detailKey = tenantKey("content-page");
      const destination = base();
      const result = await (params.id
        ? unwrap(
            api.PUT("/admin/v1/pages/{id}", { params: { header, path: { id: params.id } }, body }),
          )
        : unwrap(api.POST("/admin/v1/pages", { params: { header: creation.header(body) }, body })));
      return { result, key, detailKey, destination };
    },
    onSuccess: async ({ result, key, detailKey, destination }) => {
      qc.setQueryData([...detailKey, result.id], result);
      creation.done();
      await qc.invalidateQueries({ queryKey: key });
      if (!alive) return;
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
      navigate(destination);
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const editor = () => (
    <form
      class="flex max-w-4xl flex-col gap-5"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <Show when={draft().kind === "legal"}>
        <p role="note" class="rounded-md border border-warning-700 bg-warning-50 p-3 text-sm">
          {t("content.notice")}
        </p>
      </Show>
      <Show when={error()}>
        <p role="alert" class="text-sm text-error-700">
          {error()}
        </p>
      </Show>
      <div class="grid gap-3 sm:grid-cols-2">
        <SelectField
          label={t("content.status")}
          value={draft().status ?? "draft"}
          options={[
            { value: "draft", label: t("content.draft") },
            { value: "published", label: t("content.published") },
          ]}
          onChange={(v) =>
            setDraft((d) => ({ ...d, status: v === "published" ? "published" : "draft" }))
          }
        />
        <DateTimeField
          label={t("content.publishDate")}
          value={toLocalInput(draft().published_at)}
          onChange={(v) => setDraft((d) => ({ ...d, published_at: fromLocalInput(v) }))}
        />
      </div>
      <Show when={blog()}>
        <ContentAsset
          label={t("content.cover")}
          value={draft().image_asset_id ?? ""}
          onChange={(image_asset_id) =>
            setDraft((d) => ({ ...d, image_asset_id: image_asset_id || null }))
          }
        />
      </Show>
      <Tabs
        label={t("content.translations")}
        value={locale()}
        onChange={setLocale}
        items={CONTENT_LOCALES.map((l) => ({
          value: l,
          label: l.toUpperCase(),
          content: () => (
            <div class="flex flex-col gap-3">
              <TextField
                label={t("content.title")}
                value={translation(l).title}
                onChange={(title) =>
                  update(l, { title, ...(!editedSlugs().has(l) ? { slug: slugify(title) } : {}) })
                }
              />
              <TextField
                label={t("content.slug")}
                value={translation(l).slug}
                onChange={(slug) => {
                  setEditedSlugs(new Set([...editedSlugs(), l]));
                  update(l, { slug });
                }}
              />
              <TextField
                label={t("content.excerpt")}
                multiline
                value={translation(l).excerpt ?? ""}
                onChange={(excerpt) => update(l, { excerpt })}
              />
              <div class="grid gap-3 sm:grid-cols-2">
                <TextField
                  label={t("content.seoTitle")}
                  value={translation(l).seo_title ?? ""}
                  onChange={(seo_title) => update(l, { seo_title })}
                />
                <TextField
                  label={t("content.seoDescription")}
                  multiline
                  value={translation(l).seo_description ?? ""}
                  onChange={(seo_description) => update(l, { seo_description })}
                />
              </div>
              <BlockEditor
                blocks={translation(l).blocks ?? []}
                onChange={(blocks) => update(l, { blocks })}
              />
            </div>
          ),
        }))}
      />
      <div>
        <Button type="submit" variant="primary" loading={save.isPending}>
          {t("common.save")}
        </Button>
      </div>
    </form>
  );
  return (
    <>
      <PageHeader
        title={t(params.id ? "content.editPage" : blog() ? "content.newPost" : "content.newPage")}
        back={{ href: base(), label: t(blog() ? "content.blog" : "content.pages") }}
      />
      <Show when={params.id} fallback={editor()}>
        <QueryState query={page}>{() => editor()}</QueryState>
      </Show>
    </>
  );
}
