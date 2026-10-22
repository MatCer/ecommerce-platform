import type { Schemas } from "./api.ts";

export type FeedSource = Exclude<Schemas["Source"], "store">;

export function shopUrl(raw: string): string {
  const v = raw.trim();
  return v.includes("://") ? v : `https://${v}`;
}

export function feedImporterHref(feed: Schemas["FeedHint"]): string {
  return `/imports?${new URLSearchParams({ source: feed.source, feed: feed.url })}`;
}

export function feedPrefill(search: string): { source: FeedSource; url: string } | undefined {
  const params = new URLSearchParams(search);
  const url = params.get("feed");
  if (!url) return undefined;
  return { source: params.get("source") === "google" ? "google" : "heureka", url };
}

export type BrandAction = "apply" | "in_use";

export function nameAction(
  suggested: string | null | undefined,
  current: string | undefined,
): BrandAction | undefined {
  if (!suggested) return undefined;
  return suggested === current ? "in_use" : "apply";
}

export function logoAction(
  asset: Pick<Schemas["Asset"], "id" | "status">,
  emailLogo: string | null | undefined,
): BrandAction | "processing" | "failed" {
  if (asset.id === emailLogo) return "in_use";
  if (asset.status === "ready") return "apply";
  return asset.status === "failed" ? "failed" : "processing";
}
