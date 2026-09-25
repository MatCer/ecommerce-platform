/**
 * Category tree helpers for the accessible reorder UI (move up/down, indent, outdent, parent
 * select). Each operation yields the body of `POST /categories/{id}/move`, or `null` when it
 * is not possible from the current position.
 */
import type { components } from "@platform/admin-client";

export type CategoryNode = components["schemas"]["CategoryNode"];
export type CategoryMove = components["schemas"]["CategoryMove"];

export interface Row {
  node: CategoryNode;
  depth: number;
  parentId: string | null;
  index: number;
  siblings: number;
}

/** Depth-first rows in display order. */
export function flatten(
  nodes: readonly CategoryNode[],
  depth = 0,
  parentId: string | null = null,
): Row[] {
  return nodes.flatMap((node, index) => [
    { node, depth, parentId, index, siblings: nodes.length },
    ...flatten(node.children, depth + 1, node.id),
  ]);
}

function siblingsOf(rows: readonly Row[], parentId: string | null): Row[] {
  return rows.filter((r) => r.parentId === parentId);
}

export function moveUp(row: Row): CategoryMove | null {
  return row.index > 0 ? { parent_id: row.parentId, position: row.index - 1 } : null;
}

export function moveDown(row: Row): CategoryMove | null {
  return row.index < row.siblings - 1 ? { parent_id: row.parentId, position: row.index + 1 } : null;
}

/** Becomes the last child of the previous sibling. */
export function indent(rows: readonly Row[], row: Row): CategoryMove | null {
  const prev = siblingsOf(rows, row.parentId)[row.index - 1];
  return prev ? { parent_id: prev.node.id, position: prev.node.children.length } : null;
}

/** Becomes the next sibling of its parent. */
export function outdent(rows: readonly Row[], row: Row): CategoryMove | null {
  if (row.parentId === null) return null;
  const parent = rows.find((r) => r.node.id === row.parentId);
  return parent ? { parent_id: parent.parentId, position: parent.index + 1 } : null;
}

/** The ids of a node and its whole subtree (not valid as its new parent). */
export function subtreeIds(node: CategoryNode): Set<string> {
  const ids = new Set<string>([node.id]);
  for (const c of node.children) for (const id of subtreeIds(c)) ids.add(id);
  return ids;
}

/** A display name: the first translation in the preferred locales, else any. */
export function categoryName(
  node: { translations: readonly { locale: string; name: string }[] },
  locales: readonly string[],
): string {
  for (const l of locales) {
    const t = node.translations.find((x) => x.locale === l);
    if (t) return t.name;
  }
  return node.translations[0]?.name ?? "—";
}
