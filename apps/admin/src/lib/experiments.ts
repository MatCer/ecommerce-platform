import type { en } from "../i18n/en.ts";
import { formatNumber, formatPercent } from "./analytics.ts";
import type { Schemas } from "./api.ts";

type Copy = (typeof en)["experiments"];
type StateKey = keyof Copy["state"] & string;
type StopKey = `experiments.stopReason.${keyof Copy["stopReason"] & string}`;

export interface Message {
  key:
    | `experiments.state.${StateKey}`
    | StopKey
    | `experiments.change.${keyof Copy["change"] & string}`;
  params: Record<string, string>;
}

type Readout = Schemas["Readout"];

const state = (key: StateKey, params: Record<string, string> = {}): Message => ({
  key: `experiments.state.${key}`,
  params,
});

const snapshotDay = (iso: string, locale: string) =>
  new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeZone: "UTC" }).format(new Date(iso));

const localDay = (iso: string, locale: string) =>
  new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(new Date(iso));

export const flooredChance = (p: number, locale: string) =>
  formatPercent(Math.floor(p * 1000) / 1000, locale);

export const points = (rate: number, locale: string) =>
  new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(rate * 100);

const COLLECTING: Record<Schemas["Need"], (r: Readout, locale: string) => Message> = {
  time: (r, locale) => state("tooEarly", { date: snapshotDay(r.decides_from, locale) }),
  converters: (r, locale) =>
    state("fewBuyers", {
      min: formatNumber(r.min_converters, locale),
      control: formatNumber(r.control.converters, locale),
      variant: formatNumber(r.variant.converters, locale),
    }),
  evidence: () => state("unclear"),
};
const WINNER: Record<Schemas["Side"], StateKey> = {
  control: "winnerControl",
  variant: "winnerVariant",
};
const NO_DIFFERENCE: Record<Schemas["Basis"], StateKey> = {
  equivalent: "same",
  ended: "noWinner",
};

export function readoutMessage(detail: Schemas["ExperimentDetail"], locale: string): Message {
  if (detail.experiment.stop_reason === "srm") return state("untrusted");
  const r = detail.readout;
  if (!r) return state("draft");
  const s = r.state;
  switch (s.state) {
    case "collecting": {
      const willNeverDecide =
        r.final || (s.need === "time" && detail.experiment.status === "stopped");
      return willNeverDecide ? state("notEnough") : COLLECTING[s.need](r, locale);
    }
    case "winner":
      return state(WINNER[s.arm], { chance: flooredChance(r[s.arm].p_best, locale) });
    case "no_difference":
      return state(NO_DIFFERENCE[s.basis]);
  }
}

const STOP_KEYS: Record<Schemas["StopReason"], StopKey> = {
  manual: "experiments.stopReason.manual",
  promoted: "experiments.stopReason.promoted",
  srm: "experiments.stopReason.srm",
  harm: "experiments.stopReason.harm",
  max_duration: "experiments.stopReason.max_duration",
};

export function stopMessage(e: Schemas["Experiment"], locale: string): Message | undefined {
  if (!e.stop_reason || !e.stopped_at) return undefined;
  return { key: STOP_KEYS[e.stop_reason], params: { date: localDay(e.stopped_at, locale) } };
}

export function changeMessage(e: Schemas["Experiment"]): Message {
  const c = e.change;
  return c.kind === "theme"
    ? { key: "experiments.change.theme", params: { number: String(c.revision_number) } }
    : { key: `experiments.change.${c.kind}`, params: {} };
}

export const HERO_FIELDS = ["title", "subtitle", "cta"] as const;
export type HeroFields = Record<(typeof HERO_FIELDS)[number], string>;

export function heroCopy(fields: Record<string, HeroFields>): Schemas["HeroCopy"] | undefined {
  const copy: Schemas["HeroCopy"] = {};
  for (const [locale, texts] of Object.entries(fields))
    for (const field of HERO_FIELDS) {
      const text = texts[field].trim();
      if (text) copy[field] = { ...copy[field], [locale]: text };
    }
  return Object.keys(copy).length > 0 ? copy : undefined;
}
