import { t } from "@platform/storefront-sdk/format";
import { Button, TextField } from "@platform/ui";
import { createSignal, Show } from "solid-js";
import { call } from "../lib/client";
import HydratedControls from "./HydratedControls";
import { problemText } from "./SignIn";

type M = Record<string, string>;

/**
 * Set or change the password (A5): the current password is needed unless the session signed
 * in with an email link in the last 10 minutes. Without either, offer a fresh link.
 */
export default function PasswordForm(props: {
  m: M;
  email: string;
  hasPassword: boolean;
  recentlyVerified: boolean;
}) {
  const m = props.m;
  const [hasPassword, setHasPassword] = createSignal(props.hasPassword);
  const [current, setCurrent] = createSignal("");
  const [next, setNext] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [done, setDone] = createSignal("");
  const needsCurrent = () => !props.recentlyVerified && hasPassword();
  const blocked = () => !props.recentlyVerified && !hasPassword();

  function explain(code: string | null): string {
    switch (code) {
      case "reauth_required":
        return t(m, "password.reauth");
      case "invalid_current_password":
        return t(m, "password.wrong_current");
      case "weak_password":
        return t(m, "password.weak");
      default:
        return problemText(m, code);
    }
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    setError("");
    setDone("");
    if (next().length < 10) return setError(t(m, "password.weak"));
    setBusy(true);
    const r = await call("POST", "/_p/account/password", {
      new_password: next(),
      ...(needsCurrent() ? { current_password: current() } : {}),
    });
    setBusy(false);
    if (!r.ok) return setError(explain(r.code));
    setHasPassword(true);
    setCurrent("");
    setNext("");
    setDone(t(m, "password.saved"));
  }

  async function sendLink() {
    setError("");
    const r = await call("POST", "/_p/account/magic-link", {
      email: props.email,
      redirect: "/account/security",
    });
    if (r.ok) setDone(t(m, "account.link_sent", { email: props.email }));
    else setError(problemText(m, r.code));
  }

  return (
    <HydratedControls class="grid gap-4">
      <p class="text-sm text-muted-foreground">{t(m, "password.set_intro")}</p>
      <Show
        when={!blocked()}
        fallback={
          <div class="grid gap-3">
            <p class="text-sm">{t(m, "password.reauth")}</p>
            <Button variant="confirm" onClick={sendLink}>
              {t(m, "account.send_link")}
            </Button>
          </div>
        }
      >
        <form class="grid gap-3" noValidate onSubmit={save}>
          {/* Lets password managers attach the new password to the right account. */}
          <input type="email" name="username" autocomplete="username" value={props.email} hidden />
          <Show when={needsCurrent()}>
            <TextField
              label={t(m, "password.current")}
              type="password"
              autocomplete="current-password"
              required
              value={current()}
              onChange={setCurrent}
            />
          </Show>
          <TextField
            label={t(m, "password.new")}
            description={t(m, "password.hint")}
            type="password"
            autocomplete="new-password"
            required
            maxLength={128}
            value={next()}
            onChange={setNext}
          />
          <Button type="submit" variant="confirm" loading={busy()}>
            {t(m, "password.save")}
          </Button>
        </form>
      </Show>
      <div aria-live="polite">
        <Show when={done()}>
          <p role="status" class="rounded-md bg-identity-wash p-3 text-sm">
            {done()}
          </p>
        </Show>
        <Show when={error()}>
          <p role="alert" class="text-sm text-sale">
            {error()}
          </p>
        </Show>
      </div>
    </HydratedControls>
  );
}
