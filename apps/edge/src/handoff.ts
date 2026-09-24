import { createHash, randomBytes } from "node:crypto";

/**
 * Checkout handoff tokens (spec A1): single-use, 60 s, stored only as a SHA-256 hash.
 * ponytail: in-memory, so one edge process and lost on restart; WP6 moves it to the API
 * (`POST /internal/v1/handoffs`) when the edge scales out.
 */
export interface Handoff {
  checkoutHost: string;
  tenantId: string;
  /** Checkout-scoped cart capability minted by the API for this handoff. */
  cartToken: string;
}

const TTL_MS = 60_000;
const MAX_PENDING = 100_000;

const hash = (token: string) => createHash("sha256").update(token).digest("hex");

export class HandoffStore {
  readonly #pending = new Map<string, Handoff & { expiresAt: number }>();

  mint(h: Handoff, now = Date.now()): string {
    this.sweep(now);
    if (this.#pending.size >= MAX_PENDING) throw new Error("handoff store full");
    const token = randomBytes(32).toString("base64url");
    this.#pending.set(hash(token), { ...h, expiresAt: now + TTL_MS });
    return token;
  }

  /** Consumes the token atomically (delete-on-read). `null` if unknown, used, expired or for another host. */
  consume(token: string, checkoutHost: string, now = Date.now()): Handoff | null {
    if (!/^[A-Za-z0-9_-]{43}$/.test(token)) return null;
    const key = hash(token);
    const h = this.#pending.get(key);
    this.#pending.delete(key);
    if (!h || h.expiresAt <= now || h.checkoutHost !== checkoutHost) return null;
    return { checkoutHost: h.checkoutHost, tenantId: h.tenantId, cartToken: h.cartToken };
  }

  sweep(now = Date.now()) {
    for (const [k, h] of this.#pending) if (h.expiresAt <= now) this.#pending.delete(k);
  }
}
