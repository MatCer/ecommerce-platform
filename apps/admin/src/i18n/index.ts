import * as i18n from "@solid-primitives/i18n";
import { createEffect, createMemo, createRoot, createSignal } from "solid-js";
import { ApiError } from "../lib/authed-fetch.ts";
import { DraftError } from "../lib/product-form.ts";
import { SignedOutError } from "../lib/session.ts";
import { cs } from "./cs.ts";
import { type Dictionary, en } from "./en.ts";
import { sk } from "./sk.ts";

export const LOCALES = ["cs", "sk", "en"] as const;
export type Locale = (typeof LOCALES)[number];

const dictionaries: Record<Locale, Dictionary> = { cs, sk, en };
const KEY = "admin.locale";

function initialLocale(): Locale {
  const stored = globalThis.localStorage?.getItem(KEY);
  if (stored && (LOCALES as readonly string[]).includes(stored)) return stored as Locale;
  const nav = globalThis.navigator?.language.slice(0, 2);
  return (LOCALES as readonly string[]).includes(nav ?? "") ? (nav as Locale) : "cs";
}

const [locale, setLocaleSignal] = createSignal<Locale>(initialLocale());

export { locale };

export function setLocale(l: Locale): void {
  localStorage.setItem(KEY, l);
  setLocaleSignal(l);
}

const { flat, dateFmt } = createRoot(() => {
  createEffect(() => {
    document.documentElement.lang = locale();
  });
  return {
    flat: createMemo(() => i18n.flatten(dictionaries[locale()])),
    dateFmt: createMemo(
      () => new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }),
    ),
  };
});

/** `t("products.title")`, `t("staff.invited", { email })`. Keys are type-checked. */
export const t = i18n.translator(flat, i18n.resolveTemplate);

/** Content locales in the order to prefer for display names. */
export function contentLocales(): string[] {
  return [locale(), ...LOCALES.filter((l) => l !== locale())];
}

/** A user-facing message for any error thrown by the API layer. */
export function errorMessage(err: unknown): string {
  if (err instanceof ApiError || err instanceof DraftError) {
    const key = `errors.${err.code}`;
    const known = flat()[key as keyof ReturnType<typeof flat>];
    if (typeof known === "string") return known;
    return t("errors.generic", { code: err.code });
  }
  if (err instanceof SignedOutError) return t("errors.reauth_required");
  if (err instanceof TypeError) return t("errors.network");
  return t("errors.generic", { code: "unknown" });
}

export function formatDateTime(iso: string): string {
  return dateFmt().format(new Date(iso));
}
