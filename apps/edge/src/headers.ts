/**
 * Header hygiene and security headers (spec §9.3.6, A2, A26).
 */

/** Routing/identity headers a client must never be able to supply (spec A2). */
const UNTRUSTED =
  /^(x-tenant|x-market|x-locale|x-storefront-.*|x-forwarded-.*|forwarded|x-real-ip|x-platform-.*|x-cart-token|cf-connecting-ip|true-client-ip)$/i;

export function stripUntrusted(headers: Headers): Headers {
  const out = new Headers();
  for (const [k, v] of headers) if (!UNTRUSTED.test(k)) out.append(k, v);
  return out;
}

/** Request headers a theme worker may see. Everything else (cookies, auth, ...) is dropped. */
const THEME_REQUEST_ALLOW = new Set([
  "accept",
  "accept-language",
  "user-agent",
  "sec-purpose",
  "if-none-match",
]);

export function themeRequestHeaders(incoming: Headers): Headers {
  const out = new Headers();
  for (const [k, v] of incoming) if (THEME_REQUEST_ALLOW.has(k)) out.set(k, v);
  return out;
}

// Hop-by-hop headers, Miniflare internals and anything that would let a worker set state.
const RESPONSE_DROP =
  /^(connection|keep-alive|transfer-encoding|upgrade|te|trailer|proxy-.*|mf-.*|set-cookie|content-security-policy.*|x-powered-by|content-length|content-encoding)$/i;

/** Copies worker response headers the edge is willing to forward. */
export function workerResponseHeaders(from: Headers): Headers {
  const out = new Headers();
  for (const [k, v] of from) if (!RESPONSE_DROP.test(k)) out.append(k, v);
  return out;
}

export type CspProfile = "theme" | "checkout";

/**
 * Edge-owned CSP (spec §12.3 safety, A26). `scriptHashes` are the hashes of the unavoidable
 * inline scripts (Astro's island bootstrap), computed from the pinned Astro version at pack time.
 */
export function contentSecurityPolicy(
  profile: CspProfile,
  opts: { scriptHashes: string[]; styleHashes: string[]; checkoutOrigin?: string },
): string {
  const hashes = (h: string[]) => h.map((x) => ` '${x}'`).join("");
  const common = [
    "default-src 'self'",
    `script-src 'self'${hashes(opts.scriptHashes)}`,
    `style-src 'self'${hashes(opts.styleHashes)}`,
    // Attribute styles cannot fetch anything beyond img-src; needed for component style props.
    "style-src-attr 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self'",
    "object-src 'none'",
    "base-uri 'self'",
    "frame-ancestors 'none'",
  ];
  if (profile === "theme") {
    return [
      ...common,
      "connect-src 'self'",
      "frame-src 'none'",
      // The checkout handoff is a same-origin POST that redirects to the checkout origin.
      `form-action 'self'${opts.checkoutOrigin ? ` ${opts.checkoutOrigin}` : ""}`,
    ].join("; ");
  }
  return [
    ...common.map((d) => (d.startsWith("script-src") ? `${d} https://js.stripe.com` : d)),
    "connect-src 'self' https://api.stripe.com",
    // Pickup-point widget and Stripe Payment Element (spec §9.4), loaded on interaction only.
    "frame-src https://js.stripe.com https://hooks.stripe.com https://widget.packeta.com",
    "form-action 'self'",
  ].join("; ");
}

export function securityHeaders(profile: CspProfile, csp: string): Record<string, string> {
  return {
    "content-security-policy": csp,
    "referrer-policy": "strict-origin-when-cross-origin",
    "x-content-type-options": "nosniff",
    "cross-origin-opener-policy": "same-origin",
    "permissions-policy":
      profile === "checkout"
        ? 'camera=(), microphone=(), geolocation=(), usb=(), payment=(self "https://js.stripe.com")'
        : "camera=(), microphone=(), geolocation=(), usb=(), payment=()",
  };
}
