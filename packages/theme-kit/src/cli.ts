#!/usr/bin/env node
import { parseArgs } from "node:util";
import { type ArtifactKind, packArtifact } from "./artifact.ts";

const [command, ...rest] = process.argv.slice(2);

if (command === "pack") {
  const { values } = parseArgs({
    args: rest,
    options: {
      dist: { type: "string", default: "dist" },
      out: { type: "string" },
      kind: { type: "string", default: "theme" },
      tokens: { type: "string" },
    },
  });
  if (!values.out || (values.kind !== "theme" && values.kind !== "checkout")) {
    console.error(
      "usage: theme-kit pack --out <artifact-root> [--dist dist] [--kind theme|checkout] [--tokens theme.tokens.json]",
    );
    process.exit(2);
  }
  const manifest = await packArtifact({
    dist: values.dist,
    outRoot: values.out,
    kind: values.kind as ArtifactKind,
    tokensFile: values.tokens,
  });
  console.log(manifest.id);
} else {
  console.error("usage: theme-kit pack ...");
  process.exit(2);
}
