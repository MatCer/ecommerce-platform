import { t } from "@platform/storefront-sdk/format";
import type { SignInResult } from "@platform/storefront-sdk/types";
import { createSignal, Show } from "solid-js";
import { call } from "../lib/client";
import { Button } from "../ui.tsx";
import HydratedControls from "./HydratedControls";
import { problemText } from "./SignIn";

/**
 * Consumes a sign-in link with an explicit click: mail scanners that prefetch links cannot
 * burn the single-use token (it is only consumed by this POST).
 */
export default function VerifyLink(props: { m: Record<string, string>; token: string }) {
  const m = props.m;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  async function consume() {
    setBusy(true);
    setError("");
    const r = await call<SignInResult>("POST", "/_p/account/magic-link/consume", {
      token: props.token,
    });
    if (r.ok && r.data) return window.location.assign(r.data.redirect);
    setBusy(false);
    setError(r.status === 400 ? t(m, "account.link_invalid") : problemText(m, r.code));
  }

  return (
    <HydratedControls class="grid gap-3">
      <Button variant="primary" loading={busy()} onClick={consume}>
        {t(m, "account.verify_button")}
      </Button>
      <Show when={error()}>
        <p role="alert" class="text-sm text-sale">
          {error()}{" "}
          <a href="/account" class="underline">
            {t(m, "account.send_link")}
          </a>
        </p>
      </Show>
    </HydratedControls>
  );
}
