import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

/**
 * Theme contract lint (spec §9.1, §12.3 gates). Static checks only; the runtime boundary (A7)
 * is what actually enforces "no foreign fetch" — this catches mistakes early and tells an AI
 * editor why its change was rejected.
 */
export const REQUIRED_ROUTES = [
  "src/pages/index.astro",
  "src/pages/c/[...slug].astro",
  "src/pages/p/[slug].astro",
  "src/pages/search.astro",
  "src/pages/pages/[slug].astro",
  "src/pages/blog/index.astro",
  "src/pages/blog/[slug].astro",
  "src/pages/404.astro",
];

const RULES: { id: string; re: RegExp; message: string }[] = [
  {
    id: "foreign-fetch",
    re: /\bfetch\(\s*[`'"](https?:)?\/\//,
    message: "fetch to another origin; use @platform/storefront-sdk",
  },
  {
    id: "remote-import",
    re: /\bimport\s*(?:[^'"]*from\s*)?\(?\s*[`'"]https?:/,
    message: "import from a URL",
  },
  {
    id: "websocket",
    re: /\bnew\s+(WebSocket|EventSource)\b/,
    message: "WebSocket/EventSource are not allowed",
  },
  {
    id: "eval",
    re: /\b(eval\s*\(|new\s+Function\s*\()/,
    message: "eval/new Function (blocked by CSP)",
  },
  {
    id: "inline-script",
    re: /<script\b[^>]*\bis:inline\b/,
    message: "is:inline scripts are blocked by the CSP",
  },
  {
    id: "inline-handler",
    re: /<[a-z][^>]*\son[a-z]+\s*=\s*["']/,
    message: "inline event handler attribute (blocked by the CSP)",
  },
  {
    id: "cloudflare-env",
    re: /from\s+["']cloudflare:(?!workers["'])/,
    message: "only cloudflare:workers (env.STOREFRONT) is available",
  },
  {
    id: "cookie-access",
    re: /document\.cookie/,
    message: "themes must not read or write cookies (the SDK owns consent)",
  },
];

// set:html is allowed only for platform-sanitized CMS/product HTML and the JSON-LD helper.
const SET_HTML = /set:html=\{([^}]*)\}/g;
const SET_HTML_OK = /^\s*(jsonLd\(|[\w.]*description_html\s*$|[\w.]*content_html\s*$)/;

export interface Violation {
  file: string;
  line: number;
  rule: string;
  message: string;
}

async function walk(dir: string, rel = ""): Promise<string[]> {
  const out: string[] = [];
  for (const e of await readdir(path.join(dir, rel), { withFileTypes: true }).catch(() => [])) {
    const r = rel ? `${rel}/${e.name}` : e.name;
    if (e.isDirectory()) out.push(...(await walk(dir, r)));
    else if (/\.(astro|ts|tsx|js|jsx|mjs)$/.test(e.name)) out.push(r);
  }
  return out;
}

export async function lintTheme(
  themeDir: string,
  opts: { referencePackageJson?: string } = {},
): Promise<Violation[]> {
  const v: Violation[] = [];
  const files = new Set(await walk(themeDir, "src").then((fs) => fs));
  for (const route of REQUIRED_ROUTES) {
    if (!files.has(route))
      v.push({
        file: route,
        line: 0,
        rule: "required-route",
        message: "required route is missing",
      });
  }
  for (const f of files) {
    const lines = (await readFile(path.join(themeDir, f), "utf8")).split("\n");
    lines.forEach((text, i) => {
      for (const r of RULES)
        if (r.re.test(text)) v.push({ file: f, line: i + 1, rule: r.id, message: r.message });
      for (const m of text.matchAll(SET_HTML)) {
        if (!SET_HTML_OK.test(m[1] ?? "")) {
          v.push({
            file: f,
            line: i + 1,
            rule: "set-html",
            message: "set:html only for description_html/content_html or jsonLd()",
          });
        }
      }
    });
  }
  if (opts.referencePackageJson) {
    const deps = (p: string) =>
      readFile(p, "utf8").then((s) => {
        const j = JSON.parse(s) as { dependencies?: object; devDependencies?: object };
        return JSON.stringify([j.dependencies ?? {}, j.devDependencies ?? {}]);
      });
    if (
      (await deps(path.join(themeDir, "package.json"))) !== (await deps(opts.referencePackageJson))
    ) {
      v.push({
        file: "package.json",
        line: 0,
        rule: "locked-deps",
        message: "dependencies are platform-owned and cannot change",
      });
    }
  }
  return v;
}
