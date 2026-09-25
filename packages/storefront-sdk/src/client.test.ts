import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { createBeacon } from "./client.ts";

let cookie = "";
const sent: string[] = [];

beforeEach(() => {
  sent.length = 0;
  vi.stubGlobal("document", { cookie: "", visibilityState: "visible" });
  Object.defineProperty(document, "cookie", { get: () => cookie, configurable: true });
  vi.stubGlobal("addEventListener", () => {});
  vi.stubGlobal("location", { pathname: "/p/tee" });
  vi.stubGlobal("navigator", { sendBeacon: (_url: string, body: string) => sent.push(body) });
});
afterEach(() => vi.unstubAllGlobals());

test("the beacon sends with analytics or ads consent, nothing without either", () => {
  for (const [consent, expected] of [
    ["", 0],
    ["personalization", 0],
    ["ads", 1],
    ["analytics", 1],
  ] as const) {
    cookie = consent ? `consent=${consent}` : "";
    sent.length = 0;
    const b = createBeacon();
    b.track({ type: "page_view", template: "home" });
    b.flush();
    expect(sent.length, consent).toBe(expected);
  }
  expect(JSON.parse(sent[0] ?? "{}")).toEqual({
    events: [{ type: "page_view", template: "home" }],
    path: "/p/tee",
  });
});
