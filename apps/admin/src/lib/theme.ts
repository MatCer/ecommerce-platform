import { createSignal } from "solid-js";

export type Theme = "light" | "dark";
const KEY = "admin.theme";

function initial(): Theme {
  const stored = globalThis.localStorage?.getItem(KEY);
  if (stored === "light" || stored === "dark") return stored;
  return globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

const [theme, setThemeSignal] = createSignal<Theme>(initial());

export { theme };

export function applyTheme(): void {
  document.documentElement.classList.toggle("dark", theme() === "dark");
}

export function setTheme(value: Theme): void {
  localStorage.setItem(KEY, value);
  setThemeSignal(value);
  applyTheme();
}
