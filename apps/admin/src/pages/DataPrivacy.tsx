import {
  Badge,
  Button,
  Dialog,
  EmptyState,
  PermissionDenied,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { downloadJson } from "../lib/portability.ts";

type ExportStatus = Schemas["ExportStatus"];
const tone: Record<ExportStatus, Tone> = {
  pending: "neutral",
  running: "info",
  ready: "success",
  failed: "error",
};

function size(bytes: number | null | undefined): string {
  if (bytes == null) return "—";
  return bytes < 1024 * 1024
    ? `${Math.max(1, Math.round(bytes / 1024))} kB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function Exports() {
  const list = createQuery(() => ({
    queryKey: tenantKey("data-exports"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/data-exports", { params: { header: tenantHeader() } })),
    refetchInterval: (q) =>
      q.state.data?.items.some((e) => e.status === "pending" || e.status === "running")
        ? 3000
        : false,
  }));
  const start = createMutation(() => ({
    mutationFn: () =>
      unwrap(api.POST("/admin/v1/data-exports", { params: { header: tenantHeader() } })),
    onSuccess: () => void list.refetch(),
    onError: (e: unknown) =>
      showToast({ title: contentError(e), tone: "error", closeLabel: t("common.close") }),
  }));
  const download = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.POST("/admin/v1/data-exports/{id}/download", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: (link) => window.location.assign(link.url),
    onError: (e: unknown) =>
      showToast({ title: contentError(e), tone: "error", closeLabel: t("common.close") }),
  }));
  return (
    <section aria-labelledby="export-title" class="mb-10 flex max-w-3xl flex-col gap-3">
      <h2 id="export-title" class="font-semibold">
        {t("data.exportTitle")}
      </h2>
      <p class="text-sm text-muted-foreground">{t("data.exportHint")}</p>
      <div>
        <Button variant="confirm" loading={start.isPending} onClick={() => start.mutate()}>
          {t("data.exportStart")}
        </Button>
      </div>
      <QueryState query={list}>
        {(data) => (
          <Show when={data.items.length} fallback={<EmptyState title={t("data.exportEmpty")} />}>
            <div class="overflow-x-auto">
              <table class={tableClass} aria-labelledby="export-title">
                <thead>
                  <tr>
                    <Th>{t("data.created")}</Th>
                    <Th>{t("data.status")}</Th>
                    <Th>{t("data.size")}</Th>
                    <Th srOnly>{t("data.download")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(e) => (
                      <tr>
                        <td class={tdClass}>{formatDateTime(e.created_at)}</td>
                        <td class={tdClass}>
                          <Badge tone={tone[e.status]}>{t(`data.export_${e.status}`)}</Badge>
                          <Show when={e.error}>
                            <span class="ml-2 text-sm text-error-700">{e.error}</span>
                          </Show>
                        </td>
                        <td class={`${tdClass} figures`}>{size(e.size_bytes)}</td>
                        <td class={tdClass}>
                          <Show when={e.status === "ready"}>
                            <Button
                              loading={download.isPending && download.variables === e.id}
                              onClick={() => download.mutate(e.id)}
                            >
                              {t("data.download")}
                            </Button>
                          </Show>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
        )}
      </QueryState>
    </section>
  );
}

function PrivacyRequests() {
  const qc = useQueryClient();
  const [email, setEmail] = createSignal(""),
    [confirmOpen, setConfirmOpen] = createSignal(false),
    [confirmEmail, setConfirmEmail] = createSignal(""),
    [error, setError] = createSignal<string>();
  const access = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/privacy/access", {
          params: { header: tenantHeader() },
          body: { email: email().trim() },
        }),
      ),
    onSuccess: (doc: unknown) => {
      setError(undefined);
      downloadJson(doc, "personal-data.json");
      showToast({ title: t("data.accessDone"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const erase = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/privacy/erasure", {
          params: { header: tenantHeader() },
          body: { email: email().trim(), confirm_email: confirmEmail().trim() },
        }),
      ),
    onSuccess: (r) => {
      // Orders, subscribers, the archive and exports all changed.
      void qc.invalidateQueries();
      setConfirmOpen(false);
      setConfirmEmail("");
      setEmail("");
      setError(undefined);
      showToast({
        title: t("data.erased", {
          orders: String(r.orders_anonymized + r.archived_orders_anonymized),
          invoices: String(r.invoices_retained),
        }),
        closeLabel: t("common.close"),
      });
    },
    onError: (e: unknown) => {
      setConfirmOpen(false);
      setError(contentError(e));
    },
  }));
  const valid = () => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email().trim());
  return (
    <section aria-labelledby="privacy-title" class="flex max-w-3xl flex-col gap-3">
      <h2 id="privacy-title" class="font-semibold">
        {t("data.privacyTitle")}
      </h2>
      <p class="text-sm text-muted-foreground">{t("data.privacyHint")}</p>
      <div class="max-w-sm">
        <TextField
          type="email"
          label={t("data.subjectEmail")}
          value={email()}
          onChange={setEmail}
          autocomplete="off"
        />
      </div>
      <Show when={error()}>
        <p role="alert" class="text-error-700">
          {error()}
        </p>
      </Show>
      <div class="grid gap-4 sm:grid-cols-2">
        <div class="flex flex-col items-start gap-2 rounded-md border border-border p-4">
          <p class="text-sm">{t("data.accessHint")}</p>
          <Button disabled={!valid()} loading={access.isPending} onClick={() => access.mutate()}>
            {t("data.access")}
          </Button>
        </div>
        <div class="flex flex-col items-start gap-2 rounded-md border border-border p-4">
          <p class="text-sm">{t("data.eraseHint")}</p>
          <Button variant="danger" disabled={!valid()} onClick={() => setConfirmOpen(true)}>
            {t("data.erase")}
          </Button>
        </div>
      </div>
      <Dialog
        open={confirmOpen()}
        onOpenChange={setConfirmOpen}
        title={t("data.eraseTitle")}
        description={t("data.eraseConfirm")}
        size="sm"
        footer={
          <>
            <Button onClick={() => setConfirmOpen(false)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              loading={erase.isPending}
              disabled={confirmEmail().trim().toLowerCase() !== email().trim().toLowerCase()}
              onClick={() => erase.mutate()}
            >
              {t("data.erase")}
            </Button>
          </>
        }
      >
        <TextField
          type="email"
          label={t("data.confirmEmail")}
          value={confirmEmail()}
          onChange={setConfirmEmail}
          autocomplete="off"
        />
      </Dialog>
    </section>
  );
}

/** Full data export and GDPR access/erasure requests (WP13b, A29). Owners and admins. */
export default function DataPrivacy() {
  const { me, can } = useMembership();
  return (
    <QueryState query={me}>
      {() => (
        <Show
          when={can("admin")}
          fallback={
            <PermissionDenied
              title={t("common.forbiddenTitle")}
              description={t("common.forbiddenDesc")}
            />
          }
        >
          <PageHeader title={t("data.privacy")} />
          <Exports />
          <PrivacyRequests />
        </Show>
      )}
    </QueryState>
  );
}
