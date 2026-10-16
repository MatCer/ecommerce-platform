import { renderToString } from "solid-js/web";
import { expect, test } from "vitest";
import ConsentPanel from "./ConsentPanel.tsx";

const tags = (html: string, name: string) =>
  [...html.matchAll(new RegExp(`<${name}\\b[^>]*>`, "g"))].map((m) => m[0]);
const attr = (tag: string, name: string) =>
  new RegExp(`\\s${name}="([^"]*)"`).exec(tag)?.[1] ?? null;
const textOf = (html: string) => html.replace(/<!--.*?-->|<[^>]+>/g, "");

test("settings show a purpose's hint as its checkbox's description, not its name", () => {
  const html = renderToString(() => (
    <ConsentPanel
      purposes={["analytics", "personalization"]}
      policyUrl="/cookies"
      textVersion="2026-10-16"
      labels={{
        "consent.necessary": "Necessary",
        "consent.analytics": "Analytics",
        "consent.analytics_hint": "Visit statistics and testing of page variants.",
        "consent.personalization": "Personalization",
      }}
      settings
      onClose={() => {}}
    />
  ));

  expect(tags(html, "input").map((t) => attr(t, "aria-describedby"))).toEqual([
    null,
    "pf-consent-hint-analytics",
    null,
  ]);
  const hint = /<p\b[^>]*id="pf-consent-hint-analytics"[^>]*>(.*?)<\/p>/.exec(html);
  expect(hint && textOf(hint[1] ?? "")).toBe("Visit statistics and testing of page variants.");
  expect(
    [...html.matchAll(/<label\b[^>]*>(.*?)<\/label>/g)].map((m) => textOf(m[1] ?? "")),
  ).toEqual(["Necessary", "Analytics", "Personalization"]);
});
