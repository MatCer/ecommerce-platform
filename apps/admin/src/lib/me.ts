import { createQuery } from "@tanstack/solid-query";
import { createEffect, createMemo } from "solid-js";
import { api, hasRole, type Role, type Schemas, setTenantId, tenantId, unwrap } from "./api.ts";

export type Membership = Schemas["Membership"];

/** `/admin/v1/me`: the user and the shops they belong to. */
export function useMe() {
  return createQuery(() => ({
    queryKey: ["me"],
    queryFn: () => unwrap(api.GET("/admin/v1/me")),
    staleTime: 60_000,
  }));
}

/**
 * The membership of the selected tenant. Falls back to the first membership when the stored
 * tenant is not (or no longer) one of the user's.
 */
export function useMembership() {
  const me = useMe();
  const current = createMemo<Membership | undefined>(() => {
    const list = me.data?.memberships ?? [];
    return list.find((m) => m.tenant_id === tenantId()) ?? list[0];
  });
  createEffect(() => {
    const m = current();
    if (m && m.tenant_id !== tenantId()) setTenantId(m.tenant_id);
  });
  const can = (min: Role) => hasRole(current()?.role, min);
  return { me, current, can };
}

/** Query keys are namespaced by tenant so switching shops never shows stale data. */
export function tenantKey(...parts: unknown[]): unknown[] {
  return ["t", tenantId(), ...parts];
}
