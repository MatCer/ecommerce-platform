import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { consentStorage, readConsent, saveConsent } from "./consent.ts";

// A minimal browser: a cookie jar that keeps only the last `consent=` write, localStorage,
// and window events.
let jar = "";
let store: Map<string, string>;
const events: string[] = [];

beforeEach(() => {
  jar = "";
  store = new Map();
  events.length = 0;
  vi.stubGlobal("document", {
    get cookie() {
      return jar;
    },
    set cookie(v: string) {
      jar = v.split(";")[0] ?? "";
    },
  });
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    key: (i: number) => [...store.keys()][i] ?? null,
    get length() {
      return store.size;
    },
  });
  vi.stubGlobal("dispatchEvent", (e: Event) => events.push(e.type));
});
afterEach(() => vi.unstubAllGlobals());

test("cookie parsing ignores unknown purposes and mangled values", () => {
  expect(readConsent("a=1; consent=analytics%2Cevil%2Cads")).toEqual(["analytics", "ads"]);
  expect(readConsent("consent=")).toEqual([]);
  expect(readConsent("consent=%E0%A4%A")).toBeNull();
  expect(readConsent("x=1")).toBeNull();
});

test("nothing is stored before a choice; storage follows the personalization purpose", async () => {
  const recent = consentStorage("personalization");
  recent.set("recent", ["a"]);
  expect(store.size).toBe(0);
  expect(recent.get("recent")).toBeNull();

  const post = vi.fn(async () => new Response(null, { status: 404 }));
  await saveConsent(["personalization", "analytics"], post);
  expect(readConsent()).toEqual(["analytics", "personalization"]);
  expect(events).toEqual(["platform:consent"]);
  recent.set("recent", ["a"]);
  expect(recent.get("recent")).toEqual(["a"]);
  expect(store.get("sf:recent")).toBe('["a"]');

  // Withdrawing personalization removes what it allowed to store.
  store.set("other-app", "kept");
  await saveConsent(["analytics"], post);
  expect([...store.keys()]).toEqual(["other-app"]);
  expect(recent.get("recent")).toBeNull();
});

test("the choice is reported to the platform and survives a missing endpoint", async () => {
  const post = vi.fn(async (_url: string, _init?: RequestInit) => {
    throw new TypeError("offline");
  });
  await expect(saveConsent(["ads"], post as typeof fetch)).resolves.toBeUndefined();
  expect(readConsent()).toEqual(["ads"]);
  const [url, init] = post.mock.calls[0] ?? [];
  expect(url).toBe("/_p/consent");
  expect(init).toMatchObject({ method: "POST", body: '{"purposes":["ads"]}' });
});
