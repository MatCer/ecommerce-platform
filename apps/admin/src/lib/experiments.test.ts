import * as i18n from "@solid-primitives/i18n";
import { describe, expect, it } from "vitest";
import { cs } from "../i18n/cs.ts";
import { en } from "../i18n/en.ts";
import { sk } from "../i18n/sk.ts";
import type { Schemas } from "./api.ts";
import {
  changeMessage,
  heroCopy,
  type Message,
  points,
  readoutMessage,
  stopMessage,
} from "./experiments.ts";

const dictionaries = { cs, sk, en };
type Locale = keyof typeof dictionaries;

function say(m: (locale: Locale) => Message | undefined, locale: Locale): string {
  const t = i18n.translator(() => i18n.flatten(dictionaries[locale]), i18n.resolveTemplate);
  const message = m(locale);
  if (!message) return "";
  const { key, params } = message;
  return t(key, params).replaceAll("\u00a0", " ");
}

const sentences = (m: (locale: Locale) => Message | undefined) => ({
  en: say(m, "en"),
  cs: say(m, "cs"),
  sk: say(m, "sk"),
});

const experiment: Schemas["Experiment"] = {
  id: "0190a0b0-0000-7000-8000-000000000001",
  name: "spring-hero",
  version: 1,
  status: "running",
  change: { kind: "hero", hero: { title: { cs: "Jaro je tady" } } },
  created_at: "2026-09-30T12:00:00Z",
  started_at: "2026-10-01T12:00:00Z",
  stopped_at: null,
  stop_reason: null,
  promoted_at: null,
};

const arm = (converters: number, p_best: number): Schemas["ArmReadout"] => ({
  assigned: 6000,
  subjects: 5000,
  converters,
  conversion_rate: converters / 5000,
  p_best,
  expected_loss: 0.0004,
});

const readout: Schemas["Readout"] = {
  as_of: "2026-10-22T00:00:00Z",
  decides_from: "2026-10-23T00:00:00Z",
  final: false,
  min_converters: 100,
  state: { state: "collecting", need: "evidence" },
  control: arm(140, 0.4),
  variant: arm(150, 0.6),
};

const running = (state: Schemas["ReadoutState"], r: Partial<Schemas["Readout"]> = {}) => ({
  experiment,
  readout: { ...readout, ...r, state },
});

describe("readout sentences", () => {
  it("says a draft has not started", () => {
    const draft = { ...experiment, status: "draft" as const, started_at: null };
    expect(sentences((l) => readoutMessage({ experiment: draft, readout: null }, l))).toEqual({
      en: "Not started yet.",
      cs: "Experiment ještě neběží.",
      sk: "Experiment ešte nebeží.",
    });
  });

  it("names the first day a result is possible while it is too early", () => {
    const d = running({ state: "collecting", need: "time" });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "Collecting data. The first result is possible on Oct 23, 2026.",
      cs: "Sbírají se data. První výsledek bude k dispozici nejdříve 23. 10. 2026.",
      sk: "Zbierajú sa údaje. Prvý výsledok bude k dispozícii najskôr 23. 10. 2026.",
    });
  });

  it("quotes the buyers each version needs and has so far", () => {
    const d = running(
      { state: "collecting", need: "converters" },
      { control: arm(87, 0.3), variant: arm(112, 0.7) },
    );
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "Collecting data. Each version needs at least 100 buyers; so far the current version has 87 and the variant 112.",
      cs: "Sbírají se data. Každá verze potřebuje alespoň 100 kupujících, zatím jich má současná verze 87 a varianta 112.",
      sk: "Zbierajú sa údaje. Každá verzia potrebuje aspoň 100 kupujúcich, zatiaľ ich má súčasná verzia 87 a variant 112.",
    });
  });

  it("says neither version leads while the evidence is unclear", () => {
    const d = running({ state: "collecting", need: "evidence" });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "Collecting data. Neither version is clearly ahead yet.",
      cs: "Sbírají se data. Zatím není jasné, která verze je lepší.",
      sk: "Zbierajú sa údaje. Zatiaľ nie je jasné, ktorá verzia je lepšia.",
    });
  });

  it("names a winning variant with its chance to be better", () => {
    const d = running(
      { state: "winner", arm: "variant" },
      { control: arm(140, 0.0029), variant: arm(190, 0.9971) },
    );
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "The variant sells better: 99.7% chance it is the better version.",
      cs: "Varianta prodává lépe: s pravděpodobností 99,7 % je to lepší verze.",
      sk: "Variant predáva lepšie: s pravdepodobnosťou 99,7 % je to lepšia verzia.",
    });
  });

  it("tells the merchant to keep a winning current version, never rounding up to certainty", () => {
    const d = running(
      { state: "winner", arm: "control" },
      { control: arm(190, 0.99981), variant: arm(140, 0.00019) },
    );
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "The current version sells better: 99.9% chance it is the better version. Keep it.",
      cs: "Současná verze prodává lépe: s pravděpodobností 99,9 % je to lepší verze. Ponechte ji.",
      sk: "Súčasná verzia predáva lepšie: s pravdepodobnosťou 99,9 % je to lepšia verzia. Ponechajte ju.",
    });
  });

  it("says equivalent versions cost nothing to keep", () => {
    const d = running({ state: "no_difference", basis: "equivalent" });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "Both versions sell about the same. Keeping the current version costs nothing.",
      cs: "Obě verze prodávají zhruba stejně. Ponechat současnou verzi nic nestojí.",
      sk: "Obe verzie predávajú približne rovnako. Ponechať súčasnú verziu nič nestojí.",
    });
  });

  it("says an ended experiment found no clear difference", () => {
    const d = running({ state: "no_difference", basis: "ended" }, { final: true });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "The experiment ended without a clear difference. Keep the current version.",
      cs: "Experiment skončil bez jasného rozdílu. Ponechte současnou verzi.",
      sk: "Experiment sa skončil bez jasného rozdielu. Ponechajte súčasnú verziu.",
    });
  });
});

const stopped = (
  reason: Schemas["StopReason"],
  state: Schemas["ReadoutState"],
  r: Partial<Schemas["Readout"]> = {},
) => ({
  experiment: {
    ...experiment,
    status: "stopped" as const,
    stop_reason: reason,
    stopped_at: "2026-10-21T12:00:00Z",
  },
  readout: { ...readout, ...r, state },
});

describe("readout sentences of a stopped experiment", () => {
  it("distrusts an experiment stopped for an uneven split before anything else", () => {
    const d = stopped("srm", { state: "collecting", need: "evidence" }, { final: true });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "Visitors were not split as planned, so these results cannot be trusted.",
      cs: "Návštěvníci nebyli rozděleni podle plánu, výsledkům proto nelze věřit.",
      sk: "Návštevníci neboli rozdelení podľa plánu, výsledkom preto nemožno veriť.",
    });
  });

  it("says it stopped before there was enough data once every purchase window closed", () => {
    for (const need of ["time", "converters", "evidence"] as const) {
      const d = stopped("manual", { state: "collecting", need }, { final: true });
      expect(sentences((l) => readoutMessage(d, l))).toEqual({
        en: "The experiment stopped before there was enough data for a decision.",
        cs: "Experiment skončil bez dostatku dat pro rozhodnutí.",
        sk: "Experiment sa skončil bez dostatku údajov na rozhodnutie.",
      });
    }
  });

  it("says at once that it stopped before there was enough time", () => {
    const d = stopped("manual", { state: "collecting", need: "time" });
    expect(sentences((l) => readoutMessage(d, l))).toEqual({
      en: "The experiment stopped before there was enough data for a decision.",
      cs: "Experiment skončil bez dostatku dat pro rozhodnutí.",
      sk: "Experiment sa skončil bez dostatku údajov na rozhodnutie.",
    });
  });

  it("keeps counting the last week of visitors after the stop", () => {
    const d = stopped(
      "manual",
      { state: "collecting", need: "converters" },
      { control: arm(98, 0.4), variant: arm(99, 0.6) },
    );
    expect(say((l) => readoutMessage(d, l), "en")).toBe(
      "Collecting data. Each version needs at least 100 buyers; so far the current version has 98 and the variant 99.",
    );
  });

  it("names a winner that appeared after the stop", () => {
    const d = stopped(
      "max_duration",
      { state: "winner", arm: "variant" },
      { final: true, control: arm(140, 0.003), variant: arm(190, 0.997) },
    );
    expect(say((l) => readoutMessage(d, l), "en")).toBe(
      "The variant sells better: 99.7% chance it is the better version.",
    );
  });
});

describe("stop sentences", () => {
  const at = (reason: Schemas["StopReason"]) => (l: Locale) =>
    stopMessage(stopped(reason, readout.state).experiment, l);

  it("says when and why every stop happened", () => {
    expect(
      (["manual", "promoted", "srm", "harm", "max_duration"] as const).map((r) => sentences(at(r))),
    ).toEqual([
      {
        en: "Stopped manually on Oct 21, 2026.",
        cs: "Zastaveno ručně 21. 10. 2026.",
        sk: "Zastavené ručne 21. 10. 2026.",
      },
      {
        en: "Stopped on Oct 21, 2026 when the variant was published.",
        cs: "Zastaveno 21. 10. 2026 při zveřejnění varianty.",
        sk: "Zastavené 21. 10. 2026 pri zverejnení variantu.",
      },
      {
        en: "Stopped automatically on Oct 21, 2026 because visitors were not assigned in the planned split.",
        cs: "Automaticky zastaveno 21. 10. 2026, protože přiřazení návštěvníků neodpovídalo plánovanému rozdělení.",
        sk: "Automaticky zastavené 21. 10. 2026, pretože priradenie návštevníkov nezodpovedalo plánovanému rozdeleniu.",
      },
      {
        en: "Stopped automatically on Oct 21, 2026 because the variant was very likely reducing purchases.",
        cs: "Automaticky zastaveno 21. 10. 2026, protože varianta s vysokou pravděpodobností snižovala počet nákupů.",
        sk: "Automaticky zastavené 21. 10. 2026, pretože variant s vysokou pravdepodobnosťou znižoval počet nákupov.",
      },
      {
        en: "Stopped automatically on Oct 21, 2026 at the 56-day limit.",
        cs: "Automaticky zastaveno 21. 10. 2026 po dosažení limitu 56 dní.",
        sk: "Automaticky zastavené 21. 10. 2026 po dosiahnutí limitu 56 dní.",
      },
    ]);
  });

  it("has nothing to say about an experiment that has not stopped", () => {
    expect(stopMessage(experiment, "en")).toBeUndefined();
  });
});

describe("what the variant changes", () => {
  it("names hero text, a theme revision by number, or theme code", () => {
    const of = (change: Schemas["ExperimentChange"]) => () =>
      changeMessage({ ...experiment, change });
    expect(say(of(experiment.change), "en")).toBe("Hero text");
    expect(
      sentences(
        of({
          kind: "theme",
          revision_id: "0190a0b0-0000-7000-8000-000000000009",
          revision_number: 12,
        }),
      ),
    ).toEqual({
      en: "Theme revision #12",
      cs: "Revize vzhledu #12",
      sk: "Revízia vzhľadu #12",
    });
    expect(say(of({ kind: "component" }), "cs")).toBe("Kód vzhledu");
  });
});

describe("expected loss", () => {
  it("reads an absolute conversion rate as percentage points", () => {
    expect(points(0.0004, "en")).toBe("0.04");
    expect(points(0.0123, "cs")).toBe("1,23");
  });
});

describe("hero copy from the form", () => {
  it("keeps the texts filled in every market language, trimmed", () => {
    expect(
      heroCopy(
        {
          cs: { title: " Nová kolekce ", subtitle: "", cta: "Nakupovat" },
          sk: { title: "Nová kolekcia", subtitle: "   ", cta: "Nakupovať" },
        },
        ["cs", "sk"],
      ),
    ).toEqual({
      copy: {
        title: { cs: "Nová kolekce", sk: "Nová kolekcia" },
        cta: { cs: "Nakupovat", sk: "Nakupovať" },
      },
    });
  });

  it("is empty when every text is empty", () => {
    expect(heroCopy({ cs: { title: " ", subtitle: "", cta: "" } }, ["cs", "sk"])).toEqual({
      copy: undefined,
    });
    expect(heroCopy({}, ["cs", "sk"])).toEqual({ copy: undefined });
  });

  it("refuses a text filled in only some market languages, on each empty one", () => {
    const input = heroCopy(
      {
        cs: { title: "Podzimní výprodej", subtitle: "", cta: "Vybrat si" },
        sk: { title: "", subtitle: "", cta: "Vybrať si" },
      },
      ["cs", "sk"],
    );
    const missing = "missing" in input ? input.missing : {};
    expect(Object.keys(missing)).toEqual(["sk"]);
    expect(Object.keys(missing.sk ?? {})).toEqual(["title"]);
    expect(sentences(() => missing.sk?.title)).toEqual({
      en: "Fill this text in too, or leave it empty in every language.",
      cs: "Vyplňte i tento text, nebo ho nechte prázdný ve všech jazycích.",
      sk: "Vyplňte aj tento text, alebo ho nechajte prázdny vo všetkých jazykoch.",
    });

    const untouched = heroCopy({ cs: { title: "Podzimní výprodej", subtitle: "", cta: "" } }, [
      "cs",
      "sk",
    ]);
    expect("missing" in untouched && Object.keys(untouched.missing)).toEqual(["sk"]);
  });
});
