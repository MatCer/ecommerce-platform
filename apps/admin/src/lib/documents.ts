import type { Locale } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "./api.ts";

/** Queues a packing slip or label sheet PDF (label sheets create missing labels first). */
export async function requestDocument(
  body: { kind: "labels" | "packing_slips"; order_ids: string[]; locale: Locale },
  signal: AbortSignal,
) {
  const res = await unwrap(
    api.POST("/admin/v1/documents", { params: { header: tenantHeader() }, body, signal }),
  );
  return { id: res.document.id, failures: res.failures };
}
