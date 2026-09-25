/**
 * Browser-side calls to the checkout origin's platform routes (`/_p/account/*`, `/_p/consent`).
 * Same-origin JSON only: the edge refuses anything else (CSRF) and keeps the session in an
 * HttpOnly cookie, so no credential ever passes through this code.
 */
export interface Result<T> {
  ok: boolean;
  status: number;
  data: T | null;
  /** The problem+json `code` (`invalid_credentials`, ...), `network` when offline. */
  code: string | null;
}

export async function call<T = unknown>(
  method: "GET" | "POST" | "PUT" | "DELETE",
  path: string,
  body?: unknown,
): Promise<Result<T>> {
  try {
    const res = await fetch(path, {
      method,
      credentials: "same-origin",
      headers: body === undefined ? {} : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let data: unknown = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = null;
    }
    const code =
      !res.ok && typeof data === "object" && data !== null && "code" in data
        ? String((data as { code: unknown }).code)
        : null;
    return { ok: res.ok, status: res.status, data: res.ok ? (data as T) : null, code };
  } catch {
    return { ok: false, status: 0, data: null, code: "network" };
  }
}
