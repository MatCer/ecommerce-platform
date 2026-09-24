import { expect, test } from "vitest";
import { HandoffStore } from "./handoff.ts";

const h = {
  checkoutHost: "checkout.demo.localhost",
  tenantId: "t1",
  cartToken: "cart_checkout_token",
};

test("single use", () => {
  const s = new HandoffStore();
  const t = s.mint(h, 0);
  expect(s.consume(t, h.checkoutHost, 1)).toEqual(h);
  expect(s.consume(t, h.checkoutHost, 2)).toBeNull();
});

test("expires after 60 s", () => {
  const s = new HandoffStore();
  const t = s.mint(h, 0);
  expect(s.consume(t, h.checkoutHost, 60_000)).toBeNull();
});

test("bound to the checkout host it was minted for, and burned by a wrong-host attempt", () => {
  const s = new HandoffStore();
  const t = s.mint(h, 0);
  expect(s.consume(t, "checkout.other.localhost", 1)).toBeNull();
  expect(s.consume(t, h.checkoutHost, 2)).toBeNull();
});

test("rejects malformed tokens without lookup", () => {
  expect(new HandoffStore().consume("../../etc", h.checkoutHost)).toBeNull();
});
