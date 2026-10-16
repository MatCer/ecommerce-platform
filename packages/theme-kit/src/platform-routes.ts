import routes from "./platform-routes.json" with { type: "json" };

export const PLATFORM_SEGMENTS: readonly string[] = routes.segments;

const RESERVED = new Set(routes.segments);
const LOCALE = /^[a-z]{2}(-[a-z]{2})?$/;
const ESCAPE = /%([0-9a-f]{2})/gi;
const MAX_DECODES = 4;

/**
 * True when `pathname`, however it is encoded, cased or slashed, names a reserved segment or
 * its subtree, directly or behind a locale prefix (`/cs/checkout`).
 */
export function isPlatformPath(pathname: string): boolean {
  let p = pathname;
  for (let i = 0; i < MAX_DECODES; i++)
    p = p.replace(ESCAPE, (_, hex: string) => String.fromCharCode(parseInt(hex, 16)));
  if (p.search(ESCAPE) !== -1) return true;
  const segments: string[] = [];
  for (const s of p.toLowerCase().split(/[/\\]/)) {
    if (s === "" || s === ".") continue;
    if (s === "..") segments.pop();
    else segments.push(s);
  }
  const [first = "", second = ""] = segments;
  return RESERVED.has(first) || (LOCALE.test(first) && RESERVED.has(second));
}
