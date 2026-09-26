import {
  Alert,
  Avatar,
  Badge,
  Button,
  Card,
  ConfirmDialog,
  Dialog,
  PermissionDenied,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Role, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { claims } from "../lib/session.ts";

type Member = Schemas["StaffMember"];
const ROLES: Role[] = ["staff", "admin", "owner"];

export default function Staff() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const [inviting, setInviting] = createSignal(false);
  const [email, setEmail] = createSignal("");
  const [role, setRole] = createSignal<Role>("staff");
  const [changing, setChanging] = createSignal<Member | null>(null);
  const [removing, setRemoving] = createSignal<Member | null>(null);
  const [error, setError] = createSignal<string>();

  const staff = createQuery(() => ({
    queryKey: tenantKey("staff"),
    queryFn: () => unwrap(api.GET("/admin/v1/staff", { params: { header: tenantHeader() } })),
    enabled: can("admin"),
  }));
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("staff") });
  const roleOptions = () =>
    ROLES.filter((r) => r !== "owner" || can("owner")).map((r) => ({
      value: r,
      label: t(`roles.${r}`),
    }));
  /** Admins cannot touch owners (the API enforces the same rule). */
  const editable = (m: Member) => can("owner") || m.role !== "owner";

  const invite = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/staff/invitations", {
          params: { header: tenantHeader() },
          body: { email: email().trim(), role: role() },
        }),
      ),
    onSuccess: async (m) => {
      setInviting(false);
      await refresh();
      showToast({ title: t("staff.invited", { email: m.email }), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const change = createMutation(() => ({
    mutationFn: (v: { id: string; role: Role }) =>
      unwrap(
        api.PATCH("/admin/v1/staff/{id}", {
          params: { header: tenantHeader(), path: { id: v.id } },
          body: { role: v.role },
        }),
      ),
    onSuccess: async () => {
      setChanging(null);
      await refresh();
      showToast({ title: t("staff.roleChanged"), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/staff/{id}", { params: { header: tenantHeader(), path: { id } } }),
      ),
    onSuccess: async () => {
      setRemoving(null);
      await refresh();
      showToast({ title: t("staff.removed"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setRemoving(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  return (
    <Show
      when={can("admin")}
      fallback={
        <>
          <PageHeader title={t("staff.title")} />
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        </>
      }
    >
      <PageHeader
        title={t("staff.title")}
        actions={
          <Button
            variant="confirm"
            onClick={() => {
              setEmail("");
              setRole("staff");
              setError(undefined);
              setInviting(true);
            }}
          >
            {t("staff.invite")}
          </Button>
        }
      />
      <QueryState query={staff}>
        {(data) => (
          <div class="flex flex-col gap-3">
            <Card
              title={t("staff.title")}
              count={data.items.length}
              countIcon="user"
              padding="none"
            >
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("staff.email")}</Th>
                      <Th>{t("staff.role")}</Th>
                      <Th class="text-right">{t("staff.added")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(m) => (
                        <tr>
                          <td class={tdClass}>
                            <span class="flex items-center gap-2">
                              <Avatar name={m.email} size={24} />
                              <span class="font-semibold text-heading">{m.email}</span>
                              <Show when={m.user_id === claims()?.sub}>
                                <span class="text-sm text-muted-foreground">
                                  ({t("staff.you")})
                                </span>
                              </Show>
                            </span>
                          </td>
                          <td class={tdClass}>
                            <Badge tone={m.role === "owner" ? "info" : "neutral"}>
                              {t(`roles.${m.role}`)}
                            </Badge>
                          </td>
                          <td class={`${tdClass} figures text-right text-muted-foreground`}>
                            {formatDateTime(m.created_at)}
                          </td>
                          <td class={`${tdClass} text-right whitespace-nowrap`}>
                            <Button
                              category="tertiary"
                              size="small"
                              disabled={!editable(m)}
                              title={editable(m) ? undefined : t("staff.ownerOnly")}
                              onClick={() => {
                                setError(undefined);
                                setRole(m.role);
                                setChanging(m);
                              }}
                            >
                              {t("staff.changeRoleAction")}
                              <span class="sr-only">: {m.email}</span>
                            </Button>
                            <Button
                              variant="danger"
                              category="tertiary"
                              size="small"
                              class="ml-1"
                              disabled={!editable(m)}
                              title={editable(m) ? undefined : t("staff.ownerOnly")}
                              onClick={() => setRemoving(m)}
                            >
                              {t("common.remove")}
                              <span class="sr-only">: {m.email}</span>
                            </Button>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Card>
            <Show when={!can("owner")}>
              <p class="text-sm text-muted-foreground">{t("staff.ownerOnly")}</p>
            </Show>
          </div>
        )}
      </QueryState>

      <Dialog
        open={inviting()}
        onOpenChange={setInviting}
        title={t("staff.inviteTitle")}
        description={t("staff.inviteDesc")}
      >
        <form
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            invite.mutate();
          }}
        >
          <TextField
            label={t("staff.email")}
            type="email"
            value={email()}
            onChange={setEmail}
            required
            maxLength={254}
          />
          <SelectField
            label={t("staff.role")}
            value={role()}
            options={roleOptions()}
            description={t(`roles.${role()}Desc`)}
            onChange={(v) => setRole(v as Role)}
          />
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setInviting(false)}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              variant="confirm"
              loading={invite.isPending}
              disabled={!email().includes("@")}
            >
              {t("staff.invite")}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={changing() !== null}
        onOpenChange={(o) => !o && setChanging(null)}
        title={t("staff.changeRole", { email: changing()?.email ?? "" })}
        size="sm"
      >
        <form
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            const m = changing();
            if (m) change.mutate({ id: m.id, role: role() });
          }}
        >
          <SelectField
            label={t("staff.role")}
            value={role()}
            options={roleOptions()}
            description={t(`roles.${role()}Desc`)}
            onChange={(v) => setRole(v as Role)}
          />
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setChanging(null)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="confirm" loading={change.isPending}>
              {t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={removing() !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={t("staff.removeTitle", { email: removing()?.email ?? "" })}
        description={t("staff.removeDesc")}
        confirmLabel={t("common.remove")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const m = removing();
          if (m) remove.mutate(m.id);
        }}
      />
    </Show>
  );
}
