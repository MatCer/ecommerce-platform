import { t } from "@platform/storefront-sdk/format";
import type { SignInResult } from "@platform/storefront-sdk/types";
import { Button, TextField } from "@platform/ui";
import { createSignal, Show } from "solid-js";
import { call } from "../lib/client";
import HydratedControls from "./HydratedControls";

type M = Record<string, string>;

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export function problemText(m: M, code: string | null): string {
  switch (code) {
    case "invalid_credentials":
      return t(m, "account.invalid_credentials");
    case "too_many_attempts":
      return t(m, "account.too_many");
    case "invalid_email":
      return t(m, "account.invalid_email");
    default:
      return t(m, "account.error");
  }
}

/** Sign in with an email link (default) or a password (A5). */
export default function SignIn(props: { m: M; redirect?: string | undefined }) {
  const m = props.m;
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [busy, setBusy] = createSignal<"link" | "password" | null>(null);
  const [error, setError] = createSignal("");
  const [sent, setSent] = createSignal("");

  const validEmail = () => EMAIL.test(email().trim());

  async function sendLink(e: SubmitEvent) {
    e.preventDefault();
    setError("");
    setSent("");
    if (!validEmail()) return setError(t(m, "account.invalid_email"));
    setBusy("link");
    const r = await call("POST", "/_p/account/magic-link", {
      email: email().trim(),
      ...(props.redirect ? { redirect: props.redirect } : {}),
    });
    setBusy(null);
    if (r.ok) setSent(t(m, "account.link_sent", { email: email().trim() }));
    else setError(problemText(m, r.code));
  }

  async function signIn(e: SubmitEvent) {
    e.preventDefault();
    setError("");
    setSent("");
    if (!validEmail()) return setError(t(m, "account.invalid_email"));
    setBusy("password");
    const r = await call<SignInResult>("POST", "/_p/account/login", {
      email: email().trim(),
      password: password(),
      ...(props.redirect ? { redirect: props.redirect } : {}),
    });
    setBusy(null);
    if (r.ok && r.data) window.location.assign(r.data.redirect);
    else setError(problemText(m, r.code));
  }

  return (
    <HydratedControls class="grid gap-5">
      <form class="grid gap-3" noValidate onSubmit={sendLink} aria-describedby="sign-in-status">
        <TextField
          label={t(m, "account.email")}
          type="email"
          name="email"
          autocomplete="email"
          required
          value={email()}
          onChange={setEmail}
        />
        <Button type="submit" variant="confirm" loading={busy() === "link"}>
          {t(m, "account.send_link")}
        </Button>
      </form>

      <div id="sign-in-status" aria-live="polite">
        <Show when={sent()}>
          <p role="status" class="rounded-md bg-identity-wash p-3 text-sm">
            {sent()}
          </p>
        </Show>
        <Show when={error()}>
          <p role="alert" class="rounded-md border border-sale/40 p-3 text-sm text-sale">
            {error()}
          </p>
        </Show>
      </div>

      <details class="rounded-md border border-border p-3">
        <summary class="cursor-pointer text-sm font-semibold">
          {t(m, "account.with_password")}
        </summary>
        <form class="mt-3 grid gap-3" noValidate onSubmit={signIn}>
          <TextField
            label={t(m, "account.email")}
            type="email"
            name="username"
            autocomplete="username"
            required
            value={email()}
            onChange={setEmail}
          />
          <TextField
            label={t(m, "account.password")}
            type="password"
            name="password"
            autocomplete="current-password"
            required
            value={password()}
            onChange={setPassword}
          />
          <Button type="submit" loading={busy() === "password"}>
            {t(m, "account.sign_in_button")}
          </Button>
        </form>
      </details>
    </HydratedControls>
  );
}
