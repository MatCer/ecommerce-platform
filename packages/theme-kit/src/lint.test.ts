import { cp, mkdtemp, readFile, writeFile } from "node:fs/promises";
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
  await cp(path.join(DEFAULT_THEME, "theme.tokens.json"), path.join(dir, "theme.tokens.json"));
  return dir;
}

test("the default theme passes its own contract", async () => {
  expect(await lintTheme(DEFAULT_THEME, { referenceDir: DEFAULT_THEME })).toEqual([]);
});

test("platform-owned files and tokens are locked (A6, §9.1)", async () => {
  const dir = await copyTheme();
  // Same JSON, other formatting: fine. Missing platform files: fine (the build overlays them).
  const pkg = JSON.parse(await readFile(path.join(DEFAULT_THEME, "package.json"), "utf8"));
  await writeFile(path.join(dir, "package.json"), JSON.stringify(pkg));
  expect(await lintTheme(dir, { referenceDir: DEFAULT_THEME })).toEqual([]);

  pkg.scripts.build = "node steal.js";
  await writeFile(path.join(dir, "package.json"), JSON.stringify(pkg));
  await writeFile(path.join(dir, "astro.config.mjs"), "export default {}\n");
  await writeFile(
    path.join(dir, "theme.tokens.json"),
    '{"colors":{"x":"url(//e)"},"fonts":{},"radius":{}}',
  );
  const found = (await lintTheme(dir, { referenceDir: DEFAULT_THEME })).map(
    (v) => `${v.file}:${v.rule}`,
  );
  expect(found.sort()).toEqual([
    "astro.config.mjs:locked-files",
    "package.json:locked-deps",
    "theme.tokens.json:tokens",
  ]);
});

test("flags foreign fetch, set:html on arbitrary data, inline handlers, cookies and new deps", async () => {
  const dir = await copyTheme();
  await writeFile(
    path.join(dir, "src/pages/evil.astro"),
    [
      '---\nconst r = await fetch("https://tracker.example/x");\n---',
      "<div set:html={Astro.url.searchParams.get('x')} />",
      '<button onclick="alert(1)">x</button>',
      "<RecentlyViewed client:visible />",
      "<script>document.cookie = 'a=1'</script>",
    ].join("\n"),
  );
  await writeFile(
    path.join(dir, "package.json"),
    JSON.stringify({ dependencies: { "left-pad": "1.0.0" } }),
  );
  const rules = (await lintTheme(dir, { referenceDir: DEFAULT_THEME })).map((v) => v.rule);
  expect(rules).toEqual(
    expect.arrayContaining([
      "foreign-fetch",
      "set-html",
      "inline-handler",
      "cookie-access",
      "client-visible",
      "locked-deps",
    ]),
  );
});
