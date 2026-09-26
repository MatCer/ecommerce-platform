import {
  Alert,
  Badge,
  Button,
  Card,
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
    <Card
      class="mb-6 max-w-3xl"
      padding="none"
      titleId="export-title"
      title={t("data.exportTitle")}
      description={t("data.exportHint")}
      actions={
        <Button variant="confirm" loading={start.isPending} onClick={() => start.mutate()}>
          {t("data.exportStart")}
        </Button>
      }
    >
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={<EmptyState icon="download" title={t("data.exportEmpty")} />}
          >
            <div class="overflow-x-auto">
              <table class={tableClass} aria-labelledby="export-title">
                <thead>
                  <tr>
                    <Th>{t("data.created")}</Th>
                    <Th>{t("data.status")}</Th>
                    <Th class="text-right">{t("data.size")}</Th>
                    <Th srOnly>{t("data.download")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(e) => (
                      <tr>
                        <td class={`${tdClass} figures`}>{formatDateTime(e.created_at)}</td>
                        <td class={tdClass}>
                          <Badge tone={tone[e.status]}>{t(`data.export_${e.status}`)}</Badge>
                          <Show when={e.error}>
                            <span class="ml-2 text-sm text-error-700">{e.error}</span>
                          </Show>
                        </td>
                        <td class={`${tdClass} figures text-right`}>{size(e.size_bytes)}</td>
                        <td class={`${tdClass} text-right`}>
                          <Show when={e.status === "ready"}>
                            <Button
                              size="small"
                              icon="download"
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
    </Card>
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
    <Card class="max-w-3xl" title={t("data.privacyTitle")} description={t("data.privacyHint")}>
      <div class="flex flex-col gap-4">
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
          <Alert tone="error">{error()}</Alert>
        </Show>
        <div class="flex flex-col divide-y divide-border rounded-md border border-border">
          <div class="flex flex-wrap items-center justify-between gap-3 p-4">
            <p class="max-w-md text-sm text-muted-foreground">{t("data.accessHint")}</p>
            <Button
              icon="download"
              disabled={!valid()}
              loading={access.isPending}
              onClick={() => access.mutate()}
            >
              {t("data.access")}
            </Button>
          </div>
          <div class="flex flex-wrap items-center justify-between gap-3 p-4">
            <p class="max-w-md text-sm text-muted-foreground">{t("data.eraseHint")}</p>
            <Button variant="danger" disabled={!valid()} onClick={() => setConfirmOpen(true)}>
              {t("data.erase")}
            </Button>
          </div>
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
    </Card>
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
