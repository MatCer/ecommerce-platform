import { type components, createAdminClient } from "@platform/admin-client";
import { QueryClient } from "@tanstack/solid-query";
import { createSignal } from "solid-js";
import { ApiError, createAuthedFetch, problemOf } from "./authed-fetch.ts";
import { API_ORIGIN } from "./config.ts";
import { requestReauth } from "./reauth.ts";
import { accessToken, onSubjectChange, refreshToken, SignedOutError } from "./session.ts";

export { ApiError };
export type Schemas = components["schemas"];
export type Role = Schemas["Role"];

const TENANT_KEY = "admin.tenant";
const [tenantId, setTenantSignal] = createSignal<string | null>(
  globalThis.localStorage?.getItem(TENANT_KEY) ?? null,
);

export { tenantId };

/** The tenant id is not a secret; remembering it keeps the switcher choice across reloads. */
export function setTenantId(id: string): void {
  localStorage.setItem(TENANT_KEY, id);
  setTenantSignal(id);
}

export const api = createAdminClient({
  baseUrl: API_ORIGIN,
  fetch: createAuthedFetch({
    token: accessToken,
    refresh: refreshToken,
    reauth: requestReauth,
    tenant: tenantId,
    fetch: (req) => fetch(req),
  }),
});

/**
 * Resolves to the response body or throws an `ApiError` carrying the problem `code`.
 * (`204 No Content` resolves to `undefined`; callers of such endpoints ignore the value.)
 */
export async function unwrap<T>(
  pending: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<Exclude<T, undefined>> {
  const r = await pending;
  if (r.error !== undefined || !r.response.ok) throw await problemOf(r.response);
  return r.data as Exclude<T, undefined>;
}

/** `X-Tenant-Id` of the selected tenant, captured when the request is built. */
export function tenantHeader(): { "X-Tenant-Id": string } {
  const t = tenantId();
  if (!t) throw new ApiError(400, "tenant_required");
  return { "X-Tenant-Id": t };
}

/**
 * One `Idempotency-Key` per logical submission of a form: pressing Save again with the same body
 * (e.g. after a lost response) reuses the key, so the server replays instead of applying twice.
 * A changed body gets a new key; call `done()` after success.
 */
export function submission() {
  let last: { body: string; key: string } | null = null;
  return {
    header(body: unknown): { "X-Tenant-Id": string; "Idempotency-Key": string } {
      const serialized = `${tenantId()}|${JSON.stringify(body)}`;
      if (last?.body !== serialized) last = { body: serialized, key: crypto.randomUUID() };
      return { ...tenantHeader(), "Idempotency-Key": last.key };
    },
    done(): void {
      last = null;
    },
  };
}

/** Tenant header plus a fresh `Idempotency-Key` for one user action (retries reuse it). */
export function idempotencyKey(): { "X-Tenant-Id": string; "Idempotency-Key": string } {
  return { ...tenantHeader(), "Idempotency-Key": crypto.randomUUID() };
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      retry: (count, error) =>
        !(error instanceof ApiError && error.status < 500) &&
        !(error instanceof SignedOutError) &&
        count < 2,
    },
  },
});

// Another user (or none) now: drop every cached response of the previous one.
onSubjectChange(() => queryClient.clear());

const ORDER: Record<Role, number> = { staff: 0, admin: 1, owner: 2 };

export function hasRole(role: Role | undefined, min: Role): boolean {
  return role !== undefined && ORDER[role] >= ORDER[min];
}
