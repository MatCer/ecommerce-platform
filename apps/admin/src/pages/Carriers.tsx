import { Badge, Button, Dialog, PermissionDenied, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { ApiProblem } from "../components/CheckoutSettings.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

export default function Carriers() {
  const { can } = useMembership();
  const query = createQuery(() => ({
    queryKey: tenantKey("carriers"),
    enabled: can("admin"),
    queryFn: () => unwrap(api.GET("/admin/v1/carriers", { params: { header: tenantHeader() } })),
  }));
  return (
    <>
      <PageHeader title={t("fulfillment.carriers")} />
      <Show
        when={can("admin")}
        fallback={
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        }
      >
        <QueryState query={query}>
          {(data) => (
            <div class="grid gap-4 lg:grid-cols-2">
              <For each={["packeta", "ppl"] as const}>
                {(carrier) => (
                  <CarrierForm
                    carrier={carrier}
                    account={data.items.find((a) => a.carrier === carrier)}
                  />
                )}
              </For>
            </div>
          )}
        </QueryState>
      </Show>
    </>
  );
}

function CarrierForm(props: {
  carrier: Schemas["CarrierKind"];
  account?: Schemas["CarrierAccount"];
}) {
  const qc = useQueryClient();
  const key = tenantKey("carriers");
  const [sender, setSender] = createSignal(props.account?.sender_label ?? "");
  const [password, setPassword] = createSignal("");
  const [clientId, setClientId] = createSignal("");
  const [secret, setSecret] = createSignal("");
  const [removing, setRemoving] = createSignal(false);
  const packeta = () => props.carrier === "packeta";
  const passwordValid = () => !password() || /^[a-fA-F0-9]{32}$/.test(password());
  const clearSecrets = () => {
    setPassword("");
    setClientId("");
    setSecret("");
  };
  const saved = () => {
    clearSecrets();
    setRemoving(false);
    showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    void qc.invalidateQueries({ queryKey: key });
  };
  // The shared authed fetch parks 401 reauth_required and retries once after ReauthDialog.
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/carriers/{carrier}", {
          params: { header: tenantHeader(), path: { carrier: props.carrier } },
          body: {
            sender_label: sender().trim(),
            ...(packeta()
              ? { api_password: password() || undefined }
              : { client_id: clientId() || undefined, client_secret: secret() || undefined }),
          },
        }),
      ),
    onSuccess: saved,
  }));
  const remove = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.DELETE("/admin/v1/carriers/{carrier}", {
          params: { header: tenantHeader(), path: { carrier: props.carrier } },
        }),
      ),
    onSuccess: saved,
  }));
  const pending = () => save.isPending || remove.isPending;
  return (
    <section class="rounded-md border border-border p-4">
      <h2 class="mb-3 font-semibold">{packeta() ? "Packeta" : "PPL"}</h2>
      <div class="mb-3 grid gap-2 text-sm">
        <Badge tone={props.account?.configured ? "success" : "neutral"}>
          {props.account?.configured ? t("fulfillment.configured") : t("fulfillment.notConfigured")}
        </Badge>
        <Show when={props.account?.public_key}>
          <p class="break-all">
            {t("fulfillment.publicKey")}: {props.account?.public_key}
          </p>
        </Show>
        <Show when={props.account?.updated_at}>
          {(at) => <time dateTime={at()}>{formatDateTime(at())}</time>}
        </Show>
      </div>
      <form
        class="grid gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          if (passwordValid() && !pending()) save.mutate();
        }}
      >
        <fieldset disabled={pending()} class="grid min-w-0 gap-3">
          <TextField
            label={t("fulfillment.senderLabel")}
            required
            value={sender()}
            onChange={setSender}
          />
          <Show
            when={packeta()}
            fallback={
              <>
                <TextField
                  label={t("fulfillment.clientId")}
                  autocomplete="off"
                  value={clientId()}
                  onChange={setClientId}
                  required={!props.account?.configured || Boolean(secret())}
                />
                <TextField
                  label={t("fulfillment.clientSecret")}
                  type="password"
                  autocomplete="new-password"
                  value={secret()}
                  onChange={setSecret}
                  required={!props.account?.configured || Boolean(clientId())}
                />
              </>
            }
          >
            <TextField
              label={t("fulfillment.apiPassword")}
              type="password"
              autocomplete="new-password"
              value={password()}
              onChange={setPassword}
              required={!props.account?.configured}
              maxLength={32}
              description={t("fulfillment.packetaHint")}
              error={!passwordValid() && t("fulfillment.packetaHint")}
            />
          </Show>
          <Show when={props.account?.configured}>
            <p class="text-xs text-muted-foreground">{t("fulfillment.secretHint")}</p>
          </Show>
        </fieldset>
        <ApiProblem error={save.error} />
        <div class="flex gap-2">
          <Button
            type="submit"
            variant="confirm"
            loading={save.isPending}
            disabled={pending() || !passwordValid()}
          >
            {t("common.save")}
          </Button>
          <Show when={props.account?.configured}>
            <Button
              disabled={pending()}
              onClick={() => {
                remove.reset();
                setRemoving(true);
              }}
            >
              {t("common.delete")}
            </Button>
          </Show>
        </div>
      </form>
      <Dialog
        open={removing()}
        onOpenChange={(open) => !pending() && setRemoving(open)}
        title={t("fulfillment.removeCarrier")}
        description={t("fulfillment.removeCarrierHint")}
      >
        <ApiProblem error={remove.error} />
        <div class="flex justify-end gap-2">
          <Button disabled={pending()} onClick={() => setRemoving(false)}>
            {t("common.cancel")}
          </Button>
          <Button variant="danger" loading={remove.isPending} onClick={() => remove.mutate()}>
            {t("common.delete")}
          </Button>
        </div>
      </Dialog>
    </section>
  );
}
