import routes from "./platform-routes.json" with { type: "json" };

export const PLATFORM_SEGMENTS: readonly string[] = routes.segments;

const RESERVED = new Set(routes.segments);
const LOCALE = /^[a-z]{2}(-[a-z]{2})?$/;

/**
 * The reserved segment `pathname` names, however it is encoded, cased or slashed, directly or
 * behind a locale prefix (`/cs/checkout` → `checkout`); `null` for any other path.
 */
export function platformSegment(pathname: string): string | null {
  const segments: string[] = [];
  for (const s of decodeFully(pathname).toLowerCase().split(/[/\\]/)) {
    if (s === "" || s === ".") continue;
    if (s === "..") segments.pop();
    else segments.push(s);
  }
  const [first = "", second = ""] = segments;
  if (RESERVED.has(first)) return first;
  return LOCALE.test(first) && RESERVED.has(second) ? second : null;
}

/** True when `pathname` names a reserved segment or its subtree (see `platformSegment`). */
export function isPlatformPath(pathname: string): boolean {
  return platformSegment(pathname) !== null;
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
