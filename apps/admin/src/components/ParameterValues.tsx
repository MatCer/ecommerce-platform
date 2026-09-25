import { Button, Checkbox, SelectField, TextField } from "@platform/ui";
import { A } from "@solidjs/router";
import { For, Index, Show } from "solid-js";
import { contentLocales, t } from "../i18n/index.ts";
import type { Schemas } from "../lib/api.ts";
import { CONTENT_LOCALES, type ParameterValue } from "../lib/product-form.ts";

type Parameter = Schemas["Parameter"];

function name(p: Parameter): string {
  for (const l of contentLocales()) if (p.name_i18n[l]) return p.name_i18n[l];
  return p.key;
}

function emptyValue(p: Parameter): unknown {
  return p.kind === "text" ? {} : p.kind === "number" ? 0 : false;
}

const asText = (v: unknown): Record<string, string> =>
  typeof v === "object" && v !== null ? (v as Record<string, string>) : {};

/** Parameter values of a product: text per locale, number or yes/no; product or variant level. */
export function ParameterValues(props: {
  parameters: Parameter[];
  values: ParameterValue[];
  skus: string[];
  onChange: (v: ParameterValue[]) => void;
}) {
  const byId = (id: string) => props.parameters.find((p) => p.id === id);
  const set = (i: number, patch: Partial<ParameterValue>) =>
    props.onChange(props.values.map((v, j) => (j === i ? { ...v, ...patch } : v)));

  return (
    <Show
      when={props.parameters.length > 0}
      fallback={
        <p class="text-sm text-muted-foreground">
          {t("editor.noParameters")}{" "}
          <A href="/parameters" class="text-accent-700 underline">
            {t("nav.parameters")}
          </A>
        </p>
      }
    >
      <div class="flex flex-col gap-2">
        <Index each={props.values}>
          {(v, i) => {
            const param = () => byId(v().parameter_id);
            return (
              <div class="flex flex-wrap items-end gap-2 border-b border-border pb-2">
                <SelectField
                  class="w-48"
                  label={t("editor.parameter")}
                  value={v().parameter_id}
                  options={props.parameters.map((p) => ({ value: p.id, label: name(p) }))}
                  onChange={(id) => {
                    const p = byId(id);
                    if (p) set(i, { parameter_id: id, value: emptyValue(p) });
                  }}
                />
                <Show when={param()}>
                  {(p) => (
                    <>
                      <Show when={p().kind === "text"}>
                        <For each={CONTENT_LOCALES}>
                          {(l) => (
                            <TextField
                              class="w-40"
                              label={`${t("editor.value")} (${l})`}
                              value={asText(v().value)[l] ?? ""}
                              onChange={(text) => {
                                const next = { ...asText(v().value), [l]: text };
                                if (!text.trim()) delete next[l];
                                set(i, { value: next });
                              }}
                            />
                          )}
                        </For>
                      </Show>
                      <Show when={p().kind === "number"}>
                        <TextField
                          class="w-32"
                          label={`${t("editor.value")}${p().unit ? ` (${p().unit})` : ""}`}
                          inputMode="decimal"
                          inputClass="figures text-right"
                          value={
                            typeof v().value === "number" || typeof v().value === "string"
                              ? String(v().value)
                              : ""
                          }
                          // Kept as typed ("1." or "-") and parsed when the product is saved.
                          onChange={(text) => set(i, { value: text })}
                        />
                      </Show>
                      <Show when={p().kind === "bool"}>
                        <Checkbox
                          class="pb-1.5"
                          label={name(p())}
                          checked={v().value === true}
                          onChange={(checked) => set(i, { value: checked })}
                        />
                      </Show>
                    </>
                  )}
                </Show>
                <Show when={props.skus.length > 1}>
                  <SelectField
                    class="w-44"
                    label={t("editor.appliesTo")}
                    value={v().variant_sku ?? ""}
                    options={[
                      { value: "", label: t("editor.wholeProduct") },
                      ...props.skus.map((s) => ({ value: s, label: s })),
                    ]}
                    onChange={(sku) => set(i, { variant_sku: sku || null })}
                  />
                </Show>
                <Button
                  variant="ghost"
                  onClick={() => props.onChange(props.values.filter((_, j) => j !== i))}
                >
                  {t("common.remove")}
                  <span class="sr-only">: {param() ? name(param() as Parameter) : ""}</span>
                </Button>
              </div>
            );
          }}
        </Index>
        <div>
          <Button
            onClick={() => {
              const first = props.parameters[0];
              if (first) {
                props.onChange([
                  ...props.values,
                  { parameter_id: first.id, value: emptyValue(first), variant_sku: null },
                ]);
              }
            }}
          >
            {t("editor.addParameter")}
          </Button>
        </div>
      </div>
    </Show>
  );
}
