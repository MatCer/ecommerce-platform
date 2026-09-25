/** Admin API error: RFC 9457 problem with the API's stable `code`. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly detail: string | undefined;

  constructor(status: number, code: string, detail?: string) {
    super(detail ?? code);
    this.status = status;
    this.code = code;
    this.detail = detail;
  }
}

export async function problemOf(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let detail: string | undefined;
  try {
    const body: unknown = await res.clone().json();
    if (typeof body === "object" && body !== null) {
      const c = Reflect.get(body, "code");
      const d = Reflect.get(body, "detail");
      if (typeof c === "string") code = c;
      if (typeof d === "string") detail = d;
    }
  } catch {
    // Not JSON (a proxy error page): keep the status-derived code.
  }
  return new ApiError(res.status, code, detail);
}

export interface AuthDeps {
  /** A currently valid JWT. */
  token: () => Promise<string>;
  /** Forces a new JWT. */
  refresh: () => Promise<unknown>;
  /** Asks the user to sign in again; `false` when they cancel. */
  reauth: () => Promise<boolean>;
  tenant: () => string | null;
  fetch: (req: Request) => Promise<Response>;
}

/**
 * `fetch` for the Admin API client: adds the bearer token and `X-Tenant-Id`, retries once
 * with a fresh token on `401 invalid_token` and once after re-authentication on
 * `401 reauth_required` (the request keeps its `Idempotency-Key`, so a retry is safe).
 */
export function createAuthedFetch(deps: AuthDeps) {
  async function send(req: Request): Promise<Response> {
    const headers = new Headers(req.headers);
    headers.set("authorization", `Bearer ${await deps.token()}`);
    const tenant = deps.tenant();
    if (tenant && !headers.has("x-tenant-id")) headers.set("x-tenant-id", tenant);
    return deps.fetch(new Request(req, { headers }));
  }

  return async (req: Request): Promise<Response> => {
    const retry = req.clone();
    const res = await send(req);
    if (res.status !== 401) return res;
    const { code } = await problemOf(res);
    if (code === "invalid_token") {
      await deps.refresh();
      return send(retry);
    }
    if (code === "reauth_required" && (await deps.reauth())) {
      return send(retry);
    }
    return res;
  };
}
