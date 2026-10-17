import { platformSegment } from "@platform/theme-kit";

export type Admission = "navigation" | "refused" | "unsupported";

const isNamespace = (segment: string) => segment.startsWith("_");

export function isPlatformPagePath(pathname: string): boolean {
  const segment = platformSegment(pathname);
  return segment !== null && !isNamespace(segment);
}

export const isSameOriginCheckoutPath = (pathname: string) =>
  platformSegment(pathname) === "checkout";

export function admitNavigation(headers: Headers): Admission {
  if (headers.has("sec-purpose")) return "refused";
  const mode = headers.get("sec-fetch-mode");
  if (mode === null) return "unsupported";
  return mode === "navigate" && headers.get("sec-fetch-dest") === "document"
    ? "navigation"
    : "refused";
}

export const UNSUPPORTED_BROWSER_PAGE = `<!doctype html><html lang="cs"><meta charset="utf-8"><meta name="robots" content="noindex">
<title>Nepodporovaný prohlížeč</title>
<main><h1>Nepodporovaný prohlížeč</h1>
<p>Tento prohlížeč je na pokladnu příliš starý. Otevřete ji prosím v aktuální verzi prohlížeče Chrome, Firefox, Safari nebo Edge.</p>
<p lang="en">This browser is too old for checkout. Please open it in a current version of Chrome, Firefox, Safari or Edge.</p></main></html>`;
