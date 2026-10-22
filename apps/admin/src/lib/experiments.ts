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

/** Snapshots open at UTC midnight, so the UTC calendar day names them. */
const snapshotDay = (iso: string, locale: string) =>
  new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeZone: "UTC" }).format(new Date(iso));

/** The chance an arm is better, floored so that a near-certain winner never reads "100 %". */
export const chance = (p: number, locale: string) =>
  formatPercent(Math.floor(p * 1000) / 1000, locale);

/** An absolute conversion rate (0.0004) in percentage points (0.04). */
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

/**
 * The sentence that sums up an experiment's readout. A stopped experiment keeps collecting the
 * purchases of its last week of visitors, so "not enough data" waits for the final readout.
 */
export function readoutMessage(detail: Schemas["ExperimentDetail"], locale: string): Message {
  if (detail.experiment.stop_reason === "srm") return state("untrusted");
  const r = detail.readout;
  if (!r) return state("draft");
  const s = r.state;
  switch (s.state) {
    case "collecting":
      return r.final || (s.need === "time" && detail.experiment.status === "stopped")
        ? state("notEnough")
        : COLLECTING[s.need](r, locale);
    case "winner":
      return state(WINNER[s.arm], { chance: chance(r[s.arm].p_best, locale) });
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

/** Why and when a stopped experiment stopped, on the merchant's own calendar day. */
export function stopMessage(e: Schemas["Experiment"], locale: string): Message | undefined {
  if (!e.stop_reason || !e.stopped_at) return undefined;
  const date = new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(
    new Date(e.stopped_at),
  );
  return { key: STOP_KEYS[e.stop_reason], params: { date } };
}

export function changeMessage(e: Schemas["Experiment"]): Message {
  const c = e.change;
  return c.kind === "theme"
    ? { key: "experiments.change.theme", params: { number: String(c.revision_number) } }
    : { key: `experiments.change.${c.kind}`, params: {} };
}

export const HERO_FIELDS = ["title", "subtitle", "cta"] as const;
export type HeroFields = Record<(typeof HERO_FIELDS)[number], string>;

/** The hero copy a form sends, per market locale: empty texts keep the shop's current text. */
export function heroCopy(fields: Record<string, HeroFields>): Schemas["HeroCopy"] | undefined {
  const copy: Schemas["HeroCopy"] = {};
  for (const [locale, texts] of Object.entries(fields))
    for (const field of HERO_FIELDS) {
      const text = texts[field].trim();
      if (text) copy[field] = { ...copy[field], [locale]: text };
    }
  return Object.keys(copy).length > 0 ? copy : undefined;
}
