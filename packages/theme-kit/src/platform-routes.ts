import routes from "./platform-routes.json" with { type: "json" };

export const PLATFORM_SEGMENTS: readonly string[] = routes.segments;

const RESERVED = new Set(routes.segments);
const LOCALE = /^[a-z]{2}(-[a-z]{2})?$/;

/**
 * True when `pathname`, however it is encoded, cased or slashed, names a reserved segment or
 * its subtree, directly or behind a locale prefix (`/cs/checkout`).
 */
export function isPlatformPath(pathname: string): boolean {
  const segments: string[] = [];
  for (const s of decodeFully(pathname).toLowerCase().split(/[/\\]/)) {
    if (s === "" || s === ".") continue;
    if (s === "..") segments.pop();
    else segments.push(s);
  }
  const [first = "", second = ""] = segments;
  return RESERVED.has(first) || (LOCALE.test(first) && RESERVED.has(second));
}

const hex = (c: string | undefined) => (c && /^[0-9a-f]$/i.test(c) ? parseInt(c, 16) : -1);

function decodeFully(p: string): string {
  const out: string[] = [];
  for (const c of p) {
    out.push(c);
    for (let n = out.length; n >= 3 && out[n - 3] === "%"; n = out.length) {
      const hi = hex(out[n - 2]);
      const lo = hex(out[n - 1]);
      if (hi < 0 || lo < 0) break;
      out.length = n - 3;
      out.push(String.fromCharCode(hi * 16 + lo));
    }
  }
  return out.join("");
}
