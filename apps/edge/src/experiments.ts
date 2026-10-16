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

export class ApiEnroller implements Enroller {
  readonly #apiOrigin: string;
  readonly #token: string;
  readonly #upstream: (r: Request) => Promise<Response>;
  readonly #memo = new Map<
    string,
    { experiment: string; assignment: Assignment | null; at: number }
  >();
  #forgets = 0;
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
    const forgets = this.#forgets;
    const res = await this.#upstream(
      new Request(`${this.#apiOrigin}/internal/v1/experiments/enrol`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${this.#token}`,
          accept: "application/json",
          "content-type": "application/json",
        },
        body: JSON.stringify({ tenant_id: site.tenant_id, subject }),
        signal: AbortSignal.timeout(500),
      }),
    );
    if (res.status !== 200) throw new Error(`enrol: ${res.status}`);
    const assignment = parseAssignment(((await res.json()) as { assignment?: unknown }).assignment);
    if (forgets !== this.#forgets) return assignment;
    this.#memo.delete(key);
    if (this.#memo.size >= MAX_MEMO) this.#memo.delete(this.#memo.keys().next().value ?? "");
    this.#memo.set(key, { experiment, assignment, at: now });
    return assignment;
  }

  forget(tenantId: string, subject: string) {
    this.#forgets++;
    this.#memo.delete(`${tenantId}|${subject}`);
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
