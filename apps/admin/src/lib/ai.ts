/** AI helpers (WP22): proposal polling, value rendering and labels shared by the AI screens. */
import { createQuery } from "@tanstack/solid-query";
import { api, type Schemas, tenantHeader, unwrap } from "./api.ts";
import { tenantKey } from "./me.ts";

export type Proposal = Schemas["Proposal"];
export type ProposalKind = Schemas["ProposalKind"];
export type EntityType = Schemas["EntityType"];
export type Change = Schemas["Change"];
export type BulkPlan = Schemas["BulkPlan"];
export type Operation = Schemas["Operation"];

/** Polling interval while a job runs. */
const POLL_MS = 1000;

/** The shop's AI usage and provider (`fake` = demo fixtures). */
export function useAiUsage() {
  return createQuery(() => ({
    queryKey: tenantKey("ai-usage"),
    queryFn: () => unwrap(api.GET("/admin/v1/ai/usage", { params: { header: tenantHeader() } })),
  }));
}

/** A proposal, polled until it is no longer `pending`. */
export function useProposal(id: () => string | null) {
  return createQuery(() => ({
    queryKey: tenantKey("ai-proposal", id()),
    enabled: id() !== null,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/ai/proposals/{id}", {
          params: { header: tenantHeader(), path: { id: id() ?? "" } },
        }),
      ),
    refetchInterval: (q: { state: { data?: Proposal } }) =>
      q.state.data?.status === "pending" ? POLL_MS : false,
  }));
}

/** A bulk plan, polled while the model plans or the job applies it. */
export function useBulkPlan(id: () => string | null) {
  return createQuery(() => ({
    queryKey: tenantKey("ai-plan", id()),
    enabled: id() !== null,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/ai/bulk-plans/{id}", {
          params: { header: tenantHeader(), path: { id: id() ?? "" } },
        }),
      ),
    refetchInterval: (q: { state: { data?: BulkPlan } }) =>
      q.state.data?.status === "pending" || q.state.data?.status === "applying" ? POLL_MS : false,
  }));
}

/** Fields of an entity whose current text was written by AI. */
export function useAiMarks(entityType: () => EntityType, entityId: () => string | undefined) {
  return createQuery(() => ({
    queryKey: tenantKey("ai-marks", entityType(), entityId()),
    enabled: !!entityId(),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/ai/marks", {
          params: {
            header: tenantHeader(),
            query: { entity_type: entityType(), entity_id: entityId() ?? "" },
          },
        }),
      ),
  }));
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

const TEXT_KEYS = ["text", "html", "alt", "caption", "label", "title"] as const;

/** The texts of a page's blocks (translation previews). */
function blockTexts(blocks: unknown[]): string[] {
  const out: string[] = [];
  for (const b of blocks) {
    if (!isRecord(b)) continue;
    for (const k of TEXT_KEYS) {
      const v = b[k];
      if (typeof v === "string" && v.trim()) out.push(v);
    }
    const items = b.items;
    if (Array.isArray(items)) {
      for (const item of items) {
        if (!isRecord(item)) continue;
        for (const k of ["question", "answer_html"]) {
          const v = item[k];
          if (typeof v === "string" && v.trim()) out.push(v);
        }
      }
    }
  }
  return out;
}

/** A proposed value as displayable text (HTML stays HTML; lists become paragraphs). */
export function displayValue(field: string, value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    if (field === "labels") {
      return value
        .map((l) =>
          isRecord(l) && typeof l.label === "string" ? `<p>${escapeHtml(l.label)}</p>` : "",
        )
        .join("");
    }
    return blockTexts(value)
      .map((s) => (s.trimStart().startsWith("<") ? s : `<p>${escapeHtml(s)}</p>`))
      .join("");
  }
  return escapeHtml(JSON.stringify(value));
}

/** Whether a field's value renders as HTML in previews. */
export function isHtml(field: string): boolean {
  return field.endsWith("html") || field === "blocks" || field === "labels";
}

export function escapeHtml(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

/** Which helpers apply to an entity type. */
export function kindsFor(entity: EntityType): ProposalKind[] {
  switch (entity) {
    case "product":
      return ["product_description", "seo", "translate"];
    case "category":
      return ["category_description", "seo", "translate"];
    case "page":
      return ["seo", "translate"];
    case "menu":
      return ["translate"];
  }
}

/** Minor units as "12.90". */
export function minorToMajor(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  return `${sign}${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, "0")}`;
}
