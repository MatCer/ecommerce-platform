import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  linkClass,
  showToast,
  TextField,
} from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { legalInput, missingLabel } from "../lib/content-form.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { CONTENT_LOCALES } from "../lib/product-form.ts";

const FIELDS = [
  "company_name",
  "company_id",
  "street",
  "city",
  "postal_code",
  "country",
  "email",
  "phone",
  "registry",
  "returns_address",
] as const;
const TYPES = ["terms", "privacy", "cookies", "withdrawal", "complaints", "reviews"] as const;
const FIX = {
  legal_entity: "#legal-entity",
  legal_pages: "/content/pages",
  tax_profile: "/settings/tax",
  gpsr: "/products",
};
export default function ContentLegal() {
  const { can } = useMembership();
  const qc = useQueryClient();
  const report = createQuery(() => ({
    queryKey: tenantKey("go-live"),
    queryFn: () => unwrap(api.GET("/admin/v1/go-live", { params: { header: tenantHeader() } })),
  }));
  const entity = createQuery(() => ({
    queryKey: tenantKey("legal-entity"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/legal-entity", { params: { header: tenantHeader() } })),
  }));
  const [draft, setDraft] = createSignal<Schemas["LegalEntity"]>({
    company_name: "",
    company_id: "",
    street: "",
    city: "",
    postal_code: "",
    country: "",
    email: "",
    phone: "",
    registry: "",
    returns_address: "",
  });
  const [loaded, setLoaded] = createSignal(""),
    [error, setError] = createSignal<string>(),
    [locales, setLocales] = createSignal<string[]>(["cs"]),
    [installed, setInstalled] = createSignal<Schemas["InstallResult"]>();
  createEffect(() => {
    const key = JSON.stringify(tenantKey("legal-entity"));
    if (entity.data && loaded() !== key) {
      setDraft(legalInput(entity.data));
      setLoaded(key);
      setInstalled(undefined);
      setError(undefined);
    }
  });
  const save = createMutation(() => ({
    mutationFn: () => {
      setError(undefined);
      return unwrap(
        api.PUT("/admin/v1/legal-entity", {
          params: { header: tenantHeader() },
          body: legalInput(draft()),
        }),
      );
    },
    onSuccess: () => {
      void entity.refetch();
      void report.refetch();
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const install = createMutation(() => ({
    mutationFn: () => {
      setError(undefined);
      return unwrap(
        api.POST("/admin/v1/legal/templates/install", {
          params: { header: tenantHeader() },
          body: { locales: locales(), types: [] },
        }),
      );
    },
    onSuccess: (result) => {
      void qc.invalidateQueries({ queryKey: tenantKey("content-pages") });
      setInstalled(result);
      void report.refetch();
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const labels = () =>
    Object.fromEntries([...FIELDS, ...TYPES].map((key) => [key, t(`content.${key}`)]));
  return (
    <>
      <PageHeader title={t("content.legal")} />
      <div class="flex max-w-4xl flex-col gap-4">
        <QueryState query={report}>
          {(data) => (
            <Card
              aria-label={t("content.checklist")}
              padding="none"
              title={t("content.checklist")}
              actions={
                <Badge tone={data.ready ? "success" : "warning"}>
                  {t(data.ready ? "content.ready" : "content.notReady")}
                </Badge>
              }
              footer={<p class="text-sm text-muted-foreground">{data.notice}</p>}
            >
              <ul class="divide-y divide-border">
                <For each={data.checks}>
                  {(check) => (
                    <li class="px-4 py-3">
                      <div class="flex flex-wrap items-center gap-2">
                        <h3 class="text-sm font-semibold text-heading">
                          {t(`content.${check.code}`)}
                        </h3>
                        <Badge tone={check.ok ? "success" : "warning"}>
                          {t(check.ok ? "content.ok" : "content.missing")}
                        </Badge>
                        <A class={`ml-auto text-sm ${linkClass}`} href={FIX[check.code]}>
                          {t("content.fix")}
                          <span class="sr-only">: {t(`content.${check.code}`)}</span>
                        </A>
                      </div>
                      <ul class="mt-1 list-inside list-disc text-sm text-muted-foreground">
                        <For each={check.missing}>
                          {(value) => <li>{missingLabel(value, labels())}</li>}
                        </For>
                      </ul>
                      <Show when={check.missing_count > check.missing.length}>
                        <p class="text-sm text-muted-foreground">
                          {t("content.truncated")} ({check.missing_count})
                        </p>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Card>
          )}
        </QueryState>
        <Show when={error()}>
          <Alert tone="error">{error()}</Alert>
        </Show>
        <Card
          id="legal-entity"
          aria-label={t("content.legal_entity")}
          title={t("content.legal_entity")}
        >
          <Show when={!can("admin")}>
            <Alert tone="info" class="mb-4">
              {t("content.readOnly")}
            </Alert>
          </Show>
          <QueryState query={entity}>
            {() => (
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  if (can("admin")) save.mutate();
                }}
                class="flex flex-col gap-5"
              >
                <div class="grid gap-5 sm:grid-cols-2">
                  <For each={FIELDS}>
                    {(field) => (
                      <TextField
                        label={t(`content.${field}`)}
                        value={draft()[field]}
                        readOnly={!can("admin")}
                        type={field === "email" ? "email" : field === "phone" ? "tel" : "text"}
                        maxLength={field === "country" ? 2 : undefined}
                        onChange={(value) =>
                          setDraft((d) => ({
                            ...d,
                            [field]: field === "country" ? value.toUpperCase() : value,
                          }))
                        }
                      />
                    )}
                  </For>
                </div>
                <Show when={can("admin")}>
                  <div class="border-t border-border pt-4">
                    <Button type="submit" variant="confirm" loading={save.isPending}>
                      {t("common.save")}
                    </Button>
                  </div>
                </Show>
              </form>
            )}
          </QueryState>
        </Card>
        <Card
          aria-label={t("content.install")}
          title={t("content.install")}
          footer={
            <A href="/content/pages" class={`text-sm ${linkClass}`}>
              {t("content.pages")} →
            </A>
          }
        >
          <div class="flex flex-col gap-4">
            <Alert tone="warning">{t("content.notice")}</Alert>
            <Show when={can("admin")}>
              <div class="flex flex-wrap gap-4">
                <For each={CONTENT_LOCALES}>
                  {(l) => (
                    <Checkbox
                      label={l.toUpperCase()}
                      checked={locales().includes(l)}
                      onChange={(on) =>
                        setLocales((ls) => (on ? [...ls, l] : ls.filter((x) => x !== l)))
                      }
                    />
                  )}
                </For>
              </div>
              <Button
                class="self-start"
                disabled={!locales().length}
                loading={install.isPending}
                onClick={() => install.mutate()}
              >
                {t("content.install")}
              </Button>
            </Show>
            <Show when={installed()}>
              {(result) => (
                <Alert
                  tone="success"
                  title={t("content.installed", {
                    created: result().created.length,
                    skipped: result().skipped.length,
                  })}
                >
                  {result().notice}
                </Alert>
              )}
            </Show>
          </div>
        </Card>
      </div>
    </>
  );
}
