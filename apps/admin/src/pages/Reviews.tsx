import {
  Badge,
  Button,
  EmptyState,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { A } from "@solidjs/router";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

type Review = Schemas["Review"];
type Status = Schemas["ReviewStatus"];
const STATUSES: Status[] = ["pending", "published", "rejected", "hidden"];
const tone: Record<Status, Tone> = {
  pending: "info",
  published: "success",
  rejected: "error",
  hidden: "neutral",
};
/** Moderation actions a review offers in each state (the API enforces the same table). */
const actions: Record<Status, Status[]> = {
  pending: ["published", "rejected"],
  published: ["hidden"],
  rejected: ["published"],
  hidden: ["published"],
};
const actionLabel: Record<Status, "reviews.publish" | "reviews.reject" | "reviews.hide"> = {
  pending: "reviews.publish",
  published: "reviews.publish",
  rejected: "reviews.reject",
  hidden: "reviews.hide",
};
const doneLabel: Record<Status, "reviews.published" | "reviews.rejected" | "reviews.hidden"> = {
  pending: "reviews.published",
  published: "reviews.published",
  rejected: "reviews.rejected",
  hidden: "reviews.hidden",
};

/**
 * Review moderation (WP16): the queue of buyer reviews (all verified purchases), publish /
 * reject / hide and a public reply. Only published reviews reach the shop.
 */
export default function Reviews() {
  const qc = useQueryClient();
  const [status, setStatus] = createSignal<Status | "">("pending");

  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("reviews", status()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/reviews", {
          params: {
            header: tenantHeader(),
            query: { status: status() || undefined, limit: 50, cursor: pageParam },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];
  const toastError = (err: unknown) =>
    showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

  const moderate = createMutation(() => ({
    mutationFn: (v: { id: string; status: Status }) =>
      unwrap(
        api.PUT("/admin/v1/reviews/{id}/status", {
          params: { header: tenantHeader(), path: { id: v.id } },
          body: { status: v.status },
        }),
      ),
    onSuccess: async (r) => {
      await qc.invalidateQueries({ queryKey: tenantKey("reviews") });
      showToast({ title: t(doneLabel[r.status]), closeLabel: t("common.close") });
    },
    onError: toastError,
  }));

  return (
    <>
      <PageHeader title={t("reviews.title")} description={t("reviews.description")} />
      <div class="mb-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <SelectField
          label={t("reviews.status")}
          value={status()}
          options={[
            { value: "", label: t("reviews.all") },
            ...STATUSES.map((s) => ({ value: s, label: t(`reviews.status_${s}`) })),
          ]}
          onChange={(v) => setStatus(STATUSES.find((s) => s === v) ?? "")}
        />
      </div>
      <p class="mb-4 max-w-prose text-xs text-muted-foreground">{t("reviews.policy")}</p>
      <QueryState query={list}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState title={t("reviews.empty")} description={t("reviews.emptyDesc")} />
            }
          >
            <ul class="grid gap-3">
              <For each={rows()}>
                {(r) => (
                  <ReviewCard
                    review={r}
                    busy={moderate.isPending && moderate.variables?.id === r.id}
                    onModerate={(s) => moderate.mutate({ id: r.id, status: s })}
                    onError={toastError}
                  />
                )}
              </For>
            </ul>
            <Show when={list.hasNextPage}>
              <div class="mt-3">
                <Button loading={list.isFetchingNextPage} onClick={() => void list.fetchNextPage()}>
                  {t("common.loadMore")}
                </Button>
              </div>
            </Show>
          </Show>
        )}
      </QueryState>
    </>
  );
}

function ReviewCard(props: {
  review: Review;
  busy: boolean;
  onModerate: (s: Status) => void;
  onError: (err: unknown) => void;
}) {
  const qc = useQueryClient();
  const r = () => props.review;
  const [reply, setReply] = createSignal(props.review.reply ?? "");
  const saveReply = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/reviews/{id}/reply", {
          params: { header: tenantHeader(), path: { id: r().id } },
          body: { reply: reply().trim() || null },
        }),
      ),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: tenantKey("reviews") });
      showToast({ title: t("reviews.replySaved"), closeLabel: t("common.close") });
    },
    onError: props.onError,
  }));

  return (
    <li class="rounded-lg border border-border bg-card p-4">
      <article aria-labelledby={`review-${r().id}`} class="grid gap-3">
        <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span
            role="img"
            aria-label={t("reviews.rating", { n: String(r().rating) })}
            class="text-accent-600"
          >
            {"★".repeat(r().rating)}
            <span class="text-border">{"★".repeat(5 - r().rating)}</span>
          </span>
          <h2 id={`review-${r().id}`} class="font-semibold">
            {r().title || r().customer_name}
          </h2>
          <Badge tone={tone[r().status]}>{t(`reviews.status_${r().status}`)}</Badge>
        </div>
        <p class="flex flex-wrap gap-x-3 text-xs text-muted-foreground">
          <A href={`/products/${r().product_id}`} class="font-medium text-foreground underline">
            {r().product_name}
          </A>
          <span>{r().customer_name}</span>
          <span>{t("reviews.written", { date: formatDateTime(r().created_at) })}</span>
          <Show when={r().verified}>
            <span class="font-medium text-success-700">{t("reviews.verified")}</span>
          </Show>
          <Show when={r().order_id && r().order_number}>
            <A href={`/orders/${r().order_id}`} class="underline">
              {t("reviews.order", { n: String(r().order_number) })}
            </A>
          </Show>
        </p>
        <p class="max-w-prose text-sm whitespace-pre-line" lang={r().locale}>
          {r().body}
        </p>
        <form
          class="grid max-w-prose gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            saveReply.mutate();
          }}
        >
          <TextField
            label={t("reviews.reply")}
            description={t("reviews.replyHint")}
            value={reply()}
            onChange={setReply}
            multiline
            rows={2}
            maxLength={2000}
          />
          <div>
            <Button type="submit" loading={saveReply.isPending}>
              {t("reviews.saveReply")}
            </Button>
          </div>
        </form>
        <div class="flex flex-wrap gap-2">
          <For each={actions[r().status]}>
            {(to) => (
              <Button
                variant={to === "published" ? "confirm" : "default"}
                loading={props.busy}
                onClick={() => props.onModerate(to)}
              >
                {t(actionLabel[to])}
                <span class="sr-only">: {r().title || r().customer_name}</span>
              </Button>
            )}
          </For>
        </div>
      </article>
    </li>
  );
}
