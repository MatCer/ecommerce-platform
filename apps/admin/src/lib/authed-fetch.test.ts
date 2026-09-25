import { describe, expect, it, vi } from "vitest";
import { type AuthDeps, createAuthedFetch } from "./authed-fetch.ts";

function problem(status: number, code: string): Response {
  return new Response(JSON.stringify({ status, code, title: code, type: "about:blank" }), {
    status,
    headers: { "content-type": "application/problem+json" },
  });
}

function deps(responses: Response[], over: Partial<AuthDeps> = {}) {
  const seen: Request[] = [];
  let n = 0;
  const d: AuthDeps = {
    token: async () => `t${n}`,
    refresh: vi.fn(async () => {
      n += 1;
    }),
    reauth: vi.fn(async () => true),
    tenant: () => "tenant-1",
    fetch: async (req) => {
      seen.push(req);
      const next = responses.shift();
      if (!next) throw new Error("unexpected request");
      return next;
    },
    ...over,
  };
  return { d, seen };
}

const post = () =>
  new Request("http://api.test/admin/v1/staff/invitations", {
    method: "POST",
    body: '{"email":"a@b.cz"}',
    headers: { "idempotency-key": "k1", "content-type": "application/json" },
  });

describe("createAuthedFetch", () => {
  it("adds the bearer token and tenant header", async () => {
    const { d, seen } = deps([new Response("{}", { status: 200 })]);
    const res = await createAuthedFetch(d)(post());
    expect(res.status).toBe(200);
    expect(seen[0]?.headers.get("authorization")).toBe("Bearer t0");
    expect(seen[0]?.headers.get("x-tenant-id")).toBe("tenant-1");
  });

  it("refreshes the token once on invalid_token and retries the same body", async () => {
    const { d, seen } = deps([problem(401, "invalid_token"), new Response("{}", { status: 201 })]);
    const res = await createAuthedFetch(d)(post());
    expect(res.status).toBe(201);
    expect(d.refresh).toHaveBeenCalledTimes(1);
    expect(seen[1]?.headers.get("authorization")).toBe("Bearer t1");
    expect(await seen[1]?.text()).toBe('{"email":"a@b.cz"}');
    expect(seen[1]?.headers.get("idempotency-key")).toBe("k1");
  });

  it("asks for re-authentication on reauth_required and retries", async () => {
    const { d } = deps([problem(401, "reauth_required"), new Response(null, { status: 204 })]);
    const res = await createAuthedFetch(d)(post());
    expect(d.reauth).toHaveBeenCalledTimes(1);
    expect(res.status).toBe(204);
  });

  it("returns the 401 when the user cancels re-authentication", async () => {
    const { d, seen } = deps([problem(401, "reauth_required")], { reauth: async () => false });
    const res = await createAuthedFetch(d)(post());
    expect(res.status).toBe(401);
    expect(seen).toHaveLength(1);
  });

  it("does not retry other errors", async () => {
    const { d, seen } = deps([problem(403, "insufficient_role")]);
    const res = await createAuthedFetch(d)(post());
    expect(res.status).toBe(403);
    expect(seen).toHaveLength(1);
  });
});
