import { Alert, Button, linkClass, TextField } from "@platform/ui";
import { A, useSearchParams } from "@solidjs/router";
import { createSignal, Show } from "solid-js";
import { t } from "../i18n/index.ts";
import { resetPassword } from "../lib/session.ts";
import { AuthLayout, signInError } from "./Login.tsx";

/** Target of the reset email (Better Auth redirects here with `?token=`). */
export function ResetPassword() {
  const [params] = useSearchParams();
  const token = () => (typeof params.token === "string" ? params.token : "");
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal<string>();
  const [done, setDone] = createSignal(false);
  const [pending, setPending] = createSignal(false);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setPending(true);
    setError(undefined);
    try {
      const r = await resetPassword(token(), password());
      if (r.kind === "ok") setDone(true);
      else if (r.kind === "error")
        setError(r.code.includes("TOKEN") ? t("auth.resetInvalid") : signInError(r.code, r.status));
    } catch {
      setError(t("errors.network"));
    } finally {
      setPending(false);
    }
  };

  return (
    <AuthLayout title={t("auth.resetTitle")}>
      <Show
        when={token() && !params.error}
        fallback={<Alert tone="error">{t("auth.resetInvalid")}</Alert>}
      >
        <Show
          when={!done()}
          fallback={
            <Alert live tone="success">
              {t("auth.resetDone")}
            </Alert>
          }
        >
          <form class="flex flex-col gap-4" onSubmit={submit}>
            <TextField
              label={t("auth.newPassword")}
              description={t("auth.passwordHint")}
              type="password"
              autocomplete="new-password"
              value={password()}
              onChange={setPassword}
              error={error()}
              required
            />
            <Button
              type="submit"
              variant="confirm"
              block
              loading={pending()}
              disabled={password().length < 12}
            >
              {t("auth.resetSubmit")}
            </Button>
          </form>
        </Show>
      </Show>
      <A href="/login" class={`self-center text-sm ${linkClass}`}>
        {t("auth.backToSignIn")}
      </A>
    </AuthLayout>
  );
}
