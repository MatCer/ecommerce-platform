/**
 * AI assistant for one entity (product, category, page, menu): generate a description, SEO
 * title/meta or translations as a proposal, review before/after per field, accept the chosen
 * fields (saved through the regular services) or discard. Also lists the fields whose text is
 * AI-generated (AI Act transparency).
 */
import { Badge, Button, Checkbox, SelectField, Spinner, showToast } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Show } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import {
  type Change,
  displayValue,
  type EntityType,
  isHtml,
  kindsFor,
  type ProposalKind,
  useAiMarks,
  useAiUsage,
  useProposal,
} from "../lib/ai.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES } from "../lib/product-form.ts";
import { sanitizeHtml } from "./RichText.tsx";

const TONES: Schemas["Tone"][] = ["neutral", "friendly", "premium", "technical", "playful"];
const LENGTHS: Schemas["Length"][] = ["short", "medium", "long"];

type FieldKey =
  | "name"
  | "slug"
  | "short_description"
  | "description_html"
  | "seo_title"
  | "seo_description"
  | "title"
  | "excerpt"
  | "blocks"
  | "labels";

const FIELDS: readonly string[] = [
  "name",
  "slug",
  "short_description",
  "description_html",
  "seo_title",
  "seo_description",
  "title",
  "excerpt",
  "blocks",
  "labels",
];

/** "Description (CS)". */
export function fieldLabel(field: string, locale: string): string {
  const name = FIELDS.includes(field) ? t(`ai.field_${field as FieldKey}`) : field;
  return `${name} (${locale.toUpperCase()})`;
}

function Value(props: { field: string; value: unknown; muted?: boolean }) {
  const text = () => displayValue(props.field, props.value);
  return (
    <div
      class="min-h-8 rounded-md border border-border bg-card px-2 py-1 text-sm break-words [&_li]:ml-4 [&_li]:list-disc [&_p]:mb-1"
      classList={{ "text-muted-foreground": props.muted }}
    >
      <Show when={text()} fallback={<span class="italic">{t("ai.empty")}</span>}>
        <Show when={isHtml(props.field)} fallback={text()}>
          {/* Server-sanitized; sanitized again before rendering. */}
          <div innerHTML={sanitizeHtml(text())} />
        </Show>
      </Show>
    </div>
  );
}

/** Marks the fields of `changes` checked by default (everything but untouched values). */
function key(c: Change): string {
  return `${c.locale}:${c.field}`;
}

export function AiPanel(props: {
  entityType: EntityType;
  entityId: string;
  /** Locale preselected as the source/target language. */
  locale?: string;
  /** Called after fields were saved (reload the entity). */
  onAccepted: () => void;
  /** Shown above the accept button (e.g. unsaved form edits get replaced). */
  acceptHint?: string;
}) {
  const qc = useQueryClient();
  const usage = useAiUsage();
  const marks = useAiMarks(
    () => props.entityType,
    () => props.entityId,
  );
  const kinds = () => kindsFor(props.entityType);
  const [kind, setKind] = createSignal<ProposalKind>(kindsFor(props.entityType)[0] ?? "translate");
  const [locale, setLocale] = createSignal(props.locale ?? "cs");
  const [targets, setTargets] = createSignal<string[]>([]);
  const [tone, setTone] = createSignal<Schemas["Tone"]>("neutral");
  const [length, setLength] = createSignal<Schemas["Length"]>("medium");
  const [proposalId, setProposalId] = createSignal<string | null>(null);
  const [chosen, setChosen] = createSignal<Set<string>>(new Set());
  const [error, setError] = createSignal<unknown>();
  const proposal = useProposal(proposalId);
  const describes = () => kind() === "product_description" || kind() === "category_description";

  // New targets default to every other content language.
  createEffect(() => setTargets(CONTENT_LOCALES.filter((l) => l !== locale())));

  // A ready proposal starts with every proposed field chosen.
  createEffect(() => {
    const p = proposal.data;
    if (p?.status === "ready") setChosen(new Set(p.changes.map(key)));
  });

  const generate = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/ai/proposals", {
          params: { header: tenantHeader() },
          body: {
            kind: kind(),
            entity_type: props.entityType,
            entity_id: props.entityId,
            locale: locale(),
            target_locales: kind() === "translate" ? targets() : [],
            tone: tone(),
            length: length(),
          },
        }),
      ),
    onMutate: () => setError(undefined),
    onSuccess: (p) => {
      qc.setQueryData(tenantKey("ai-proposal", p.id), p);
      setProposalId(p.id);
    },
    onError: (e: unknown) => setError(e),
    onSettled: () => void qc.invalidateQueries({ queryKey: tenantKey("ai-usage") }),
  }));

  const accept = createMutation(() => ({
    mutationFn: () => {
      const p = proposal.data;
      if (!p) throw new ApiError(400, "no_fields");
      return unwrap(
        api.POST("/admin/v1/ai/proposals/{id}/accept", {
          params: { header: tenantHeader(), path: { id: p.id } },
          body: {
            fields: p.changes
              .filter((c) => chosen().has(key(c)))
              .map((c) => ({ locale: c.locale, field: c.field })),
          },
        }),
      );
    },
    onMutate: () => setError(undefined),
    onSuccess: () => {
      setProposalId(null);
      void marks.refetch();
      showToast({ title: t("ai.accepted"), closeLabel: t("common.close") });
      props.onAccepted();
    },
    onError: (e: unknown) => setError(e),
  }));

  const discard = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/ai/proposals/{id}/discard", {
          params: { header: tenantHeader(), path: { id: proposalId() ?? "" } },
        }),
      ),
    onSettled: () => setProposalId(null),
  }));

  const quotaExceeded = () =>
    error() instanceof ApiError && (error() as ApiError).code === "ai_quota_exceeded";
  const busy = () => generate.isPending || proposal.data?.status === "pending";
  const toggle = (c: Change, on: boolean) => {
    const next = new Set(chosen());
    if (on) next.add(key(c));
    else next.delete(key(c));
    setChosen(next);
  };
  const localeOptions = () =>
    CONTENT_LOCALES.map((l) => ({ value: l, label: t(`common.locale_${l}`) }));

  return (
    <section
      aria-labelledby={`ai-${props.entityType}-h`}
      class="flex flex-col gap-3 rounded-lg border border-border bg-muted/40 p-3"
    >
      <div class="flex flex-wrap items-center gap-2">
        <h3 id={`ai-${props.entityType}-h`} class="text-sm font-semibold">
          {t("ai.panel")}
        </h3>
        <Show when={usage.data?.provider === "fake"}>
          <Badge tone="info">{t("ai.demo")}</Badge>
        </Show>
      </div>
      <Show when={usage.data?.provider === "fake"}>
        <p class="text-xs text-muted-foreground">{t("ai.demoHint")}</p>
      </Show>

      <Show when={(marks.data?.items.length ?? 0) > 0}>
        <div class="flex flex-col gap-1">
          <p class="text-xs font-medium text-muted-foreground">{t("ai.marks")}</p>
          <ul class="flex flex-wrap gap-1.5" aria-label={t("ai.marks")}>
            <For each={marks.data?.items}>
              {(m) => (
                <li>
                  <Badge tone="info">
                    {t("ai.aiLabel")}: {fieldLabel(m.field, m.locale)}
                  </Badge>
                </li>
              )}
            </For>
          </ul>
        </div>
      </Show>

      <form
        class="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (!busy()) generate.mutate();
        }}
      >
        <div class="grid gap-3 sm:grid-cols-2">
          <SelectField
            label={t("ai.task")}
            value={kind()}
            options={kinds().map((k) => ({ value: k, label: t(`ai.kind_${k}`) }))}
            onChange={(v) => setKind(kinds().find((k) => k === v) ?? "translate")}
          />
          <SelectField
            label={kind() === "translate" ? t("ai.sourceLanguage") : t("ai.language")}
            value={locale()}
            options={localeOptions()}
            onChange={setLocale}
          />
          <Show when={describes()}>
            <SelectField
              label={t("ai.tone")}
              value={tone()}
              options={TONES.map((v) => ({ value: v, label: t(`ai.tone_${v}`) }))}
              onChange={(v) => setTone(TONES.find((x) => x === v) ?? "neutral")}
            />
            <SelectField
              label={t("ai.length")}
              value={length()}
              options={LENGTHS.map((v) => ({ value: v, label: t(`ai.length_${v}`) }))}
              onChange={(v) => setLength(LENGTHS.find((x) => x === v) ?? "medium")}
            />
          </Show>
        </div>
        <Show when={kind() === "translate"}>
          <fieldset class="flex flex-wrap gap-3">
            <legend class="mb-1 text-xs font-medium text-muted-foreground">
              {t("ai.targets")}
            </legend>
            <For each={CONTENT_LOCALES.filter((l) => l !== locale())}>
              {(l) => (
                <Checkbox
                  label={t(`common.locale_${l}`)}
                  checked={targets().includes(l)}
                  onChange={(on) =>
                    setTargets(on ? [...targets(), l] : targets().filter((x) => x !== l))
                  }
                />
              )}
            </For>
          </fieldset>
        </Show>
        <div>
          <Button
            type="submit"
            variant="primary"
            loading={busy()}
            disabled={kind() === "translate" && targets().length === 0}
          >
            {t("ai.generate")}
          </Button>
        </div>
      </form>

      <div aria-live="polite">
        <Show when={busy()}>
          <p role="status" class="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner size="sm" />
            {proposal.data && proposal.data.progress.total > 1
              ? t("ai.translatedOf", {
                  done: proposal.data.progress.done,
                  total: proposal.data.progress.total,
                })
              : t("ai.generating")}
          </p>
        </Show>
      </div>

      <Show when={error()}>
        <div role="alert" class="rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
          <Show when={quotaExceeded()} fallback={errorMessage(error())}>
            {t("ai.quotaExceeded")}{" "}
            <A href="/settings/ai" class="underline">
              {t("ai.seeUsage")}
            </A>
          </Show>
        </div>
      </Show>

      <Show when={proposal.data?.status === "failed"}>
        <p role="alert" class="rounded-md bg-error-50 px-3 py-2 text-sm text-error-700">
          {proposal.data?.error === "ai_quota_exceeded"
            ? t("ai.quotaExceeded")
            : t("ai.failed", {
                reason: errorMessage(new ApiError(422, proposal.data?.error ?? "unknown")),
              })}
        </p>
      </Show>

      <Show when={proposal.data?.status === "ready" ? proposal.data : undefined}>
        {(p) => (
          <div class="flex flex-col gap-3">
            <p class="text-xs text-muted-foreground">{t("ai.proposalHint")}</p>
            <Show when={p().warnings.length > 0}>
              <div class="rounded-md border border-warning-700 bg-warning-50 px-3 py-2 text-sm">
                <p class="font-medium">{t("ai.warnings")}</p>
                <ul class="ml-4 list-disc">
                  <For each={p().warnings}>{(w) => <li>{w}</li>}</For>
                </ul>
              </div>
            </Show>
            <ul class="flex flex-col gap-3" aria-label={t("ai.proposal")}>
              <For each={p().changes}>
                {(c) => (
                  <li class="flex flex-col gap-1.5 border-b border-border pb-3">
                    <Checkbox
                      label={fieldLabel(c.field, c.locale)}
                      checked={chosen().has(key(c))}
                      onChange={(on) => toggle(c, on)}
                    />
                    <div class="grid gap-2 md:grid-cols-2">
                      <div class="flex flex-col gap-0.5">
                        <span class="text-xs text-muted-foreground">{t("ai.before")}</span>
                        <Value field={c.field} value={c.before} muted />
                      </div>
                      <div class="flex flex-col gap-0.5">
                        <span class="text-xs text-muted-foreground">{t("ai.after")}</span>
                        <Value field={c.field} value={c.after} />
                      </div>
                    </div>
                  </li>
                )}
              </For>
            </ul>
            <Show when={props.acceptHint}>
              <p class="text-xs text-muted-foreground">{props.acceptHint}</p>
            </Show>
            <div class="flex flex-wrap gap-2">
              <Button
                variant="primary"
                loading={accept.isPending}
                disabled={chosen().size === 0}
                onClick={() => accept.mutate()}
              >
                {t("ai.accept")}
              </Button>
              <Button loading={discard.isPending} onClick={() => discard.mutate()}>
                {t("ai.discard")}
              </Button>
            </div>
          </div>
        )}
      </Show>
    </section>
  );
}
