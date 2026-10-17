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
  const fetch = vi.fn(async () => new Response(null, { status }));
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

afterEach(() => vi.unstubAllGlobals());

test("a platform page whose token the edge refuses reloads /checkout and sends the call once", async () => {
  const assign = page("page-token");
  const fetch = answer(403);
  const r = await call("PUT", "/_p/checkout/shipping", { method_id: "m" });
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(assign).toHaveBeenCalledTimes(1);
  expect(new URL(assign.mock.calls[0]?.[0] as string, "https://shop.test").pathname).toBe(
    "/checkout",
  );
  expect(r.ok).toBe(false);
});

test("a page without a page token keeps a refusal as an error and stays", async () => {
  const assign = page(null);
  answer(403);
  const r = await call("PUT", "/_p/checkout/shipping", { method_id: "m" });
  expect(assign).not.toHaveBeenCalled();
  expect(r).toMatchObject({ ok: false, status: 403 });
});
