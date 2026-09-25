import { Button, TextField } from "@platform/ui";
import { useNavigate, useSearchParams } from "@solidjs/router";
import { createEffect, createSignal, type JSX, Match, Show, Switch } from "solid-js";
import { t } from "../i18n/index.ts";
import {
  requestPasswordReset,
  type SignInResult,
  sendMagicLink,
  signInWithPassword,
  status,
  verifyTotp,
} from "../lib/session.ts";

/** Maps Better Auth error codes to messages (no account enumeration beyond Better Auth's). */
export function signInError(code: string, httpStatus: number): string {
  if (httpStatus === 429) return t("auth.tooMany");
  if (code === "INVALID_EMAIL_OR_PASSWORD") return t("auth.invalidCredentials");
  if (code === "EMAIL_NOT_VERIFIED") return t("auth.emailNotVerified");
  if (/CODE|OTP|TOTP/.test(code)) return t("auth.invalidCode");
  return t("auth.failed");
}

/** Only same-origin paths are valid post-login targets (no open redirect). */
function safeNext(raw: string | string[] | undefined): string {
  const next = Array.isArray(raw) ? raw[0] : raw;
  return next?.startsWith("/") && !next.startsWith("//") ? next : "/";
}

type Step = "credentials" | "code" | "magic-sent" | "reset-sent";

export function AuthLayout(props: { title: string; lead?: string; children: JSX.Element }) {
  return (
    <main class="grid min-h-dvh place-items-center bg-background p-4">
      <div class="flex w-full max-w-sm flex-col gap-5 rounded-lg border border-border bg-card p-6">
        <div class="flex flex-col gap-1">
          <p class="col-label">{t("app.name")}</p>
          <h1 class="text-lg font-semibold tracking-tight">{props.title}</h1>
          <Show when={props.lead}>
            <p class="text-sm text-muted-foreground">{props.lead}</p>
          </Show>
        </div>
        {props.children}
      </div>
    </main>
  );
}

export function Login() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const [step, setStep] = createSignal<Step>("credentials");
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal<string | undefined>(
    params.error ? t("auth.resetInvalid") : undefined,
  );
  const [pending, setPending] = createSignal<"password" | "magic" | "reset" | "code" | null>(null);

  createEffect(() => {
    if (status() === "signed-in") navigate(safeNext(params.next), { replace: true });
  });

  const run = async (
    kind: "password" | "magic" | "reset" | "code",
    fn: () => Promise<SignInResult>,
  ) => {
    setError(undefined);
    setPending(kind);
    try {
      const r = await fn();
      if (r.kind === "error") setError(signInError(r.code, r.status));
      return r;
    } catch {
      setError(t("errors.network"));
      return null;
    } finally {
      setPending(null);
    }
  };

  const submitPassword = async (e: SubmitEvent) => {
    e.preventDefault();
    const r = await run("password", () => signInWithPassword(email().trim(), password()));
    if (r?.kind === "two-factor") setStep("code");
  };

  const submitCode = async (e: SubmitEvent) => {
    e.preventDefault();
    await run("code", () => verifyTotp(code().trim()));
  };

  const withEmail = (
    kind: "magic" | "reset",
    next: Step,
    fn: (e: string) => Promise<SignInResult>,
  ) => {
    if (!email().trim()) {
      setError(t("auth.enterEmail"));
      return;
    }
    void run(kind, () => fn(email().trim())).then((r) => r?.kind === "ok" && setStep(next));
  };

  const errorBox = () => (
    <Show when={error()}>
      <p role="alert" class="rounded-md bg-error-50 px-3 py-2 text-xs font-medium text-error-700">
        {error()}
      </p>
    </Show>
  );

  return (
    <Switch>
      <Match when={step() === "code"}>
        <AuthLayout title={t("auth.twoFactorTitle")} lead={t("auth.twoFactorLead")}>
          <form class="flex flex-col gap-3" onSubmit={submitCode}>
            <TextField
              label={t("auth.code")}
              value={code()}
              onChange={setCode}
              inputMode="numeric"
              autocomplete="one-time-code"
              maxLength={10}
              required
            />
            {errorBox()}
            <Button type="submit" variant="primary" loading={pending() === "code"}>
              {t("auth.verify")}
            </Button>
            <Button variant="ghost" onClick={() => setStep("credentials")}>
              {t("auth.backToSignIn")}
            </Button>
          </form>
        </AuthLayout>
      </Match>
      <Match when={step() === "magic-sent" || step() === "reset-sent"}>
        <AuthLayout title={t("auth.magicSentTitle")}>
          <p role="status" class="text-sm">
            {step() === "magic-sent"
              ? t("auth.magicSentDesc", { email: email() })
              : t("auth.resetSent", { email: email() })}
          </p>
          <Button variant="ghost" onClick={() => setStep("credentials")}>
            {t("auth.backToSignIn")}
          </Button>
        </AuthLayout>
      </Match>
      <Match when={true}>
        <AuthLayout title={t("auth.signInTitle")} lead={t("auth.signInLead")}>
          <form class="flex flex-col gap-3" onSubmit={submitPassword}>
            <TextField
              label={t("auth.email")}
              type="email"
              name="email"
              autocomplete="username"
              value={email()}
              onChange={setEmail}
              required
            />
            <TextField
              label={t("auth.password")}
              type="password"
              name="password"
              autocomplete="current-password"
              value={password()}
              onChange={setPassword}
              required
            />
            {errorBox()}
            <Button type="submit" variant="primary" loading={pending() === "password"}>
              {t("auth.signIn")}
            </Button>
          </form>
          <div class="flex items-center gap-3 text-xs text-faint-foreground" aria-hidden="true">
            <span class="h-px flex-1 bg-border" />
            {t("auth.or")}
            <span class="h-px flex-1 bg-border" />
          </div>
          <div class="flex flex-col gap-2">
            <Button
              loading={pending() === "magic"}
              onClick={() => withEmail("magic", "magic-sent", sendMagicLink)}
            >
              {t("auth.magicLink")}
            </Button>
            <button
              type="button"
              class="self-center text-xs text-accent-700 underline-offset-2 hover:underline"
              onClick={() => withEmail("reset", "reset-sent", requestPasswordReset)}
            >
              {t("auth.forgot")}
            </button>
          </div>
        </AuthLayout>
      </Match>
    </Switch>
  );
}
