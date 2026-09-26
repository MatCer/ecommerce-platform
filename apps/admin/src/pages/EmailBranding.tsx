import { Button, SelectField, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, Show } from "solid-js";
import { ContentAsset } from "../components/ContentAsset.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, LOCALES, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type TemplateText = Schemas["TemplateText"];
const TEMPLATES = [
  "order_confirmation",
  "payment_reminder",
  "newsletter_confirm",
  "magic_link",
  "password_changed",
  "staff_invite",
] as const;
/** Templates whose texts may also use `{number}` (the order number). */
const ORDER_TEMPLATES = new Set(["order_confirmation", "payment_reminder"]);

function templateLabel(name: string): string {
  const k = TEMPLATES.find((x) => x === name);
  return k ? t(`emails.template_${k}`) : name;
}

const toastError = (err: unknown) =>
  showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

function Logo(props: { admin: boolean }) {
  const qc = useQueryClient();
  const branding = createQuery(() => ({
    queryKey: tenantKey("email-branding"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/email-branding", { params: { header: tenantHeader() } })),
  }));
  const [logo, setLogo] = createSignal<string>();
  const current = () => logo() ?? branding.data?.logo_asset_id ?? "";
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/email-branding", {
          params: { header: tenantHeader() },
          body: { logo_asset_id: current() || null },
        }),
      ),
    onSuccess: (saved) => {
      qc.setQueryData(tenantKey("email-branding"), saved);
      setLogo(undefined);
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: toastError,
  }));
  return (
    <section aria-labelledby="email-logo" class="flex max-w-2xl flex-col gap-3">
      <h2 id="email-logo" class="text-sm font-semibold">
        {t("emails.logo")}
      </h2>
      <p class="text-sm text-muted-foreground">{t("emails.logoDesc")}</p>
      <QueryState query={branding}>
        {() => (
          <form
            class="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <ContentAsset label={t("emails.logo")} value={current()} onChange={setLogo} />
            <Show when={props.admin}>
              <div>
                <Button type="submit" variant="confirm" loading={save.isPending}>
                  {t("emails.saveLogo")}
                </Button>
              </div>
            </Show>
          </form>
        )}
      </QueryState>
    </section>
  );
}

function Texts(props: { admin: boolean }) {
  const qc = useQueryClient();
  const texts = createQuery(() => ({
    queryKey: tenantKey("email-templates"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/email-templates", { params: { header: tenantHeader() } })),
  }));
  const [template, setTemplate] = createSignal<string>("order_confirmation");
  const [locale, setLocale] = createSignal<string>("cs");
  const [subject, setSubject] = createSignal("");
  const [intro, setIntro] = createSignal("");
  const item = (): TemplateText | undefined =>
    texts.data?.items.find((x) => x.template === template() && x.locale === locale());
  const templates = () => [...new Set(texts.data?.items.map((x) => x.template) ?? [])];

  // Load the chosen template's texts whenever the choice (or the saved data) changes.
  createEffect(() => {
    const it = item();
    setSubject(it?.subject ?? "");
    setIntro(it?.intro ?? "");
  });

  const save = createMutation(() => ({
    mutationFn: (body: { template: string; locale: string; input: Schemas["TemplateTextInput"] }) =>
      unwrap(
        api.PUT("/admin/v1/email-templates/{template}/{locale}", {
          params: {
            header: tenantHeader(),
            path: { template: body.template, locale: body.locale },
          },
          body: body.input,
        }),
      ),
    onSuccess: (saved) => {
      qc.setQueryData(tenantKey("email-templates"), {
        items: (texts.data?.items ?? []).map((x) =>
          x.template === saved.template && x.locale === saved.locale ? saved : x,
        ),
      });
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: toastError,
  }));

  return (
    <section aria-labelledby="email-texts" class="flex max-w-2xl flex-col gap-3">
      <h2 id="email-texts" class="text-sm font-semibold">
        {t("emails.texts")}
      </h2>
      <p class="text-sm text-muted-foreground">{t("emails.textsDesc")}</p>
      <QueryState query={texts}>
        {() => (
          <form
            class="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate({
                template: template(),
                locale: locale(),
                input: { subject: subject().trim() || null, intro: intro().trim() || null },
              });
            }}
          >
            <div class="grid gap-2 sm:grid-cols-2">
              <SelectField
                label={t("emails.templateSelect")}
                value={template()}
                options={templates().map((x) => ({ value: x, label: templateLabel(x) }))}
                onChange={setTemplate}
              />
              <SelectField
                label={t("marketing.language")}
                value={locale()}
                options={LOCALES.map((l) => ({ value: l, label: t(`common.locale_${l}`) }))}
                onChange={setLocale}
              />
            </div>
            <p class="text-xs text-muted-foreground">
              {ORDER_TEMPLATES.has(template())
                ? t("emails.placeholdersOrder")
                : t("emails.placeholders")}
            </p>
            <TextField
              label={t("emails.subject")}
              value={subject()}
              onChange={setSubject}
              placeholder={item()?.default_subject}
              description={t("emails.default", { text: item()?.default_subject ?? "" })}
              maxLength={200}
              disabled={!props.admin}
            />
            <TextField
              label={t("emails.intro")}
              multiline
              rows={4}
              value={intro()}
              onChange={setIntro}
              placeholder={item()?.default_intro}
              description={t("emails.default", { text: item()?.default_intro ?? "" })}
              maxLength={1000}
              disabled={!props.admin}
            />
            <p class="text-xs text-muted-foreground">{t("emails.emptyDefault")}</p>
            <Show when={props.admin}>
              <div>
                <Button type="submit" variant="confirm" loading={save.isPending}>
                  {t("emails.saveTexts")}
                </Button>
              </div>
            </Show>
          </form>
        )}
      </QueryState>
    </section>
  );
}

/** Email branding (WP18): the logo on top of every email and the editable email texts. */
export default function EmailBranding() {
  const { can } = useMembership();
  return (
    <>
      <PageHeader title={t("emails.branding")} description={t("emails.brandingDesc")} />
      <Show when={!can("admin")}>
        <p role="note" class="mb-4 text-sm text-muted-foreground">
          {t("emails.adminOnly")}
        </p>
      </Show>
      <div class="flex flex-col gap-8">
        <Logo admin={can("admin")} />
        <Texts admin={can("admin")} />
      </div>
    </>
  );
}
