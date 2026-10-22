import { describe, expect, it } from "vitest";
import { feedImporterHref, feedPrefill, logoAction, nameAction, shopUrl } from "./store-import.ts";

describe("shop address", () => {
  it("adds https:// to a bare domain", () => {
    expect(shopUrl("stara-kocka.cz")).toBe("https://stara-kocka.cz");
    expect(shopUrl("  www.stara-kocka.cz/  ")).toBe("https://www.stara-kocka.cz/");
  });

  it("keeps an explicit scheme so the API decides about http", () => {
    expect(shopUrl("https://stara-kocka.cz/")).toBe("https://stara-kocka.cz/");
    expect(shopUrl("http://mocks:4010/")).toBe("http://mocks:4010/");
  });
});

describe("feed importer link", () => {
  const search = (href: string) => new URL(href, "http://admin.localhost").search;

  it("opens the imports page with the feed form prefilled", () => {
    const href = feedImporterHref({ source: "google", url: "https://x.cz/feed.xml?a=1&b=2" });
    expect(href.startsWith("/imports?")).toBe(true);
    expect(feedPrefill(search(href))).toEqual({
      source: "google",
      url: "https://x.cz/feed.xml?a=1&b=2",
    });
  });

  it("prefills nothing without a feed and falls back to Heureka for an unknown source", () => {
    expect(feedPrefill("")).toBeUndefined();
    expect(feedPrefill("?source=google")).toBeUndefined();
    expect(feedPrefill("?source=store&feed=https%3A%2F%2Fx.cz%2Fh.xml")).toEqual({
      source: "heureka",
      url: "https://x.cz/h.xml",
    });
  });
});

describe("brand suggestion actions", () => {
  it("offers the name only when it differs from the current one", () => {
    expect(nameAction("Stará Kočka", "Moje Kočka")).toBe("apply");
    expect(nameAction("Stará Kočka", "Stará Kočka")).toBe("in_use");
    expect(nameAction(null, "Moje Kočka")).toBeUndefined();
    expect(nameAction("", "Moje Kočka")).toBeUndefined();
  });

  it("offers the logo once it is processed and not yet the email logo", () => {
    expect(logoAction({ id: "a", status: "ready" }, null)).toBe("apply");
    expect(logoAction({ id: "a", status: "ready" }, "b")).toBe("apply");
    expect(logoAction({ id: "a", status: "ready" }, "a")).toBe("in_use");
    expect(logoAction({ id: "a", status: "pending" }, null)).toBe("processing");
    expect(logoAction({ id: "a", status: "processing" }, null)).toBe("processing");
    expect(logoAction({ id: "a", status: "failed" }, null)).toBe("failed");
  });
});
