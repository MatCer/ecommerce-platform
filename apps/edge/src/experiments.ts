import type { Site } from "./sites.ts";

export interface Assignment {
  experiment_id: string;
  version: number;
  arm: string;
}

export interface Enroller {
  enrol(site: Site, subject: string): Promise<Assignment | null>;
  forget(tenantId: string, subject: string): void;
}

const ARM_RE = /^[a-z0-9_-]{1,32}$/;
const MEMO_MS = 60_000;
const MAX_MEMO = 10_000;
const DEADLINE_MS = 150;
const BACKOFF_MS = 10_000;

export class ApiEnroller implements Enroller {
  readonly #apiOrigin: string;
  readonly #token: string;
  readonly #upstream: (r: Request) => Promise<Response>;
  readonly #memo = new Map<
    string,
    { experiment: string; assignment: Assignment | null; at: number }
  >();
  readonly #inflight = new Map<
    string,
    { experiment: string; answer: Promise<Assignment | null> }
  >();
  #forgets = 0;
  #downUntil = 0;
  #down = false;
  #probing = false;
  constructor(apiOrigin: string, token: string, upstream?: (r: Request) => Promise<Response>) {
    this.#apiOrigin = apiOrigin;
    this.#token = token;
    this.#upstream = upstream ?? ((r) => fetch(r));
  }

  async enrol(site: Site, subject: string, now = Date.now()): Promise<Assignment | null> {
    if (!site.experiment) return null;
    const key = `${site.tenant_id}|${subject}`;
    const experiment = `${site.experiment.id}:${site.experiment.version}`;
    const hit = this.#memo.get(key);
    if (hit && hit.experiment === experiment && now - hit.at < MEMO_MS) return hit.assignment;
    const flying = this.#inflight.get(key);
    if (flying?.experiment === experiment) return flying.answer;
    if (now < this.#downUntil || this.#probing) return null;
    const answer = this.#ask(site, subject, key, experiment, now).finally(() => {
      if (this.#inflight.get(key)?.answer === answer) this.#inflight.delete(key);
    });
    this.#inflight.set(key, { experiment, answer });
    return answer;
  }

  async #ask(
    site: Site,
    subject: string,
    key: string,
    experiment: string,
    now: number,
  ): Promise<Assignment | null> {
    const forgets = this.#forgets;
    const probe = this.#down;
    if (probe) this.#probing = true;
    let assignment: Assignment | null;
    try {
      assignment = await this.#call(site, subject);
      this.#down = false;
    } catch (err) {
      this.#down = !(err instanceof RateLimited);
      if (this.#down) this.#downUntil = now + BACKOFF_MS;
      throw err;
    } finally {
      if (probe) this.#probing = false;
    }
    if (forgets !== this.#forgets) return assignment;
    this.#memo.delete(key);
    if (this.#memo.size >= MAX_MEMO) this.#memo.delete(this.#memo.keys().next().value ?? "");
    this.#memo.set(key, { experiment, assignment, at: now });
    return assignment;
  }

  async #call(site: Site, subject: string): Promise<Assignment | null> {
    const res = await this.#upstream(
      new Request(`${this.#apiOrigin}/internal/v1/experiments/enrol`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${this.#token}`,
          accept: "application/json",
          "content-type": "application/json",
          ...(site.clientIp ? { "x-client-ip": site.clientIp } : {}),
        },
        body: JSON.stringify({ tenant_id: site.tenant_id, subject }),
        signal: AbortSignal.timeout(DEADLINE_MS),
      }),
    );
    if (res.status === 429) throw new RateLimited();
    if (res.status !== 200) throw new Error(`enrol: ${res.status}`);
    return parseAssignment(((await res.json()) as { assignment?: unknown }).assignment);
  }

  forget(tenantId: string, subject: string) {
    this.#forgets++;
    this.#memo.delete(`${tenantId}|${subject}`);
    this.#inflight.delete(`${tenantId}|${subject}`);
  }
}

class RateLimited extends Error {
  constructor() {
    super("enrol: 429");
  }
}

function parseAssignment(v: unknown): Assignment | null {
  if (typeof v !== "object" || v === null) return null;
  const { experiment_id, version, arm } = v as Record<string, unknown>;
  return typeof experiment_id === "string" &&
    Number.isInteger(version) &&
    typeof arm === "string" &&
    ARM_RE.test(arm)
    ? { experiment_id, version: version as number, arm }
    : null;
}
