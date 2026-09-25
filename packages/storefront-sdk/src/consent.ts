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

/** What the banner asked, for the record (`docs/decisions/consent-contract.md`). */
export interface ConsentAsked {
  /** The purposes offered (`ShopModel.consent.purposes`); the rest are left as they were. */
  offered: readonly ConsentPurpose[];
  /** The wording the visitor saw (`ShopModel.consent.text_version`). */
  textVersion: string;
}

/**
 * Saves a choice (also "nothing"): the cookie for 180 days, then records it with the platform
 * (`POST /_p/consent`, A20), which keeps the evidence and answers the anonymous subject cookie.
 * The report is best effort: offline, the choice still applies on this device. Withdrawn
 * purposes lose their device storage at once.
 */
export async function saveConsent(
  purposes: readonly ConsentPurpose[],
  asked: ConsentAsked,
  post: typeof fetch = globalThis.fetch,
): Promise<void> {
  const granted = PURPOSES.filter((p) => purposes.includes(p));
  // Scoped to the shop host (not host-only) so it is the same cookie the edge writes for the
  // checkout subdomain's preferences page.
  const domain = globalThis.location?.hostname ? `; Domain=${location.hostname}` : "";
  // biome-ignore lint/suspicious/noDocumentCookie: Cookie Store API is not available in Safari/Firefox
  document.cookie = `${COOKIE}=${encodeURIComponent(granted.join(","))}; Path=/; Max-Age=15552000; SameSite=Lax; Secure${domain}`;
  clearWithdrawn(granted);
  dispatchEvent(new CustomEvent(CONSENT_CHANGED, { detail: granted }));
  // The platform's contract: every offered purpose, granted or refused, and the text version.
  const body = JSON.stringify({
    purposes: Object.fromEntries(asked.offered.map((p) => [p, granted.includes(p)])),
    text_version: asked.textVersion,
    source: "banner",
  });
  // A beacon survives navigation and its answer is not ours to handle; fetch only where
  // beacons are unavailable.
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
    /* offline: the local choice stands */
  }
}

/** Reopens the consent banner with the current choice (e.g. a footer button). */
export const openConsentSettings = () => dispatchEvent(new Event(CONSENT_OPEN));

/**
 * Removes every SDK key (`sf:…`) not covered by a granted purpose: the storage of withdrawn
 * purposes, and anything unnamespaced or unknown (fail closed).
 */
function clearWithdrawn(granted: readonly ConsentPurpose[]) {
  const kept = granted.map((p) => `${STORAGE_PREFIX}${p}:`);
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const key = localStorage.key(i);
      if (key?.startsWith(STORAGE_PREFIX) && !kept.some((prefix) => key.startsWith(prefix)))
        localStorage.removeItem(key);
    }
  } catch {
    /* storage disabled */
  }
}

/**
 * Device storage that exists only while `purpose` is granted (A20: "recently viewed" needs
 * `personalization`). Keys live under `sf:<purpose>:`; without consent reads return `null`,
 * writes are dropped, and withdrawing the purpose deletes them.
 */
export function consentStorage(purpose: ConsentPurpose) {
  const key = (k: string) => `${STORAGE_PREFIX}${purpose}:${k}`;
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
