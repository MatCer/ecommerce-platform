import { readFile } from "node:fs/promises";
import path from "node:path";
import { tokensToCss, validateTokens } from "./tokens.ts";

const ID = "virtual:theme-tokens.css";
const RESOLVED = "/__theme-tokens.css";

/**
 * Vite plugin: `import "virtual:theme-tokens.css"` yields the validated `theme.tokens.json` as
 * CSS custom properties, so the JSON file is the single source of truth for theme and checkout.
 */
export function themeTokens(file = "theme.tokens.json") {
  let abs = file;
  return {
    name: "platform-theme-tokens",
    configResolved(config: { root: string }) {
      abs = path.resolve(config.root, file);
    },
    resolveId(id: string) {
      return id === ID ? RESOLVED : undefined;
    },
    async load(id: string) {
      if (id !== RESOLVED) return undefined;
      return tokensToCss(validateTokens(JSON.parse(await readFile(abs, "utf8"))));
    },
  };
}
