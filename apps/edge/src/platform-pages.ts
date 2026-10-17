/**
 * Platform pages on a same-origin shop (C1, `docs/decisions/same-origin-checkout.md`): only a
 * top-level document navigation reaches one, decided from Fetch Metadata alone.
 */
import { platformSegment } from "@platform/theme-kit";

export type Admission = "navigation" | "refused" | "unsupported";

/** A reserved page path (`/checkout`, `/o/…`, `/withdraw`, …) under any spelling the decoder knows. */
export function isPlatformPagePath(pathname: string): boolean {
  const segment = platformSegment(pathname);
  return segment !== null && !segment.startsWith("_");
}

/**
 * Prefetch and prerender would consume one-shot pages, and a browser without Fetch Metadata
 * cannot be told apart from a script fetch, so both fail closed. `Sec-Fetch-Site` is not checked:
 * email and payment-provider arrivals are cross-site navigations.
 */
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
