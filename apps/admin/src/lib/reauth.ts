/**
 * Re-authentication for sensitive operations (A9): the API answers `401 reauth_required` when
 * the login is older than 15 minutes. The API client parks the request here, the
 * ReauthDialog asks for the password (+ TOTP), and the request is retried once.
 */
import { createSignal } from "solid-js";

const [open, setOpen] = createSignal(false);

export { open as reauthOpen };

let waiting: Array<(ok: boolean) => void> = [];

/** Resolves `true` after a successful re-login, `false` if the user cancels. */
export function requestReauth(): Promise<boolean> {
  setOpen(true);
  return new Promise((resolve) => waiting.push(resolve));
}

export function finishReauth(ok: boolean): void {
  const done = waiting;
  waiting = [];
  setOpen(false);
  for (const resolve of done) resolve(ok);
}
