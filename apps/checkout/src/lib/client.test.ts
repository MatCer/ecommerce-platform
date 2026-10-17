import { afterEach, expect, test, vi } from "vitest";
import { call } from "./client";

function page(token: string | null) {
  vi.stubGlobal("document", {
    querySelector: (selector: string) =>
      selector === 'meta[name="page-token"]' && token !== null ? { content: token } : null,
  });
  const assign = vi.fn();
  vi.stubGlobal("location", { assign });
  return assign;
}

function answer(status: number) {
  const fetch = vi.fn(async () =>
    status === 200
      ? Response.json({ ok: true })
      : Response.json({ code: `code_${status}` }, { status }),
  );
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => vi.unstubAllGlobals());

const calls: [string, "GET" | "PUT" | "POST", string, unknown, Record<string, string>][] = [
  ["a read", "GET", "/_p/checkout", undefined, {}],
  ["a choice", "PUT", "/_p/checkout/shipping", { method_id: "m" }, {}],
  [
    "the placement",
    "POST",
    "/_p/checkout/place-order",
    { version: 1 },
    { "idempotency-key": "key-1" },
  ],
];

test.each(
  calls.flatMap(([label, ...rest]) =>
    [403, 404].map((status) => [label, status, ...rest] as const),
  ),
)(
  "%s refused with %i on a page with a token reloads the stale checkout once and sends nothing more",
  async (_, status, method, path, body, headers) => {
    const assign = page("page-token");
    const fetch = answer(status);
    const r = await call(method, path, body, headers);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(assign.mock.calls).toEqual([["/checkout?error=stale_page"]]);
    expect(r).toEqual({ ok: false, status, data: null, code: "stale_page" });
  },
);

test.each([200, 409, 422])("a %i on a page with a token stays on the page", async (status) => {
  const assign = page("page-token");
  answer(status);
  const r = await call("PUT", "/_p/checkout/shipping", { method_id: "m" });
  expect(assign).not.toHaveBeenCalled();
  expect(r).toMatchObject({ ok: status === 200, status });
  if (status !== 200) expect(r.code).toBe(`code_${status}`);
});

test("a network error on a page with a token stays on the page", async () => {
  const assign = page("page-token");
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new TypeError("offline");
    }),
  );
  const r = await call("PUT", "/_p/checkout/shipping", { method_id: "m" });
  expect(assign).not.toHaveBeenCalled();
  expect(r).toEqual({ ok: false, status: 0, data: null, code: "network" });
});

test.each([403, 404])(
  "a page without a page token keeps a %i as an error and stays",
  async (status) => {
    const assign = page(null);
    answer(status);
    const r = await call("PUT", "/_p/checkout/shipping", { method_id: "m" });
    expect(assign).not.toHaveBeenCalled();
    expect(r).toEqual({ ok: false, status, data: null, code: `code_${status}` });
  },
);
