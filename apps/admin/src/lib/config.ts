/**
 * Where the SPA finds its services. Derived from the admin host at runtime so one static build
 * works on any port (`admin.localhost:8080` -> `api.localhost:8080`); `VITE_*` overrides them.
 */
export function siblingOrigin(origin: string, service: string): string {
  const url = new URL(origin);
  url.hostname = url.hostname.replace(/^admin\./, `${service}.`);
  return url.origin;
}

const here = globalThis.location?.origin ?? "http://admin.localhost";

export const API_ORIGIN: string = import.meta.env.VITE_API_ORIGIN || siblingOrigin(here, "api");
/** Better Auth is served on the admin origin (first-party session cookie). */
export const AUTH_BASE = "/api/auth";
/** Public-bucket objects (image variants). */
export const MEDIA_BASE: string =
  import.meta.env.VITE_MEDIA_BASE || `${siblingOrigin(here, "s3")}/public/`;

export function mediaUrl(key: string): string {
  return MEDIA_BASE + key.split("/").map(encodeURIComponent).join("/");
}
