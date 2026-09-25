import type { ConsentPurpose } from "./types.ts";

/**
 * Consent on the shop origin (spec §11.3, A20). Nothing is stored on the device until the
 * visitor chooses; the choice itself is a strictly necessary cookie. The platform records it
 * server-side through `POST /_p/consent` and resolves consent from its own records when it
 * matters (sending, forwarding), so the browser copy only decides what the page may do.
 */

export const PURPOSES: readonly ConsentPurpose[] = [
  "analytics",
  "ads",
  "personalization",
  "email_marketing",
  "review_invites",
];

const COOKIE = "consent";
/** Fired on `window` after a choice is saved; `detail` is the granted purposes. */
export const CONSENT_CHANGED = "platform:consent";
/** Fired on `window` to reopen the banner in settings mode (footer "Cookie settings"). */
export const CONSENT_OPEN = "platform:consent-open";
/** Device storage written through `consentStorage` uses this key prefix. */
const STORAGE_PREFIX = "sf:";

/** Granted purposes, or `null` when the visitor has not decided yet (show the banner). */
export function readConsent(cookie = globalThis.document?.cookie ?? ""): ConsentPurpose[] | null {
  const raw = cookie
    .split(";")
    .map((c) => c.trim())
    .find((c) => c.startsWith(`${COOKIE}=`))
    ?.slice(COOKIE.length + 1);
  if (raw === undefined) return null;
  let decoded: string;
  try {
    decoded = decodeURIComponent(raw);
  } catch {
    return null; // a mangled cookie is no decision: ask again
  }
  return decoded.split(",").filter((p): p is ConsentPurpose => PURPOSES.includes(p as never));
}

export const hasConsent = (purpose: ConsentPurpose) => readConsent()?.includes(purpose) ?? false;

/**
 * Saves a choice (also "nothing"): the cookie for 180 days, then reports it to the platform.
 * The report is best effort: until the consent endpoint exists (or when offline) the choice
 * still applies on this device. Withdrawn purposes lose their device storage at once.
 */
export async function saveConsent(
  purposes: readonly ConsentPurpose[],
  post: typeof fetch = globalThis.fetch,
): Promise<void> {
  const granted = PURPOSES.filter((p) => purposes.includes(p));
  // biome-ignore lint/suspicious/noDocumentCookie: Cookie Store API is not available in Safari/Firefox
  document.cookie = `${COOKIE}=${encodeURIComponent(granted.join(","))}; Path=/; Max-Age=15552000; SameSite=Lax; Secure`;
  if (!granted.includes("personalization")) clearStorage();
  dispatchEvent(new CustomEvent(CONSENT_CHANGED, { detail: granted }));
  // A beacon survives navigation and its answer is not ours to handle (a 404 before the
  // endpoint exists stays silent); fetch only where beacons are unavailable.
  const body = JSON.stringify({ purposes: granted });
  if (
    globalThis.navigator?.sendBeacon?.(
      "/_p/consent",
      new Blob([body], { type: "application/json" }),
    )
  )
    return;
  try {
    await post("/_p/consent", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
      credentials: "same-origin",
      keepalive: true,
    });
  } catch {
    /* offline or not deployed yet: the local choice stands */
  }
}

/** Reopens the consent banner with the current choice (e.g. a footer button). */
export const openConsentSettings = () => dispatchEvent(new Event(CONSENT_OPEN));

function clearStorage() {
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const key = localStorage.key(i);
      if (key?.startsWith(STORAGE_PREFIX)) localStorage.removeItem(key);
    }
  } catch {
    /* storage disabled */
  }
}

/**
 * Device storage that exists only while `purpose` is granted (A20: "recently viewed" needs
 * `personalization`). Without consent reads return `null` and writes are dropped.
 */
export function consentStorage(purpose: ConsentPurpose) {
  const key = (k: string) => `${STORAGE_PREFIX}${k}`;
  return {
    get<T>(k: string): T | null {
      if (!hasConsent(purpose)) return null;
      try {
        const raw = localStorage.getItem(key(k));
        return raw === null ? null : (JSON.parse(raw) as T);
      } catch {
        return null;
      }
    },
    set(k: string, value: unknown) {
      if (!hasConsent(purpose)) return;
      try {
        localStorage.setItem(key(k), JSON.stringify(value));
      } catch {
        /* quota or storage disabled */
      }
    },
  };
}
