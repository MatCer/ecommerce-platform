import {
  Badge,
  Button,
  ConfirmDialog,
  Dialog,
  SelectField,
  showToast,
  Tabs,
  TextField,
} from "@platform/ui";
import { useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Match, Show, Switch } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { EmailBlockEditor } from "../components/EmailBlockEditor.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, formatDateTime, LOCALES, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import {
  blankContent,
  type ContentProblem,
  campaignInput,
  campaignTone,
  contentProblem,
  type LocaleContent,
  parseEmails,
} from "../lib/marketing.ts";
import { tenantKey } from "../lib/me.ts";
import { fromLocalInput } from "../lib/money.ts";
import { CONTENT_LOCALES } from "../lib/product-form.ts";
import { useSegments } from "../lib/queries.ts";

type Campaign = Schemas["Campaign"];
type Content = Record<string, LocaleContent>;

const STATS = [
  "recipients",
  "sent",
  "accepted",
  "clicked",
  "clicks",
  "unsubscribed",
  "bounced",
  "complained",
  "failed",
  "skipped",
  "uncertain",
] as const;

/** Every editable language, the campaign's own content filled in. */
function fullContent(content: Content = {}): Content {
  return Object.fromEntries(CONTENT_LOCALES.map((l) => [l, content[l] ?? blankContent()]));
}

function localeName(l: string): string {
  const known = LOCALES.find((x) => x === l);
  return known ? t(`common.locale_${known}`) : l.toUpperCase();
}

function problemText(p: ContentProblem): string {
  switch (p.kind) {
    case "no_content":
      return t("marketing.problemNoContent");
    case "subject":
      return t("marketing.problemSubject", { locale: p.locale.toUpperCase() });
    case "blocks":
      return t("marketing.problemBlocks", { locale: p.locale.toUpperCase() });
    case "block":
      return t("marketing.problemBlock", {
        locale: p.locale.toUpperCase(),
        n: String(p.index + 1),
      });
  }
}

/** Rendered email (the saved version) in a sandboxed frame: no scripts, no same-origin access. */
function CampaignPreview(props: { campaign: Campaign }) {
  const locales = () => Object.keys(props.campaign.content);
  const [locale, setLocale] = createSignal("");
  const [q, setQ] = createSignal("");
  const [subscriber, setSubscriber] = createSignal("");
  const lang = () => (locales().includes(locale()) ? locale() : (locales()[0] ?? "cs"));
  const found = createQuery(() => ({
    queryKey: tenantKey("subscriber-search", q().trim()),
    enabled: q().trim().length >= 2,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/subscribers", {
          params: {
            header: tenantHeader(),
            query: { q: q().trim(), status: "subscribed", limit: 10 },
          },
        }),
      ),
  }));
  const rendered = createQuery(() => ({
    queryKey: tenantKey(
      "campaign-preview",
      props.campaign.id,
      props.campaign.updated_at,
      subscriber() || lang(),
    ),
    queryFn: () =>
      unwrap(
        api.POST("/admin/v1/campaigns/{id}/preview", {
          params: { header: tenantHeader(), path: { id: props.campaign.id } },
          body: subscriber() ? { subscriber_id: subscriber() } : { locale: lang() },
        }),
      ),
  }));
  return (
    <section class="flex flex-col gap-3" aria-labelledby="campaign-preview">
      <h2 id="campaign-preview" class="text-sm font-semibold">
        {t("marketing.preview")}
      </h2>
      <div class="grid gap-2 sm:grid-cols-3">
        <SelectField
          label={t("marketing.language")}
          value={lang()}
          options={locales().map((l) => ({ value: l, label: localeName(l) }))}
          disabled={subscriber() !== ""}
          onChange={setLocale}
        />
        <TextField
          type="search"
          label={t("marketing.findSubscriber")}
          value={q()}
          onChange={setQ}
        />
        <SelectField
          label={t("marketing.previewAs")}
          value={subscriber()}
          options={[
            { value: "", label: t("marketing.noSubscriber") },
            ...(found.data?.items ?? []).map((s) => ({ value: s.id, label: s.email })),
          ]}
          onChange={setSubscriber}
        />
      </div>
      <QueryState query={rendered}>
        {(r) => (
          <div class="flex flex-col gap-2">
            <p class="text-sm">
              <span class="text-muted-foreground">{t("marketing.subject")}: </span>
              <span class="font-medium">{r.subject}</span>
            </p>
            <iframe
              title={t("marketing.previewFrame")}
              sandbox=""
              srcdoc={r.html}
              class="h-[36rem] w-full rounded-md border border-border bg-white"
            />
            <details>
              <summary class="cursor-pointer text-xs text-accent-700">
                {t("marketing.plainText")}
              </summary>
              <pre class="mt-1 max-h-96 overflow-auto rounded-sm bg-muted p-2 text-xs whitespace-pre-wrap">
                {r.text}
              </pre>
            </details>
          </div>
        )}
      </QueryState>
    </section>
  );
}

/**
 * A campaign (WP18): drafts are edited per language with blocks; saved campaigns can be
 * previewed as a subscriber, test-sent, scheduled and cancelled. Only drafts are editable.
 */
export default function CampaignEditor() {
  const params = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const segments = useSegments();
  const detailKey = (id: string | undefined) => tenantKey("campaign", id);

  const campaign = createQuery(() => ({
    queryKey: detailKey(params.id),
    enabled: !!params.id,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/campaigns/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
    refetchInterval: (query) => (query.state.data?.status === "sending" ? 5000 : false),
  }));

  const [name, setName] = createSignal("");
  const [segmentId, setSegmentId] = createSignal("");
  const [content, setContent] = createSignal<Content>(fullContent());
  const [locale, setLocale] = createSignal("cs");
  const [problem, setProblem] = createSignal<ContentProblem | null>(null);
  const [error, setError] = createSignal<string>();
  const [loaded, setLoaded] = createSignal("");

  const fill = (c: Campaign | null) => {
    setName(c?.name ?? "");
    setSegmentId(c?.segment_id ?? "");
    setContent(fullContent(c?.content));
    setProblem(null);
    setError(undefined);
  };
  createEffect(() => {
    const key = JSON.stringify(detailKey(params.id));
    if (loaded() === key) return;
    if (!params.id) {
      fill(null);
      setLoaded(key);
    } else if (campaign.data) {
      fill(campaign.data);
      setLocale(Object.keys(campaign.data.content)[0] ?? "cs");
      setLoaded(key);
    }
  });

  const body = () => campaignInput(name(), segmentId(), content());
  const dirty = () => {
    const c = campaign.data;
    return (
      !c ||
      JSON.stringify(body()) !==
        JSON.stringify(campaignInput(c.name, c.segment_id ?? "", c.content))
    );
  };
  const setLocaleContent = (l: string, patch: Partial<LocaleContent>) =>
    setContent({ ...content(), [l]: { ...(content()[l] ?? blankContent()), ...patch } });

  const onSaved = async (result: Campaign) => {
    qc.setQueryData(detailKey(result.id), result);
    await qc.invalidateQueries({ queryKey: tenantKey("campaigns") });
  };

  const create = submission();
  const save = createMutation(() => ({
    mutationFn: (input: Schemas["CampaignInput"]) =>
      params.id
        ? unwrap(
            api.PUT("/admin/v1/campaigns/{id}", {
              params: { header: tenantHeader(), path: { id: params.id } },
              body: input,
            }),
          )
        : unwrap(
            api.POST("/admin/v1/campaigns", {
              params: { header: create.header(input) },
              body: input,
            }),
          ),
    onSuccess: async (result) => {
      const created = !params.id;
      await onSaved(result);
      showToast({
        title: created ? t("common.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
      if (created) {
        create.done();
        navigate(`/marketing/campaigns/${result.id}`, { replace: true });
      } else {
        // Take over the server's normalized content (sanitized HTML, trimmed text).
        fill(result);
      }
    },
    onError: (err) => setError(errorMessage(err)),
  }));
  const submit = () => {
    setError(undefined);
    const p = contentProblem(content());
    setProblem(p);
    if (p && p.kind !== "no_content") setLocale(p.locale);
    if (!p) save.mutate(body());
  };

  // --- actions on a saved campaign ---------------------------------------------------------
  const [testing, setTesting] = createSignal(false);
  const [testTo, setTestTo] = createSignal("");
  const [testLocale, setTestLocale] = createSignal("");
  const [scheduling, setScheduling] = createSignal(false);
  const [when, setWhen] = createSignal<"now" | "later">("now");
  const [at, setAt] = createSignal("");
  const [cancelling, setCancelling] = createSignal(false);
  const [dialogError, setDialogError] = createSignal<string>();
  const id = () => params.id ?? "";
  const locales = () => Object.keys(campaign.data?.content ?? {});

  const sendTest = createMutation(() => ({
    mutationFn: (emails: string[]) =>
      unwrap(
        api.POST("/admin/v1/campaigns/{id}/test", {
          params: { header: tenantHeader(), path: { id: id() } },
          body: { emails, locale: testLocale() || null },
        }),
      ),
    onSuccess: (r) => {
      setTesting(false);
      showToast({
        title: t("marketing.testSent", { n: String(r.queued) }),
        closeLabel: t("common.close"),
      });
    },
    onError: (err) => setDialogError(errorMessage(err)),
  }));

  const schedule = createMutation(() => ({
    mutationFn: (when: string | null) =>
      unwrap(
        api.POST("/admin/v1/campaigns/{id}/schedule", {
          params: { header: tenantHeader(), path: { id: id() } },
          body: { at: when },
        }),
      ),
    onSuccess: async (result) => {
      setScheduling(false);
      await onSaved(result);
      showToast({ title: t("marketing.scheduled"), closeLabel: t("common.close") });
    },
    onError: (err) => setDialogError(errorMessage(err)),
  }));

  const cancel = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/campaigns/{id}/cancel", {
          params: { header: tenantHeader(), path: { id: id() } },
        }),
      ),
    onSuccess: async (result) => {
      setCancelling(false);
      await onSaved(result);
      showToast({ title: t("marketing.cancelled"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setCancelling(false);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  const segmentName = (sid: string | null | undefined) =>
    sid
      ? (segments.data?.items.find((s) => s.id === sid)?.name ?? "—")
      : t("marketing.allSubscribers");
  const isDraft = () => !params.id || campaign.data?.status === "draft";

  const editor = () => (
    <form
      class="flex max-w-4xl flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div class="grid gap-3 sm:grid-cols-2">
        <TextField
          label={t("marketing.name")}
          description={t("marketing.nameHint")}
          value={name()}
          onChange={setName}
          required
          maxLength={200}
        />
        <SelectField
          label={t("marketing.recipients")}
          value={segmentId()}
          options={[
            { value: "", label: t("marketing.allSubscribers") },
            ...(segments.data?.items ?? []).map((s) => ({ value: s.id, label: s.name })),
          ]}
          onChange={setSegmentId}
        />
      </div>
      <p class="text-xs text-muted-foreground">{t("marketing.languagesHint")}</p>
      <Tabs
        label={t("marketing.languages")}
        value={locale()}
        onChange={setLocale}
        items={CONTENT_LOCALES.map((l) => ({
          value: l,
          label: l.toUpperCase(),
          content: () => (
            <div class="flex flex-col gap-3">
              <TextField
                label={t("marketing.subject")}
                value={content()[l]?.subject ?? ""}
                maxLength={200}
                onChange={(subject) => setLocaleContent(l, { subject })}
              />
              <TextField
                label={t("marketing.preheader")}
                description={t("marketing.preheaderHint")}
                value={content()[l]?.preheader ?? ""}
                maxLength={200}
                onChange={(preheader) => setLocaleContent(l, { preheader })}
              />
              <EmailBlockEditor
                blocks={content()[l]?.blocks ?? []}
                invalid={(() => {
                  const p = problem();
                  return p?.kind === "block" && p.locale === l ? p.index : undefined;
                })()}
                onChange={(blocks) => setLocaleContent(l, { blocks })}
              />
            </div>
          ),
        }))}
      />
      <Show when={problem()}>
        {(p) => (
          <p role="alert" class="text-sm font-medium text-error-700">
            {problemText(p())}
          </p>
        )}
      </Show>
      <Show when={error()}>
        <p role="alert" class="text-sm font-medium text-error-700">
          {error()}
        </p>
      </Show>
      <div>
        <Button type="submit" variant="primary" loading={save.isPending} disabled={!name().trim()}>
          {t("common.save")}
        </Button>
      </div>
    </form>
  );

  const summary = (c: Campaign) => (
    <div class="flex max-w-4xl flex-col gap-4">
      <dl class="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[max-content_1fr]">
        <dt class="text-muted-foreground">{t("marketing.recipients")}</dt>
        <dd>{segmentName(c.segment_id)}</dd>
        <Show when={c.scheduled_at}>
          {(s) => (
            <>
              <dt class="text-muted-foreground">{t("marketing.scheduledAt")}</dt>
              <dd class="figures">{formatDateTime(s())}</dd>
            </>
          )}
        </Show>
        <Show when={c.started_at}>
          {(s) => (
            <>
              <dt class="text-muted-foreground">{t("marketing.startedAt")}</dt>
              <dd class="figures">{formatDateTime(s())}</dd>
            </>
          )}
        </Show>
        <Show when={c.finished_at}>
          {(s) => (
            <>
              <dt class="text-muted-foreground">{t("marketing.finishedAt")}</dt>
              <dd class="figures">{formatDateTime(s())}</dd>
            </>
          )}
        </Show>
        <For each={Object.entries(c.content)}>
          {([l, lc]) => (
            <>
              <dt class="text-muted-foreground">
                {t("marketing.subject")} ({l.toUpperCase()})
              </dt>
              <dd>{lc.subject}</dd>
            </>
          )}
        </For>
      </dl>
      <p class="text-xs text-muted-foreground">{t("marketing.notEditable")}</p>
      <section aria-labelledby="campaign-stats" class="flex flex-col gap-2">
        <h2 id="campaign-stats" class="text-sm font-semibold">
          {t("marketing.stats")}
        </h2>
        <dl class="grid grid-cols-2 gap-2 sm:grid-cols-4 lg:grid-cols-6">
          <For each={STATS}>
            {(k) => (
              <div class="rounded-md border border-border p-2">
                <dt class="col-label">{t(`marketing.stat_${k}`)}</dt>
                <dd class="figures text-lg font-semibold">{c.stats[k]}</dd>
              </div>
            )}
          </For>
        </dl>
      </section>
    </div>
  );

  const actions = () => (
    <Show when={campaign.data}>
      {(c) => (
        <>
          <Badge tone={campaignTone[c().status]}>{t(`marketing.cstatus_${c().status}`)}</Badge>
          <Button
            disabled={isDraft() && dirty()}
            onClick={() => {
              setDialogError(undefined);
              setTestLocale("");
              setTesting(true);
            }}
          >
            {t("marketing.sendTest")}
          </Button>
          <Show when={c().status === "draft" || c().status === "scheduled"}>
            <Button
              variant="primary"
              disabled={isDraft() && dirty()}
              onClick={() => {
                setDialogError(undefined);
                setWhen("now");
                setAt("");
                setScheduling(true);
              }}
            >
              {t("marketing.schedule")}
            </Button>
          </Show>
          <Show when={c().status === "scheduled" || c().status === "sending"}>
            <Button variant="danger" onClick={() => setCancelling(true)}>
              {t("marketing.cancelSending")}
            </Button>
          </Show>
        </>
      )}
    </Show>
  );

  return (
    <>
      <PageHeader
        title={
          params.id ? (campaign.data?.name ?? t("marketing.campaign")) : t("marketing.newCampaign")
        }
        back={{ href: "/marketing/campaigns", label: t("marketing.campaigns") }}
        actions={params.id ? actions() : undefined}
      />
      <Switch>
        <Match when={!params.id}>{editor()}</Match>
        <Match when={true}>
          <QueryState query={campaign}>
            {(c) => (
              <div class="flex flex-col gap-6">
                <Show when={c.status === "draft"} fallback={summary(c)}>
                  {editor()}
                  <Show when={dirty()}>
                    <p role="status" class="text-xs text-warning-700">
                      {t("marketing.unsavedHint")}
                    </p>
                  </Show>
                </Show>
                <CampaignPreview campaign={c} />
              </div>
            )}
          </QueryState>
        </Match>
      </Switch>

      <Dialog
        open={testing()}
        onOpenChange={setTesting}
        title={t("marketing.sendTest")}
        description={t("marketing.sendTestDesc")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            const emails = parseEmails(testTo());
            if (!emails) {
              setDialogError(t("marketing.testEmailsHint"));
              return;
            }
            setDialogError(undefined);
            sendTest.mutate(emails);
          }}
        >
          <TextField
            label={t("marketing.testEmails")}
            description={t("marketing.testEmailsHint")}
            multiline
            rows={3}
            value={testTo()}
            onChange={setTestTo}
            required
          />
          <SelectField
            label={t("marketing.language")}
            value={testLocale()}
            options={[
              { value: "", label: t("marketing.defaultLanguage") },
              ...locales().map((l) => ({ value: l, label: localeName(l) })),
            ]}
            onChange={setTestLocale}
          />
          <Show when={dialogError()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {dialogError()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setTesting(false)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" loading={sendTest.isPending}>
              {t("marketing.send")}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={scheduling()}
        onOpenChange={setScheduling}
        title={t("marketing.schedule")}
        description={t("marketing.scheduleDesc", {
          segment: segmentName(campaign.data?.segment_id),
        })}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (when() === "now") {
              schedule.mutate(null);
              return;
            }
            const iso = fromLocalInput(at());
            if (!iso) {
              setDialogError(t("marketing.pickTime"));
              return;
            }
            schedule.mutate(iso);
          }}
        >
          <SelectField
            label={t("marketing.when")}
            value={when()}
            options={[
              { value: "now", label: t("marketing.sendNow") },
              { value: "later", label: t("marketing.sendLater") },
            ]}
            onChange={(v) => setWhen(v === "later" ? "later" : "now")}
          />
          <Show when={when() === "later"}>
            <DateTimeField label={t("marketing.sendAt")} value={at()} onChange={setAt} />
          </Show>
          <Show when={dialogError()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {dialogError()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setScheduling(false)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" loading={schedule.isPending}>
              {when() === "now" ? t("marketing.sendNow") : t("marketing.schedule")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={cancelling()}
        onOpenChange={setCancelling}
        title={t("marketing.cancelTitle")}
        description={t("marketing.cancelDesc")}
        confirmLabel={t("marketing.cancelSending")}
        cancelLabel={t("common.close")}
        danger
        pending={cancel.isPending}
        onConfirm={() => cancel.mutate()}
      />
    </>
  );
}
