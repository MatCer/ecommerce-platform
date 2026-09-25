import {
  Badge,
  Button,
  ConfirmDialog,
  Dialog,
  EmptyState,
  PermissionDenied,
  SelectField,
  showToast,
  Tabs,
  TextField,
  type Tone,
} from "@platform/ui";
import {
  createInfiniteQuery,
  createMutation,
  createQuery,
  useQueryClient,
} from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Message = Schemas["MessageSummary"];
type Suppression = Schemas["Suppression"];

const STATUSES = ["pending", "sending", "accepted", "uncertain", "failed"] as const;
const STREAMS = ["transactional", "marketing"] as const;
const statusTone: Record<string, Tone> = {
  pending: "info",
  sending: "info",
  accepted: "success",
  uncertain: "warning",
  failed: "error",
};
const known = <T extends string>(list: readonly T[], v: string): T | undefined =>
  list.find((x) => x === v);

function statusLabel(s: string): string {
  const k = known(STATUSES, s);
  return k ? t(`emails.status_${k}`) : s;
}

function streamLabel(s: string): string {
  const k = known(STREAMS, s);
  return k ? t(`emails.stream_${k}`) : s;
}

function MessageDetail(props: { id: string }) {
  const detail = createQuery(() => ({
    queryKey: tenantKey("email", props.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/emails/{id}", {
          params: { header: tenantHeader(), path: { id: props.id } },
        }),
      ),
  }));
  return (
    <QueryState query={detail}>
      {(m) => (
        <div class="flex flex-col gap-3">
          <dl class="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
            <dt class="text-muted-foreground">{t("emails.to")}</dt>
            <dd>{m.to_email}</dd>
            <dt class="text-muted-foreground">{t("emails.subject")}</dt>
            <dd>{m.subject}</dd>
            <dt class="text-muted-foreground">{t("emails.template")}</dt>
            <dd>
              {m.template} · {streamLabel(m.stream)} · {m.locale.toUpperCase()}
            </dd>
            <dt class="text-muted-foreground">{t("emails.status")}</dt>
            <dd>
              {statusLabel(m.status)} ({t("emails.attempts", { n: String(m.attempts) })})
            </dd>
            <dt class="text-muted-foreground">{t("emails.created")}</dt>
            <dd class="figures">{formatDateTime(m.created_at)}</dd>
            <Show when={m.accepted_at}>
              {(a) => (
                <>
                  <dt class="text-muted-foreground">{t("emails.accepted")}</dt>
                  <dd class="figures">{formatDateTime(a())}</dd>
                </>
              )}
            </Show>
            <Show when={m.last_error}>
              {(e) => (
                <>
                  <dt class="text-muted-foreground">{t("emails.lastError")}</dt>
                  <dd class="text-error-700">{e()}</dd>
                </>
              )}
            </Show>
            <Show when={m.list_unsubscribe}>
              {(u) => (
                <>
                  <dt class="text-muted-foreground">{t("emails.listUnsubscribe")}</dt>
                  <dd class="break-all text-xs">{u()}</dd>
                </>
              )}
            </Show>
          </dl>
          <Show
            when={m.html}
            fallback={
              <p role="note" class="rounded-md bg-muted p-3 text-sm">
                {m.sensitive ? t("emails.sensitive") : t("emails.noBody")}
              </p>
            }
          >
            {(html) => (
              <iframe
                title={t("emails.body")}
                sandbox=""
                srcdoc={html()}
                class="h-96 w-full rounded-md border border-border bg-white"
              />
            )}
          </Show>
          <Show when={m.text}>
            {(text) => (
              <details>
                <summary class="cursor-pointer text-xs text-accent-700">
                  {t("marketing.plainText")}
                </summary>
                <pre class="mt-1 max-h-64 overflow-auto rounded-sm bg-muted p-2 text-xs whitespace-pre-wrap">
                  {text()}
                </pre>
              </details>
            )}
          </Show>
        </div>
      )}
    </QueryState>
  );
}

function MessageLog() {
  const [status, setStatus] = createSignal("");
  const [stream, setStream] = createSignal("");
  const [to, setTo] = createSignal("");
  const [open, setOpen] = createSignal<Message | null>(null);
  const filters = () => ({
    status: status() || undefined,
    stream: stream() || undefined,
    to: to().trim() || undefined,
  });
  const log = createInfiniteQuery(() => ({
    queryKey: tenantKey("emails", filters()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/emails", {
          params: { header: tenantHeader(), query: { ...filters(), limit: 50, cursor: pageParam } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => log.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <div class="flex flex-col gap-3">
      <div class="grid gap-2 sm:grid-cols-3">
        <TextField type="search" label={t("emails.searchTo")} value={to()} onChange={setTo} />
        <SelectField
          label={t("emails.status")}
          value={status()}
          options={[
            { value: "", label: t("marketing.all") },
            ...STATUSES.map((s) => ({ value: s, label: t(`emails.status_${s}`) })),
          ]}
          onChange={setStatus}
        />
        <SelectField
          label={t("emails.stream")}
          value={stream()}
          options={[
            { value: "", label: t("marketing.all") },
            ...STREAMS.map((s) => ({ value: s, label: t(`emails.stream_${s}`) })),
          ]}
          onChange={setStream}
        />
      </div>
      <QueryState query={log}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={<EmptyState title={t("emails.empty")} description={t("emails.emptyDesc")} />}
          >
            <div class="overflow-x-auto">
              <table class={tableClass} aria-label={t("emails.log")}>
                <thead>
                  <tr>
                    <Th>{t("emails.created")}</Th>
                    <Th>{t("emails.to")}</Th>
                    <Th>{t("emails.subject")}</Th>
                    <Th>{t("emails.template")}</Th>
                    <Th>{t("emails.status")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(m) => (
                      <tr>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {formatDateTime(m.created_at)}
                        </td>
                        <td class={`${tdClass} max-w-56 truncate`} title={m.to_email}>
                          {m.to_email}
                        </td>
                        <td class={`${tdClass} max-w-72 truncate`} title={m.subject}>
                          {m.subject}
                        </td>
                        <td class={`${tdClass} text-xs text-muted-foreground`}>
                          {m.template}
                          <span class="block">{streamLabel(m.stream)}</span>
                        </td>
                        <td class={tdClass}>
                          <Badge tone={statusTone[m.status] ?? "neutral"}>
                            {statusLabel(m.status)}
                          </Badge>
                        </td>
                        <td class={`${tdClass} text-right`}>
                          <Button variant="ghost" onClick={() => setOpen(m)}>
                            {t("emails.details")}
                            <span class="sr-only">: {m.subject}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={log.hasNextPage}>
              <div>
                <Button loading={log.isFetchingNextPage} onClick={() => void log.fetchNextPage()}>
                  {t("common.loadMore")}
                </Button>
              </div>
            </Show>
          </Show>
        )}
      </QueryState>
      <Dialog
        open={open() !== null}
        onOpenChange={(o) => !o && setOpen(null)}
        title={open()?.subject ?? ""}
        size="md"
      >
        <Show when={open()}>{(m) => <MessageDetail id={m().id} />}</Show>
      </Dialog>
    </div>
  );
}

function Suppressions() {
  const qc = useQueryClient();
  const [q, setQ] = createSignal("");
  const [adding, setAdding] = createSignal(false);
  const [email, setEmail] = createSignal("");
  const [note, setNote] = createSignal("");
  const [error, setError] = createSignal<string>();
  const [removing, setRemoving] = createSignal<Suppression | null>(null);
  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("email-suppressions", q().trim()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/email-suppressions", {
          params: {
            header: tenantHeader(),
            query: { q: q().trim() || undefined, after: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_after ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("email-suppressions") });

  const add = createMutation(() => ({
    mutationFn: (body: Schemas["SuppressionInput"]) =>
      unwrap(
        api.POST("/admin/v1/email-suppressions", { params: { header: tenantHeader() }, body }),
      ),
    onSuccess: async () => {
      setAdding(false);
      await refresh();
      showToast({ title: t("emails.suppressed"), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));
  const remove = createMutation(() => ({
    mutationFn: (address: string) =>
      unwrap(
        api.DELETE("/admin/v1/email-suppressions", {
          params: { header: tenantHeader(), query: { email: address } },
        }),
      ),
    onSuccess: async () => {
      setRemoving(null);
      await refresh();
      showToast({ title: t("emails.unsuppressed"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setRemoving(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  return (
    <div class="flex flex-col gap-3">
      <p class="text-sm text-muted-foreground">{t("emails.suppressionsDesc")}</p>
      <div class="flex flex-wrap items-end gap-2">
        <TextField
          type="search"
          label={t("emails.searchSuppressions")}
          value={q()}
          onChange={setQ}
        />
        <Button
          variant="primary"
          onClick={() => {
            setEmail("");
            setNote("");
            setError(undefined);
            setAdding(true);
          }}
        >
          {t("emails.addSuppression")}
        </Button>
      </div>
      <QueryState query={list}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                title={t("emails.noSuppressions")}
                description={t("emails.noSuppressionsDesc")}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass} aria-label={t("emails.suppressions")}>
                <thead>
                  <tr>
                    <Th>{t("emails.address")}</Th>
                    <Th>{t("emails.reason")}</Th>
                    <Th>{t("emails.note")}</Th>
                    <Th>{t("emails.created")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(s) => (
                      <tr>
                        <td class={`${tdClass} font-medium`}>{s.email}</td>
                        <td class={`${tdClass} text-xs`}>
                          {s.reason === "bounce" ||
                          s.reason === "complaint" ||
                          s.reason === "manual"
                            ? t(`emails.reason_${s.reason}`)
                            : s.reason}
                        </td>
                        <td class={`${tdClass} text-xs text-muted-foreground`}>{s.note ?? ""}</td>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {formatDateTime(s.created_at)}
                        </td>
                        <td class={`${tdClass} text-right`}>
                          <Button variant="ghost" onClick={() => setRemoving(s)}>
                            {t("common.remove")}
                            <span class="sr-only">: {s.email}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={list.hasNextPage}>
              <div>
                <Button loading={list.isFetchingNextPage} onClick={() => void list.fetchNextPage()}>
                  {t("common.loadMore")}
                </Button>
              </div>
            </Show>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={adding()}
        onOpenChange={setAdding}
        title={t("emails.addSuppression")}
        description={t("emails.addSuppressionDesc")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            setError(undefined);
            add.mutate({ email: email().trim(), note: note().trim() || null });
          }}
        >
          <TextField
            type="email"
            label={t("emails.address")}
            value={email()}
            onChange={setEmail}
            required
            maxLength={254}
          />
          <TextField label={t("emails.note")} value={note()} onChange={setNote} maxLength={500} />
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setAdding(false)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" loading={add.isPending}>
              {t("common.add")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={removing() !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={t("emails.removeTitle", { email: removing()?.email ?? "" })}
        description={t("emails.removeDesc")}
        confirmLabel={t("common.remove")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const s = removing();
          if (s) remove.mutate(s.email);
        }}
      />
    </div>
  );
}

/** Emails (WP18, owners/admins): what the shop sent, and addresses it never mails. */
export default function Emails() {
  const { can } = useMembership();
  const [tab, setTab] = createSignal("log");
  return (
    <>
      <PageHeader title={t("emails.title")} description={t("emails.description")} />
      <Show
        when={can("admin")}
        fallback={
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        }
      >
        <Tabs
          label={t("emails.title")}
          value={tab()}
          onChange={setTab}
          items={[
            { value: "log", label: t("emails.log"), content: () => <MessageLog /> },
            {
              value: "suppressions",
              label: t("emails.suppressions"),
              content: () => <Suppressions />,
            },
          ]}
        />
      </Show>
    </>
  );
}
