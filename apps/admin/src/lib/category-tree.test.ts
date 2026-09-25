import { describe, expect, it } from "vitest";
import {
  type CategoryNode,
  categoryName,
  flatten,
  indent,
  moveDown,
  moveUp,
  outdent,
  subtreeIds,
} from "./category-tree.ts";

const node = (id: string, children: CategoryNode[] = []): CategoryNode => ({
  id,
  position: 0,
  translations: [{ locale: "cs", name: id.toUpperCase(), slug: id }],
  created_at: "",
  updated_at: "",
  children,
});

// a
// ├─ a1
// └─ a2
// b
const tree = [node("a", [node("a1"), node("a2")]), node("b")];
const rows = flatten(tree);
const row = (id: string) => {
  const r = rows.find((x) => x.node.id === id);
  if (!r) throw new Error(id);
  return r;
};

describe("category tree operations", () => {
  it("flattens depth-first with depth and parent", () => {
    expect(rows.map((r) => `${r.node.id}:${r.depth}:${r.parentId}`)).toEqual([
      "a:0:null",
      "a1:1:a",
      "a2:1:a",
      "b:0:null",
    ]);
  });

  it("moves among siblings", () => {
    expect(moveUp(row("a"))).toBeNull();
    expect(moveUp(row("a2"))).toEqual({ parent_id: "a", position: 0 });
    expect(moveDown(row("a1"))).toEqual({ parent_id: "a", position: 1 });
    expect(moveDown(row("b"))).toBeNull();
  });

  it("indents under the previous sibling and outdents after the parent", () => {
    expect(indent(rows, row("a"))).toBeNull();
    expect(indent(rows, row("b"))).toEqual({ parent_id: "a", position: 2 });
    expect(indent(rows, row("a2"))).toEqual({ parent_id: "a1", position: 0 });
    expect(outdent(rows, row("a"))).toBeNull();
    expect(outdent(rows, row("a1"))).toEqual({ parent_id: null, position: 1 });
  });

  it("collects a subtree and picks names by locale preference", () => {
    expect([...subtreeIds(row("a").node)]).toEqual(["a", "a1", "a2"]);
    expect(categoryName(row("b").node, ["en", "cs"])).toBe("B");
  });
});
