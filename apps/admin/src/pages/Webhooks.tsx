import {
  Badge,
  Button,
  Checkbox,
  ConfirmDialog,
  Dialog,
  EmptyState,
  ErrorState,
  FieldGroup,
  PermissionDenied,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { useSearchParams } from "@solidjs/router";
import {
  createInfiniteQuery,
  createMutation,
  createQuery,
  useQueryClient,
} from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { groupEvents } from "../lib/analytics.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Subscription = Schemas["Subscription"];
type Delivery = Schemas["Delivery"];
type Form = { url: string; description: string; events: string[]; active: boolean };

const DELIVERY_STATUSES = ["pending", "retrying", "succeeded", "dead"] as const;
const STATUS_TONE: Record<(typeof DELIVERY_STATUSES)[number], Tone> = {
  pending: "neutral",
  retrying: "warning",
  succeeded: "success",
  dead: "error",
};
const GROUPS = ["order", "product", "inventory", "customer"] as const;

const str = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v) ?? "";
const unavailable = (err: unknown) => err instanceof ApiError && err.status === 503;

function groupLabel(group: string): string {
  const known = GROUPS.find((g) => g === group);
  return known ? t(`webhooks.groups.${known}`) : group;
}

function StatusBadge(props: { status: string }) {
  const known = DELIVERY_STATUSES.find((s) => s === props.status);
  return (
    <Badge tone={known ? STATUS_TONE[known] : "neutral"}>
      {known ? t(`webhooks.statuses.${known}`) : props.status}
    </Badge>
  );
}

export default function Webhooks() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const [params, setParams] = useSearchParams();
  const filter = () => str(params.subscription) || undefined;

  const [editing, setEditing] = createSignal<Subscription | "new" | null>(null);
  const [form, setForm] = createSignal<Form>({
    url: "",
    description: "",
    events: [],
    active: true,
  });
  const [error, setError] = createSignal<string>();
  const [removing, setRemoving] = createSignal<Subscription | null>(null);
  const [rotating, setRotating] = createSignal<Subscription | null>(null);
  const [secret, setSecret] = createSignal<string | null>(null);
  const [payload, setPayload] = createSignal<Delivery | null>(null);

  const list = createQuery(() => ({
    queryKey: tenantKey("webhooks"),
    queryFn: () => unwrap(api.GET("/admin/v1/webhooks", { params: { header: tenantHeader() } })),
    enabled: can("admin"),
  }));

  const deliveries = createInfiniteQuery(() => ({
    queryKey: tenantKey("webhook-deliveries", filter() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/webhooks/deliveries", {
          params: {
            header: tenantHeader(),
            query: { subscription_id: filter(), cursor: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: can("admin") && list.isSuccess,
  }));
  const rows = () => deliveries.data?.pages.flatMap((p) => p.items) ?? [];
  const urlOf = (id: string) => list.data?.items.find((s) => s.id === id)?.url ?? id;

  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("webhooks") });
  const refreshDeliveries = () =>
    qc.invalidateQueries({ queryKey: tenantKey("webhook-deliveries") });
  const toastError = (err: unknown) =>
    showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

  const openForm = (s: Subscription | "new") => {
    setForm(
      s === "new"
        ? { url: "", description: "", events: [], active: true }
        : { url: s.url, description: s.description, events: [...s.events], active: s.active },
    );
    setError(undefined);
    setEditing(s);
  };
  const toggleEvent = (e: string, on: boolean) =>
    setForm((f) => ({
      ...f,
      events: on ? [...f.events, e] : f.events.filter((x) => x !== e),
    }));

  const save = createMutation(() => ({
    mutationFn: async (): Promise<string | null> => {
      const f = form();
      const body = {
        url: f.url.trim(),
        description: f.description.trim(),
        events: f.events,
        active: f.active,
      };
      const target = editing();
      if (target === "new") {
        const created = await unwrap(
          api.POST("/admin/v1/webhooks", { params: { header: tenantHeader() }, body }),
        );
        return created.secret;
      }
      if (target) {
        await unwrap(
          api.PATCH("/admin/v1/webhooks/{id}", {
            params: { header: tenantHeader(), path: { id: target.id } },
            body,
          }),
        );
      }
      return null;
    },
    onSuccess: async (newSecret) => {
      const created = editing() === "new";
      setEditing(null);
      await refresh();
      if (newSecret) setSecret(newSecret);
      showToast({
        title: created ? t("webhooks.created") : t("webhooks.saved"),
        closeLabel: t("common.close"),
      });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/webhooks/{id}", { params: { header: tenantHeader(), path: { id } } }),
      ),
    onSuccess: async (_, id) => {
      setRemoving(null);
      if (filter() === id) setParams({ subscription: undefined });
      await Promise.all([refresh(), refreshDeliveries()]);
      showToast({ title: t("webhooks.deleted"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setRemoving(null);
      toastError(err);
    },
  }));

  const rotate = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/webhooks/{id}/rotate-secret", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async (res) => {
      setRotating(null);
      setSecret(res.secret);
      await refresh();
    },
    onError: (err) => {
      setRotating(null);
      toastError(err);
    },
  }));

  const redeliver = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/webhooks/deliveries/{id}/redeliver", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      await refreshDeliveries();
      showToast({ title: t("webhooks.redelivered"), closeLabel: t("common.close") });
    },
    onError: toastError,
  }));

  const copySecret = async () => {
    try {
      await navigator.clipboard.writeText(secret() ?? "");
      showToast({ title: t("webhooks.copied"), closeLabel: t("common.close") });
    } catch {
      showToast({ title: t("webhooks.copyFailed"), tone: "error", closeLabel: t("common.close") });
    }
  };

  return (
    <Show
      when={can("admin")}
      fallback={
        <>
          <PageHeader title={t("webhooks.title")} />
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        </>
      }
    >
      <PageHeader
        title={t("webhooks.title")}
        description={t("webhooks.lead")}
        actions={
          <Show when={list.isSuccess}>
            <Button variant="confirm" onClick={() => openForm("new")}>
              {t("webhooks.new")}
            </Button>
          </Show>
        }
      />
      <Show
        when={!unavailable(list.error)}
        fallback={
          <ErrorState
            title={t("webhooks.unavailableTitle")}
            description={t("webhooks.unavailableDesc")}
          />
        }
      >
        <QueryState query={list}>
          {(data) => (
            <div class="flex flex-col gap-8">
              <Show
                when={data.items.length > 0}
                fallback={
                  <EmptyState
                    title={t("webhooks.emptyTitle")}
                    description={t("webhooks.emptyDesc")}
                    action={
                      <Button variant="confirm" onClick={() => openForm("new")}>
                        {t("webhooks.new")}
                      </Button>
                    }
                  />
                }
              >
                <div class="overflow-x-auto">
                  <table class={tableClass}>
                    <thead>
                      <tr>
                        <Th>{t("webhooks.url")}</Th>
                        <Th>{t("webhooks.events")}</Th>
                        <Th>{t("webhooks.status")}</Th>
                        <Th>{t("webhooks.secret")}</Th>
                        <Th srOnly>{t("common.actions")}</Th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={data.items}>
                        {(s) => (
                          <tr class="align-top">
                            <td class={`${tdClass} py-2`}>
                              <span class="figures block max-w-80 text-xs break-all">{s.url}</span>
                              <Show when={s.description}>
                                <span class="block text-xs text-muted-foreground">
                                  {s.description}
                                </span>
                              </Show>
                            </td>
                            <td class={`${tdClass} py-2`}>
                              <ul class="figures flex max-w-72 flex-wrap gap-x-2 text-xs">
                                <For each={s.events}>{(e) => <li>{e}</li>}</For>
                              </ul>
                            </td>
                            <td class={`${tdClass} py-2`}>
                              <Badge tone={s.active ? "success" : "neutral"}>
                                {s.active ? t("webhooks.active") : t("webhooks.inactive")}
                              </Badge>
                            </td>
                            <td
                              class={`${tdClass} py-2 text-xs whitespace-nowrap text-muted-foreground`}
                            >
                              {t("webhooks.secretEnding", { hint: s.secret_hint })}
                            </td>
                            <td class={`${tdClass} py-1 text-right whitespace-nowrap`}>
                              <Button
                                category="tertiary"
                                onClick={() => setParams({ subscription: s.id })}
                              >
                                {t("webhooks.showDeliveries")}
                                <span class="sr-only">: {s.url}</span>
                              </Button>
                              <Button category="tertiary" onClick={() => openForm(s)}>
                                {t("common.edit")}
                                <span class="sr-only">: {s.url}</span>
                              </Button>
                              <Button category="tertiary" onClick={() => setRotating(s)}>
                                {t("webhooks.rotate")}
                                <span class="sr-only">: {s.url}</span>
                              </Button>
                              <Button category="tertiary" onClick={() => setRemoving(s)}>
                                {t("common.delete")}
                                <span class="sr-only">: {s.url}</span>
                              </Button>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>

              <section aria-labelledby="deliveries" class="flex flex-col gap-3">
                <div class="flex flex-col gap-0.5">
                  <h2 id="deliveries" class="text-sm font-semibold">
                    {t("webhooks.deliveries")}
                  </h2>
                  <p class="text-xs text-muted-foreground">{t("webhooks.deliveriesDesc")}</p>
                </div>
                <div class="flex flex-wrap items-end gap-2">
                  <SelectField
                    class="w-72 max-w-full"
                    label={t("webhooks.subscription")}
                    value={filter() ?? ""}
                    options={[
                      { value: "", label: t("webhooks.allSubscriptions") },
                      ...data.items.map((s) => ({ value: s.id, label: s.url })),
                    ]}
                    onChange={(v) => setParams({ subscription: v || undefined })}
                  />
                  <Button
                    loading={deliveries.isRefetching && !deliveries.isFetchingNextPage}
                    onClick={() => void deliveries.refetch()}
                  >
                    {t("webhooks.refresh")}
                  </Button>
                </div>
                <QueryState query={deliveries}>
                  {() => (
                    <Show
                      when={rows().length > 0}
                      fallback={
                        <EmptyState
                          title={t("webhooks.noDeliveries")}
                          description={t("webhooks.noDeliveriesDesc")}
                        />
                      }
                    >
                      <div class="overflow-x-auto">
                        <table class={tableClass} aria-labelledby="deliveries">
                          <thead>
                            <tr>
                              <Th>{t("webhooks.time")}</Th>
                              <Th>{t("webhooks.event")}</Th>
                              <Show when={!filter()}>
                                <Th>{t("webhooks.subscription")}</Th>
                              </Show>
                              <Th>{t("webhooks.status")}</Th>
                              <Th class="text-right">{t("webhooks.attempts")}</Th>
                              <Th class="text-right">{t("webhooks.response")}</Th>
                              <Th>{t("webhooks.lastError")}</Th>
                              <Th>{t("webhooks.nextRetry")}</Th>
                              <Th srOnly>{t("common.actions")}</Th>
                            </tr>
                          </thead>
                          <tbody>
                            <For each={rows()}>
                              {(d) => (
                                <tr>
                                  <td
                                    class={`${tdClass} figures text-xs whitespace-nowrap text-faint-foreground`}
                                  >
                                    {formatDateTime(d.created_at)}
                                  </td>
                                  <td class={`${tdClass} figures text-xs`}>{d.event_type}</td>
                                  <Show when={!filter()}>
                                    <td
                                      class={`${tdClass} figures max-w-48 truncate text-xs`}
                                      title={urlOf(d.subscription_id)}
                                    >
                                      {urlOf(d.subscription_id)}
                                    </td>
                                  </Show>
                                  <td class={tdClass}>
                                    <StatusBadge status={d.status} />
                                  </td>
                                  <td class={`${tdClass} figures text-right`}>{d.attempts}</td>
                                  <td class={`${tdClass} figures text-right`}>
                                    {d.response_code ?? "—"}
                                  </td>
                                  <td
                                    class={`${tdClass} max-w-56 truncate text-xs`}
                                    title={d.last_error ?? ""}
                                  >
                                    {d.last_error ?? "—"}
                                  </td>
                                  <td class={`${tdClass} text-xs whitespace-nowrap`}>
                                    {d.next_at &&
                                    (d.status === "retrying" || d.status === "pending")
                                      ? formatDateTime(d.next_at)
                                      : "—"}
                                  </td>
                                  <td class={`${tdClass} text-right whitespace-nowrap`}>
                                    <Button category="tertiary" onClick={() => setPayload(d)}>
                                      {t("webhooks.payload")}
                                      <span class="sr-only">
                                        : {d.event_type}, {formatDateTime(d.created_at)}
                                      </span>
                                    </Button>
                                    <Show when={d.status === "succeeded" || d.status === "dead"}>
                                      <Button
                                        category="tertiary"
                                        loading={
                                          redeliver.isPending && redeliver.variables === d.id
                                        }
                                        onClick={() => redeliver.mutate(d.id)}
                                      >
                                        {t("webhooks.redeliver")}
                                        <span class="sr-only">
                                          : {d.event_type}, {formatDateTime(d.created_at)}
                                        </span>
                                      </Button>
                                    </Show>
                                  </td>
                                </tr>
                              )}
                            </For>
                          </tbody>
                        </table>
                      </div>
                      <Show when={deliveries.hasNextPage}>
                        <div>
                          <Button
                            loading={deliveries.isFetchingNextPage}
                            onClick={() => void deliveries.fetchNextPage()}
                          >
                            {t("common.loadMore")}
                          </Button>
                        </div>
                      </Show>
                    </Show>
                  )}
                </QueryState>
              </section>

              <Dialog
                open={editing() !== null}
                onOpenChange={(o) => !o && setEditing(null)}
                title={editing() === "new" ? t("webhooks.new") : t("webhooks.edit")}
              >
                <form
                  class="flex flex-col gap-3"
                  onSubmit={(e) => {
                    e.preventDefault();
                    save.mutate();
                  }}
                >
                  <TextField
                    label={t("webhooks.url")}
                    type="url"
                    value={form().url}
                    onChange={(url) => setForm((f) => ({ ...f, url }))}
                    description={t("webhooks.urlHint")}
                    placeholder="https://"
                    required
                    maxLength={2048}
                  />
                  <TextField
                    label={t("webhooks.description")}
                    value={form().description}
                    onChange={(description) => setForm((f) => ({ ...f, description }))}
                    maxLength={200}
                  />
                  <FieldGroup legend={t("webhooks.events")} description={t("webhooks.eventsHint")}>
                    <div class="grid gap-3 sm:grid-cols-2">
                      <For each={groupEvents(data.event_types)}>
                        {(g) => (
                          <fieldset class="flex flex-col gap-1.5">
                            <legend class="mb-1 text-xs font-medium text-muted-foreground">
                              {groupLabel(g.group)}
                            </legend>
                            <For each={g.events}>
                              {(e) => (
                                <Checkbox
                                  label={<span class="figures text-xs">{e}</span>}
                                  checked={form().events.includes(e)}
                                  onChange={(on) => toggleEvent(e, on)}
                                />
                              )}
                            </For>
                          </fieldset>
                        )}
                      </For>
                    </div>
                  </FieldGroup>
                  <Checkbox
                    label={t("webhooks.active")}
                    description={t("webhooks.activeHint")}
                    checked={form().active}
                    onChange={(active) => setForm((f) => ({ ...f, active }))}
                  />
                  <Show when={error()}>
                    <p role="alert" class="text-xs font-medium text-error-700">
                      {error()}
                    </p>
                  </Show>
                  <div class="flex justify-end gap-2">
                    <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
                    <Button
                      type="submit"
                      variant="confirm"
                      loading={save.isPending}
                      disabled={form().url.trim() === "" || form().events.length === 0}
                    >
                      {editing() === "new" ? t("common.create") : t("common.save")}
                    </Button>
                  </div>
                </form>
              </Dialog>
            </div>
          )}
        </QueryState>
      </Show>

      <Dialog
        open={secret() !== null}
        onOpenChange={(o) => !o && setSecret(null)}
        title={t("webhooks.secretTitle")}
        description={t("webhooks.secretOnce")}
      >
        <div class="flex flex-col gap-3">
          <div class="flex items-end gap-2">
            <TextField
              class="min-w-0 flex-1"
              inputClass="figures"
              label={t("webhooks.secret")}
              value={secret() ?? ""}
              onChange={() => undefined}
              readOnly
            />
            <Button onClick={() => void copySecret()}>{t("webhooks.copy")}</Button>
          </div>
          <div class="flex flex-col gap-1 rounded-md bg-muted p-3 text-xs">
            <p class="font-semibold">{t("webhooks.verifyTitle")}</p>
            <p class="text-muted-foreground">{t("webhooks.verifyText")}</p>
          </div>
          <div class="flex justify-end">
            <Button variant="confirm" onClick={() => setSecret(null)}>
              {t("webhooks.secretStored")}
            </Button>
          </div>
        </div>
      </Dialog>

      <Dialog
        open={payload() !== null}
        onOpenChange={(o) => !o && setPayload(null)}
        title={t("webhooks.payloadTitle", { event: payload()?.event_type ?? "" })}
      >
        <pre class="figures max-h-96 overflow-auto rounded-sm bg-muted p-2 text-xs">
          {JSON.stringify(payload()?.payload, null, 2)}
        </pre>
        <div class="flex justify-end">
          <Button onClick={() => setPayload(null)}>{t("common.close")}</Button>
        </div>
      </Dialog>

      <ConfirmDialog
        open={rotating() !== null}
        onOpenChange={(o) => !o && setRotating(null)}
        title={t("webhooks.rotateTitle")}
        description={t("webhooks.rotateDesc")}
        confirmLabel={t("webhooks.rotate")}
        cancelLabel={t("common.cancel")}
        pending={rotate.isPending}
        onConfirm={() => {
          const s = rotating();
          if (s) rotate.mutate(s.id);
        }}
      />

      <ConfirmDialog
        open={removing() !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={t("webhooks.deleteTitle", { url: removing()?.url ?? "" })}
        description={t("webhooks.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const s = removing();
          if (s) remove.mutate(s.id);
        }}
      />
    </Show>
  );
}
