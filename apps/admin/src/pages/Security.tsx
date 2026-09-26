import { Badge, Button, FieldGroup, showToast, TextField } from "@platform/ui";
import { createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { renderSVG } from "uqr";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { authClient, claims, refreshToken, requestPasswordReset } from "../lib/session.ts";
import { signInError } from "./Login.tsx";

interface Enrollment {
  uri: string;
  secret: string;
  backupCodes: string[];
}

/** TOTP enrollment (Better Auth two-factor plugin) and the password needed for it. */
export default function Security() {
  const qc = useQueryClient();
  const state = createQuery(() => ({
    queryKey: ["security", claims()?.sub],
    queryFn: async () => {
      const [session, accounts] = await Promise.all([
        authClient.getSession(),
        authClient.listAccounts(),
      ]);
      if (session.error || accounts.error) throw new Error("session");
      const user = session.data?.user as { twoFactorEnabled?: boolean | null } | undefined;
      return {
        twoFactor: user?.twoFactorEnabled === true,
        hasPassword: (accounts.data ?? []).some((a) => a.providerId === "credential"),
      };
    },
  }));
  const [password, setPassword] = createSignal("");
  const [code, setCode] = createSignal("");
  const [enrollment, setEnrollment] = createSignal<Enrollment | null>(null);
  const [error, setError] = createSignal<string>();
  const [pending, setPending] = createSignal(false);

  const run = async (fn: () => Promise<{ error: { code?: string; status: number } | null }>) => {
    setPending(true);
    setError(undefined);
    try {
      const { error: e } = await fn();
      if (e) setError(signInError(e.code ?? "", e.status));
      return !e;
    } catch {
      setError(t("errors.network"));
      return false;
    } finally {
      setPending(false);
    }
  };

  const enable = async (e: SubmitEvent) => {
    e.preventDefault();
    let result: Enrollment | null = null;
    const ok = await run(async () => {
      const r = await authClient.twoFactor.enable({ password: password(), method: "totp" });
      if (r.data?.method === "totp") {
        const secret = new URL(r.data.totpURI).searchParams.get("secret") ?? "";
        result = { uri: r.data.totpURI, secret, backupCodes: r.data.backupCodes };
      }
      return r;
    });
    if (ok && result) {
      setEnrollment(result);
      setPassword("");
    }
  };

  const activate = async (e: SubmitEvent) => {
    e.preventDefault();
    const ok = await run(() => authClient.twoFactor.verifyTotp({ code: code().trim() }));
    if (ok) {
      // Enabling 2FA ends the old sessions; the plugin issued a new one: get a new token.
      await refreshToken();
      setEnrollment(null);
      setCode("");
      await qc.invalidateQueries({ queryKey: ["security"] });
      showToast({ title: t("security.enabled"), closeLabel: t("common.close") });
    }
  };

  const disable = async (e: SubmitEvent) => {
    e.preventDefault();
    const ok = await run(() => authClient.twoFactor.disable({ password: password() }));
    if (ok) {
      setPassword("");
      await qc.invalidateQueries({ queryKey: ["security"] });
      showToast({ title: t("security.disabled"), closeLabel: t("common.close") });
    }
  };

  const sendSetPassword = async () => {
    const ok = await run(async () => {
      const r = await requestPasswordReset(claims()?.email ?? "");
      return { error: r.kind === "error" ? { code: r.code, status: r.status } : null };
    });
    if (ok) showToast({ title: t("security.setPasswordSent"), closeLabel: t("common.close") });
  };

  const errorLine = () => (
    <Show when={error()}>
      <p role="alert" class="text-xs font-medium text-error-700">
        {error()}
      </p>
    </Show>
  );

  return (
    <>
      <PageHeader title={t("security.title")} />
      <QueryState query={state}>
        {(s) => (
          <section class="flex max-w-xl flex-col gap-4" aria-labelledby="tfa-h">
            <div class="flex items-center gap-2">
              <h2 id="tfa-h" class="text-base font-semibold">
                {t("security.twoFactor")}
              </h2>
              <Badge tone={s.twoFactor ? "success" : "neutral"}>
                {s.twoFactor ? t("security.on") : t("security.off")}
              </Badge>
            </div>
            <p class="text-sm text-muted-foreground">{t("security.lead")}</p>

            <Show when={enrollment()}>
              {(en) => (
                <FieldGroup legend={t("security.enable")}>
                  <p class="text-sm">{t("security.scan")}</p>
                  {/* uqr renders plain SVG paths from the otpauth URI (no user HTML). */}
                  <div
                    role="img"
                    aria-label={t("security.qrLabel")}
                    class="size-44 rounded-md bg-white p-2"
                    innerHTML={renderSVG(en().uri)}
                  />
                  <TextField
                    label={t("security.secret")}
                    value={en().secret}
                    onChange={() => {}}
                    readOnly
                    inputClass="figures"
                  />
                  <div>
                    <p class="text-xs font-medium text-muted-foreground">
                      {t("security.backupCodes")}
                    </p>
                    <p class="text-xs text-faint-foreground">{t("security.backupHint")}</p>
                    <ul class="figures mt-1 grid grid-cols-2 gap-1 text-sm">
                      <For each={en().backupCodes}>{(c) => <li>{c}</li>}</For>
                    </ul>
                  </div>
                  <form class="flex items-end gap-2" onSubmit={activate}>
                    <TextField
                      label={t("security.confirmCode")}
                      value={code()}
                      onChange={setCode}
                      inputMode="numeric"
                      autocomplete="one-time-code"
                      required
                    />
                    <Button type="submit" variant="confirm" loading={pending()}>
                      {t("security.activate")}
                    </Button>
                  </form>
                  {errorLine()}
                </FieldGroup>
              )}
            </Show>

            <Show when={!enrollment()}>
              <Show
                when={s.hasPassword}
                fallback={
                  <div class="flex flex-col items-start gap-2">
                    <p class="text-sm">{t("security.noPassword")}</p>
                    <Button onClick={() => void sendSetPassword()} loading={pending()}>
                      {t("security.sendSetPassword")}
                    </Button>
                    {errorLine()}
                  </div>
                }
              >
                <form
                  class="flex flex-wrap items-end gap-2"
                  onSubmit={s.twoFactor ? disable : enable}
                >
                  <TextField
                    label={t("security.currentPassword")}
                    type="password"
                    autocomplete="current-password"
                    value={password()}
                    onChange={setPassword}
                    required
                  />
                  <Button
                    type="submit"
                    variant={s.twoFactor ? "default" : "confirm"}
                    loading={pending()}
                  >
                    {s.twoFactor ? t("security.disable") : t("security.enable")}
                  </Button>
                </form>
                {errorLine()}
              </Show>
            </Show>
          </section>
        )}
      </QueryState>
    </>
  );
}
