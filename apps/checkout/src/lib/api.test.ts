import { beforeEach, expect, test, vi } from "vitest";

const binding = vi.hoisted(() => ({ fetch: vi.fn<typeof fetch>() }));
vi.mock("cloudflare:workers", () => ({ env: { CHECKOUT: binding } }));

import { checkoutApi } from "./api";

const api = checkoutApi(
  new Request("https://checkout.example/", { headers: { "x-platform-ctx": "context" } }),
);
const token = "a".repeat(64);
const id = "12345678-1234-1234-1234-123456789abc";
beforeEach(() => binding.fetch.mockReset());

test.each([
  ["withdrawalForm", token, `/withdrawals/${token}`],
  ["myWithdrawalForm", id, `/customer/orders/${id}/withdrawal`],
  ["orderDocuments", token, `/orders/${token}/documents`],
  ["myDocuments", id, `/customer/orders/${id}/documents`],
] as const)(
  "%s uses the scoped binding and rejects malformed identifiers",
  async (method, value, path) => {
    binding.fetch.mockResolvedValueOnce(Response.json({ items: [] }));
    await expect(api[method](value)).resolves.toEqual({ items: [] });
    expect(binding.fetch).toHaveBeenCalledWith(`https://checkout${path}`, {
      headers: { "x-platform-ctx": "context" },
    });
    binding.fetch.mockClear();
    for (const invalid of [
      "",
      "short",
      `${value}/extra`,
      `${value}?token=forged`,
      value.toUpperCase(),
    ]) {
      await expect(api[method](invalid)).resolves.toBeNull();
    }
    expect(binding.fetch).not.toHaveBeenCalled();
  },
);

test("expired withdrawal links return null; upstream failures are not presented as expired", async () => {
  binding.fetch.mockResolvedValueOnce(new Response(null, { status: 404 }));
  await expect(api.withdrawalForm(token)).resolves.toBeNull();
  binding.fetch.mockResolvedValueOnce(new Response(null, { status: 503 }));
  await expect(api.withdrawalForm(token)).rejects.toThrow("checkout API 503");
});
