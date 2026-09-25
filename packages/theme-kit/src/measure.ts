#!/usr/bin/env node
/**
 * Lab performance + accessibility gate for a theme (spec §9.6, A26).
 *
 *   node packages/theme-kit/src/measure.ts --base http://demo.localhost:8280 \
 *     --pages /,/c/trika,/p/tricko-basic --runs 3 [--out .perf/report.json] [--no-fail]
 *
 * - JS: Playwright, mobile viewport. Every executable script transferred until network idle,
 *   then the page is scrolled to the bottom so `client:visible` islands load, then idle again.
 *   Counted as gzip -9 of each script body (the budget's unit) and as the actual transfer size.
 * - LCP/TBT/CLS: Lighthouse, default mobile preset (Moto G Power, slow 4G, 4× CPU), median of
 *   `--runs`. Runs are serial (machine limits).
 * - axe (WCAG 2.2 AA) serious/critical violations, CSP violations, third-party origins.
 */
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { parseArgs } from "node:util";
import { gzipSync } from "node:zlib";
import { AxeBuilder } from "@axe-core/playwright";
import lighthouse from "lighthouse";
import { type BrowserContext, chromium } from "playwright";
import { BUDGET, judge, median, type PageResult } from "./budget.ts";

const { values } = parseArgs({
  options: {
    base: { type: "string", default: "http://demo.localhost:8280" },
    pages: { type: "string", default: "/,/c/trika,/p/tricko-basic,/search?q=mikina" },
    runs: { type: "string", default: "3" },
    out: { type: "string", default: ".perf/report.json" },
    "no-fail": { type: "boolean", default: false },
    "skip-lighthouse": { type: "boolean", default: false },
    explain: { type: "boolean", default: false },
  },
});

const base = new URL(values.base);
const pages = values.pages.split(",").filter(Boolean);
const runs = Math.max(1, Number(values.runs));
// Local HTTPS (Caddy `tls internal`) to measure over TLS + HTTP/2 like production.
const LOCAL_TLS = base.protocol === "https:" && base.hostname.endsWith(".localhost");
const kindOf = (p: string): PageResult["kind"] =>
  p === "/" ? "home" : p.startsWith("/c/") ? "category" : p.startsWith("/p/") ? "product" : "other";

const MOBILE = {
  viewport: { width: 412, height: 823 },
  deviceScaleFactor: 1.75,
  isMobile: true,
  hasTouch: true,
  userAgent:
    "Mozilla/5.0 (Linux; Android 11; moto g power (2022)) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36",
};

/** Waits until no request has started or finished for `quietMs`. */
async function settle(ctx: BrowserContext, quietMs = 800, maxMs = 15_000) {
  let last = Date.now();
  const bump = () => {
    last = Date.now();
  };
  ctx.on("request", bump);
  ctx.on("requestfinished", bump);
  const start = Date.now();
  while (Date.now() - last < quietMs && Date.now() - start < maxMs)
    await new Promise((r) => setTimeout(r, 100));
  ctx.off("request", bump);
  ctx.off("requestfinished", bump);
}

async function measureJs(url: string, opts: { withRum: boolean }) {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ ...MOBILE, ignoreHTTPSErrors: LOCAL_TLS });
  if (opts.withRum) {
    // Worst case: every purpose granted (loads everything consent unlocks) + the RUM sample.
    const all = "analytics,ads,personalization,email_marketing,review_invites";
    await ctx.addCookies([{ name: "consent", value: encodeURIComponent(all), url: base.origin }]);
    await ctx.addInitScript(() => {
      Math.random = () => 0; // force the RUM sample
    });
  }
  const page = await ctx.newPage();
  const scripts = new Map<string, { gzip: number; transfer: number }>();
  const origins = new Set<string>();
  const csp: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy/i.test(m.text())) csp.push(m.text().slice(0, 200));
  });
  page.on("request", (r) => origins.add(new URL(r.url()).origin));
  page.on("response", async (res) => {
    if (res.request().resourceType() !== "script") return;
    try {
      const body = await res.body();
      const sizes = await res.request().sizes();
      scripts.set(res.url(), {
        gzip: gzipSync(body, { level: 9 }).byteLength,
        transfer: sizes.responseBodySize,
      });
    } catch {
      /* redirects / aborted */
    }
  });

  await page.goto(url, { waitUntil: "networkidle" });
  // An Authorization header makes the edge bypass its HTML cache (A2): a fresh render to count.
  const fresh = await ctx.request.get(url, { headers: { authorization: "Bearer measure" } });
  const subrequests = Number(fresh.headers()["x-edge-subrequests"] ?? Number.NaN);
  await settle(ctx);
  // A26: scroll the full page so every client:visible island is triggered.
  const height = await page.evaluate(() => document.documentElement.scrollHeight);
  for (let y = 0; y <= height; y += 400) {
    await page.evaluate((top) => window.scrollTo(0, top), y);
    await page.waitForTimeout(120);
  }
  // End at the very bottom (a fixed consent banner then covers only the page's end padding).
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await settle(ctx);

  const axe = opts.withRum
    ? []
    : (
        await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
          .analyze()
      ).violations.map((v) => ({
        id: v.id,
        impact: v.impact ?? "unknown",
        nodes: v.nodes.length,
      }));
  // Inline <script> bodies are part of the HTML transfer; count them too (Astro bootstrap).
  const inline = await page.evaluate(() =>
    [...document.querySelectorAll("script:not([src])")]
      .filter((s) => !s.getAttribute("type") || s.getAttribute("type") === "module")
      .map((s) => s.textContent ?? ""),
  );
  await browser.close();
  const inlineGzip = inline.reduce((n, s) => n + gzipSync(s, { level: 9 }).byteLength, 0);
  const files = [...scripts.entries()].map(([u, s]) => ({ url: new URL(u).pathname, ...s }));
  return {
    files,
    inlineGzip,
    gzip: files.reduce((n, f) => n + f.gzip, 0) + inlineGzip,
    transfer: files.reduce((n, f) => n + f.transfer, 0),
    thirdParty: [...origins].filter((o) => o !== base.origin && !o.startsWith("data:")),
    axe,
    subrequests,
    csp,
  };
}

async function measureLighthouse(url: string) {
  const port = 9300 + Math.floor(Math.random() * 500);
  const browser = await chromium.launch({
    args: [
      `--remote-debugging-port=${port}`,
      ...(LOCAL_TLS ? ["--ignore-certificate-errors"] : []),
    ],
  });
  try {
    const result = await lighthouse(url, {
      port,
      output: "json",
      logLevel: "error",
      onlyCategories: ["performance"],
    });
    const audits = result?.lhr.audits ?? {};
    if (values.explain) {
      // Where the LCP time goes and what competed for the (simulated) network before it.
      const detail = (k: string) =>
        JSON.stringify(audits[k]?.details ?? audits[k]?.displayValue ?? null).slice(0, 1200);
      const lcpNode = /"selector":"([^"]*)"/.exec(detail("lcp-breakdown-insight"))?.[1];
      console.log(
        `\n# ${url}\nFCP ${Math.round(audits["first-contentful-paint"]?.numericValue ?? 0)} ms, LCP ${Math.round(audits["largest-contentful-paint"]?.numericValue ?? 0)} ms, LCP element: ${lcpNode}`,
      );
      const items =
        (audits["network-requests"]?.details as { items?: Record<string, unknown>[] } | undefined)
          ?.items ?? [];
      for (const i of items) {
        console.log(
          `  ${String(i.resourceType).padEnd(10)} ${String(i.priority).padEnd(8)} ${String(Math.round(Number(i.transferSize) / 1024)).padStart(4)} kB  ${Math.round(Number(i.networkRequestTime))}→${Math.round(Number(i.networkEndTime))} ms  ${String(i.url).replace(/^https?:\/\/[^/]+/, "")}`,
        );
      }
    }
    return {
      lcpMs: audits["largest-contentful-paint"]?.numericValue ?? Number.NaN,
      tbtMs: audits["total-blocking-time"]?.numericValue ?? Number.NaN,
      cls: audits["cumulative-layout-shift"]?.numericValue ?? Number.NaN,
      score: result?.lhr.categories.performance?.score ?? null,
    };
  } finally {
    await browser.close();
  }
}

const results: (PageResult & {
  files: { url: string; gzip: number; transfer: number }[];
  lighthouseRuns: unknown[];
})[] = [];
for (const p of pages) {
  const url = new URL(p, base).href;
  const js = await measureJs(url, { withRum: false });
  const rum = await measureJs(url, { withRum: true });
  const lh = [];
  if (!values["skip-lighthouse"])
    for (let i = 0; i < runs; i++) lh.push(await measureLighthouse(url));
  results.push({
    path: p,
    kind: kindOf(p),
    lcpMs: median(lh.map((r) => r.lcpMs)),
    tbtMs: median(lh.map((r) => r.tbtMs)),
    cls: median(lh.map((r) => r.cls)),
    jsGzip: js.gzip,
    jsTransfer: js.transfer,
    jsGzipWithRum: rum.gzip,
    subrequests: js.subrequests,
    thirdPartyOrigins: js.thirdParty,
    axe: js.axe,
    cspViolations: [...js.csp, ...rum.csp],
    files: js.files,
    lighthouseRuns: lh,
  });
}

let failed = false;
console.log(
  `\nBudget (§9.6): LCP ≤ ${BUDGET.lcpMs} ms, TBT ≤ ${BUDGET.tbtMs} ms, CLS ≤ ${BUDGET.cls}, JS ≤ 30 kB gz (home 35)\n`,
);
console.log(
  "page".padEnd(22),
  "LCP ms".padStart(7),
  "TBT ms".padStart(7),
  "CLS".padStart(6),
  "JS gz kB".padStart(9),
  "+RUM kB".padStart(8),
  "xfer kB".padStart(8),
  "calls".padStart(6),
  " result",
);
for (const r of results) {
  const fails = judge(r);
  failed ||= fails.length > 0;
  console.log(
    r.path.padEnd(22),
    String(Math.round(r.lcpMs)).padStart(7),
    String(Math.round(r.tbtMs)).padStart(7),
    r.cls.toFixed(3).padStart(6),
    (r.jsGzip / 1024).toFixed(1).padStart(9),
    (r.jsGzipWithRum / 1024).toFixed(1).padStart(8),
    (r.jsTransfer / 1024).toFixed(1).padStart(8),
    String(r.subrequests).padStart(6),
    fails.length ? ` FAIL: ${fails.join("; ")}` : " ok",
  );
}
await mkdir(path.dirname(values.out), { recursive: true });
await writeFile(
  values.out,
  `${JSON.stringify({ base: base.href, budget: BUDGET, results }, null, 2)}\n`,
);
console.log(`\nreport: ${values.out}`);
if (failed && !values["no-fail"]) process.exit(1);
