import { Button, Dialog, TextField } from "@platform/ui";
import { createSignal, Show } from "solid-js";
import { t } from "../i18n/index.ts";
import { finishReauth, reauthOpen } from "../lib/reauth.ts";
import { claims, type SignInResult, signInWithPassword, verifyTotp } from "../lib/session.ts";
import { signInError } from "../pages/Login.tsx";

/** Password (+ TOTP) re-login for `401 reauth_required`; the parked request is then retried. */
export function ReauthDialog() {
  const [password, setPassword] = createSignal("");
  const [code, setCode] = createSignal("");
  const [step, setStep] = createSignal<"password" | "code">("password");
  const [error, setError] = createSignal<string>();
  const [pending, setPending] = createSignal(false);

  const reset = () => {
    setPassword("");
    setCode("");
    setStep("password");
    setError(undefined);
  };

  const handle = (r: SignInResult) => {
    if (r.kind === "ok") {
      reset();
      finishReauth(true);
    } else if (r.kind === "two-factor") {
      setStep("code");
      setError(undefined);
    } else {
      setError(signInError(r.code, r.status));
    }
  };

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setPending(true);
    try {
      handle(
        step() === "password"
          ? await signInWithPassword(claims()?.email ?? "", password())
          : await verifyTotp(code().trim()),
      );
    } finally {
      setPending(false);
    }
  };

  const cancel = () => {
    reset();
    finishReauth(false);
  };

  return (
    <Dialog
      open={reauthOpen()}
      onOpenChange={(open) => !open && cancel()}
      title={t("auth.reauthTitle")}
      description={t("auth.reauthDesc")}
      size="sm"
    >
      <form class="flex flex-col gap-3" onSubmit={submit}>
        <TextField
          label={t("auth.email")}
          value={claims()?.email ?? ""}
          onChange={() => {}}
          readOnly
        />
        <Show
          when={step() === "code"}
          fallback={
            <TextField
              label={t("auth.password")}
              type="password"
              autocomplete="current-password"
              value={password()}
              onChange={setPassword}
              required
            />
          }
        >
          <TextField
            label={t("auth.code")}
            inputMode="numeric"
            autocomplete="one-time-code"
            value={code()}
            onChange={setCode}
            required
          />
        </Show>
        <Show when={error()}>
          <p role="alert" class="text-xs font-medium text-error-700">
            {error()}
          </p>
        </Show>
        <div class="flex justify-end gap-2">
          <Button onClick={cancel}>{t("common.cancel")}</Button>
          <Button type="submit" variant="primary" loading={pending()}>
            {step() === "code" ? t("auth.verify") : t("auth.reauthSubmit")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
