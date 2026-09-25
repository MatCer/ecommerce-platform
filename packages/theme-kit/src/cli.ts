#!/usr/bin/env node
import { mkdir, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import { parseArgs } from "node:util";
import { type ArtifactKind, packArtifact, verifyArtifact } from "./artifact.ts";
import { lintTheme } from "./lint.ts";

const [command, ...rest] = process.argv.slice(2);

if (command === "pack") {
  const { values } = parseArgs({
    args: rest,
    options: {
      dist: { type: "string", default: "dist" },
      out: { type: "string" },
      kind: { type: "string", default: "theme" },
      tokens: { type: "string" },
      channel: { type: "string" },
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
  if (values.channel) {
    if (!/^[a-z0-9-]{1,64}$/.test(values.channel)) throw new Error("invalid channel name");
    // Pointer file, written atomically: publishing = moving the pointer (A22).
    const dir = path.join(values.out, "channels");
    await mkdir(dir, { recursive: true });
    await writeFile(path.join(dir, `.${values.channel}.tmp`), `${manifest.id}\n`);
    await rename(path.join(dir, `.${values.channel}.tmp`), path.join(dir, values.channel));
  }
  console.log(manifest.id);
} else if (command === "verify") {
  // Before publishing (A22): the directory must be exactly the content-addressed artifact.
  const { values, positionals } = parseArgs({
    args: rest,
    allowPositionals: true,
    options: { root: { type: "string", default: ".artifacts" } },
  });
  for (const id of positionals) {
    const m = await verifyArtifact(values.root, id);
    console.log(
      `${id}: ok (${m.kind}, ${m.runtime.modules.length} modules, ${Object.keys(m.assets).length} assets)`,
    );
  }
} else if (command === "lint") {
  const { values, positionals } = parseArgs({
    args: rest,
    allowPositionals: true,
    options: { reference: { type: "string" } },
  });
  // `--reference` is the platform's theme directory (or, as before, its package.json).
  const ref = values.reference;
  const violations = await lintTheme(positionals[0] ?? ".", {
    referenceDir: ref && (ref.endsWith("package.json") ? path.dirname(ref) : ref),
  });
  for (const v of violations) console.log(`${v.file}:${v.line} ${v.rule}: ${v.message}`);
  console.log(
    violations.length ? `${violations.length} contract violation(s)` : "theme contract: ok",
  );
  process.exit(violations.length ? 1 : 0);
} else {
  console.error(
    "usage: theme-kit pack ... | theme-kit verify [--root dir] <id>... | theme-kit lint <theme-dir> [--reference <platform theme dir>]",
  );
  process.exit(2);
}
