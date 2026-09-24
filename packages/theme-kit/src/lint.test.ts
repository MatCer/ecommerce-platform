import { cp, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";
import { lintTheme } from "./lint.ts";

const DEFAULT_THEME = fileURLToPath(new URL("../../../themes/default", import.meta.url));

async function copyTheme() {
  const dir = await mkdtemp(path.join(tmpdir(), "wp2-lint-"));
  await cp(path.join(DEFAULT_THEME, "src"), path.join(dir, "src"), { recursive: true });
  await cp(path.join(DEFAULT_THEME, "package.json"), path.join(dir, "package.json"));
  return dir;
}

test("the default theme passes its own contract", async () => {
  expect(
    await lintTheme(DEFAULT_THEME, {
      referencePackageJson: path.join(DEFAULT_THEME, "package.json"),
    }),
  ).toEqual([]);
});

test("flags foreign fetch, set:html on arbitrary data, inline handlers, cookies and new deps", async () => {
  const dir = await copyTheme();
  await writeFile(
    path.join(dir, "src/pages/evil.astro"),
    [
      '---\nconst r = await fetch("https://tracker.example/x");\n---',
      "<div set:html={Astro.url.searchParams.get('x')} />",
      '<button onclick="alert(1)">x</button>',
      "<script>document.cookie = 'a=1'</script>",
    ].join("\n"),
  );
  await writeFile(
    path.join(dir, "package.json"),
    JSON.stringify({ dependencies: { "left-pad": "1.0.0" } }),
  );
  const rules = (
    await lintTheme(dir, { referencePackageJson: path.join(DEFAULT_THEME, "package.json") })
  ).map((v) => v.rule);
  expect(rules).toEqual(
    expect.arrayContaining([
      "foreign-fetch",
      "set-html",
      "inline-handler",
      "cookie-access",
      "locked-deps",
    ]),
  );
});
