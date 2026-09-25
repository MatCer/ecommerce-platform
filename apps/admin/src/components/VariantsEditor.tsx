import { Button, TextField } from "@platform/ui";
import { For, Index, Show } from "solid-js";
import { contentLocales, t } from "../i18n/index.ts";
import {
  CONTENT_LOCALES,
  codify,
  compactI18n,
  eanValid,
  type ProductOption,
  type VariantDraft,
  variantMatrix,
} from "../lib/product-form.ts";
import { Th, tableClass, tdClass } from "./Page.tsx";

function label(names: Record<string, string>): string {
  for (const l of contentLocales()) if (names[l]) return names[l];
  return Object.values(names)[0] ?? "";
}

/** Code for a new option/value: from its first name, unique among `taken`. */
function freshCode(names: Record<string, string>, taken: readonly string[], fallback: string) {
  const base = codify(label(names)) || fallback;
  let code = base;
  for (let i = 2; taken.includes(code); i++) code = `${base}-${i}`;
  return code;
}

function OptionEditor(props: {
  option: ProductOption;
  index: number;
  locked: boolean;
  onChange: (o: ProductOption) => void;
  onRemove: () => void;
}) {
  const heading = () => label(props.option.name_i18n) || `#${props.index + 1}`;
  const setName = (l: string, v: string) => {
    const name_i18n = compactI18n({ ...props.option.name_i18n, [l]: v });
    props.onChange({
      ...props.option,
      name_i18n: { ...props.option.name_i18n, [l]: v },
      code: props.locked ? props.option.code : codify(label(name_i18n)) || props.option.code,
    });
  };
  const setValue = (vi: number, l: string, v: string) => {
    const values = props.option.values.map((val, j) =>
      j === vi ? { ...val, name_i18n: { ...val.name_i18n, [l]: v } } : val,
    );
    props.onChange({ ...props.option, values });
  };
  const addValue = () => {
    const code = freshCode(
      {},
      props.option.values.map((v) => v.code),
      `v${props.option.values.length + 1}`,
    );
    props.onChange({ ...props.option, values: [...props.option.values, { code, name_i18n: {} }] });
  };
  return (
    <fieldset class="flex flex-col gap-2 rounded-md border border-border p-3">
      <legend class="px-1 text-sm font-semibold">{heading()}</legend>
      <div class="grid gap-2 sm:grid-cols-4">
        <For each={CONTENT_LOCALES}>
          {(l) => (
            <TextField
              label={`${t("editor.optionName")} (${l})`}
              value={props.option.name_i18n[l] ?? ""}
              onChange={(v) => setName(l, v)}
              maxLength={100}
            />
          )}
        </For>
        <TextField
          label={t("editor.optionCode")}
          value={props.option.code}
          onChange={(v) => props.onChange({ ...props.option, code: v })}
          inputClass="figures"
          maxLength={64}
        />
      </div>
      <table class={tableClass}>
        <thead>
          <tr>
            <For each={CONTENT_LOCALES}>
              {(l) => <Th>{`${t("editor.optionValues")} (${l})`}</Th>}
            </For>
            <Th>{t("editor.optionCode")}</Th>
            <Th srOnly>{t("common.actions")}</Th>
          </tr>
        </thead>
        <tbody>
          <Index each={props.option.values}>
            {(val, vi) => (
              <tr>
                <For each={CONTENT_LOCALES}>
                  {(l) => (
                    <td class={`${tdClass} py-1`}>
                      <TextField
                        hideLabel
                        label={`${heading()}: ${t("editor.optionValues")} ${vi + 1} (${l})`}
                        value={val().name_i18n[l] ?? ""}
                        onChange={(v) => setValue(vi, l, v)}
                        maxLength={100}
                      />
                    </td>
                  )}
                </For>
                <td class={`${tdClass} figures text-xs text-muted-foreground`}>{val().code}</td>
                <td class={`${tdClass} text-right`}>
                  <Button
                    variant="ghost"
                    disabled={props.option.values.length <= 1}
                    onClick={() =>
                      props.onChange({
                        ...props.option,
                        values: props.option.values.filter((_, j) => j !== vi),
                      })
                    }
                  >
                    {t("common.remove")}
                    <span class="sr-only">: {label(val().name_i18n) || val().code}</span>
                  </Button>
                </td>
              </tr>
            )}
          </Index>
        </tbody>
      </table>
      <div class="flex flex-wrap gap-2">
        <Button onClick={addValue}>{t("common.add")}</Button>
        <Button variant="ghost" onClick={props.onRemove}>
          {t("editor.removeOption")}
        </Button>
      </div>
    </fieldset>
  );
}

/** Options (with translated names and values) and the variant matrix (SKU, EAN, weight). */
export function VariantsEditor(props: {
  options: ProductOption[];
  variants: VariantDraft[];
  skuBase: string;
  onOptions: (o: ProductOption[]) => void;
  onVariants: (v: VariantDraft[]) => void;
}) {
  const usedCodes = () => new Set(props.variants.flatMap((v) => Object.keys(v.option_values)));

  const optionLabel = (v: VariantDraft) =>
    props.options
      .map((o) => {
        const val = o.values.find((x) => x.code === v.option_values[o.code]);
        return val ? label(val.name_i18n) || val.code : "?";
      })
      .join(" / ") || "—";

  const regenerate = () => {
    // Settle value codes from names first, so generated variants reference final codes.
    const options = props.options.map((o) => {
      const codes: string[] = [];
      return {
        ...o,
        name_i18n: compactI18n(o.name_i18n),
        values: o.values.map((v) => {
          const code =
            /^v\d+(-\d+)?$/.test(v.code) && label(v.name_i18n)
              ? freshCode(v.name_i18n, codes, v.code)
              : v.code;
          codes.push(code);
          return { code, name_i18n: compactI18n(v.name_i18n) };
        }),
      };
    });
    props.onOptions(options);
    props.onVariants(variantMatrix(options, props.variants, props.skuBase));
  };

  const setVariant = (i: number, patch: Partial<VariantDraft>) =>
    props.onVariants(props.variants.map((v, j) => (j === i ? { ...v, ...patch } : v)));

  return (
    <div class="flex flex-col gap-3">
      <p class="text-xs text-muted-foreground">{t("editor.matrixHint")}</p>
      <Index each={props.options}>
        {(o, i) => (
          <OptionEditor
            option={o()}
            index={i}
            locked={usedCodes().has(o().code)}
            onChange={(next) => props.onOptions(props.options.map((x, j) => (j === i ? next : x)))}
            onRemove={() => props.onOptions(props.options.filter((_, j) => j !== i))}
          />
        )}
      </Index>
      <div class="flex flex-wrap gap-2">
        <Button
          onClick={() =>
            props.onOptions([
              ...props.options,
              {
                code: freshCode(
                  {},
                  props.options.map((o) => o.code),
                  `option${props.options.length + 1}`,
                ),
                name_i18n: {},
                values: [{ code: "v1", name_i18n: {} }],
              },
            ])
          }
        >
          {t("editor.addOption")}
        </Button>
        <Show
          when={props.options.length > 0}
          fallback={
            <Show when={props.variants.length === 0}>
              <Button onClick={regenerate}>{t("editor.singleVariant")}</Button>
            </Show>
          }
        >
          <Button variant="primary" onClick={regenerate}>
            {t("editor.regenerate")}
          </Button>
        </Show>
      </div>
      <Show
        when={props.variants.length > 0}
        fallback={<p class="text-sm text-muted-foreground">{t("editor.noVariants")}</p>}
      >
        <div class="overflow-x-auto">
          <table class={tableClass}>
            <caption class="sr-only">{t("editor.variants")}</caption>
            <thead>
              <tr>
                <Th>{t("editor.variant")}</Th>
                <Th>{t("editor.sku")}</Th>
                <Th>{t("editor.ean")}</Th>
                <Th>{t("editor.weight")}</Th>
                <Th>{t("editor.isDefault")}</Th>
                <Th srOnly>{t("common.actions")}</Th>
              </tr>
            </thead>
            <tbody>
              <Index each={props.variants}>
                {(v, i) => {
                  const name = () => optionLabel(v());
                  const eanError = () => v().ean.trim() !== "" && !eanValid(v().ean.trim());
                  return (
                    <tr>
                      <th scope="row" class={`${tdClass} text-left font-medium`}>
                        {name()}
                      </th>
                      <td class={`${tdClass} py-1`}>
                        <TextField
                          hideLabel
                          label={`${t("editor.sku")}: ${name()}`}
                          value={v().sku}
                          onChange={(sku) => setVariant(i, { sku })}
                          inputClass="figures"
                          maxLength={64}
                          required
                        />
                      </td>
                      <td class={`${tdClass} py-1`}>
                        <TextField
                          hideLabel
                          label={`${t("editor.ean")}: ${name()}`}
                          value={v().ean}
                          onChange={(ean) => setVariant(i, { ean })}
                          inputMode="numeric"
                          inputClass="figures"
                          maxLength={14}
                          error={eanError() && t("editor.invalidEan")}
                        />
                      </td>
                      <td class={`${tdClass} w-28 py-1`}>
                        <TextField
                          hideLabel
                          label={`${t("editor.weight")}: ${name()}`}
                          value={v().weight_g}
                          onChange={(weight_g) => setVariant(i, { weight_g })}
                          inputMode="numeric"
                          inputClass="figures text-right"
                        />
                      </td>
                      <td class={tdClass}>
                        <input
                          type="radio"
                          name="default-variant"
                          class="size-4 accent-accent-600"
                          aria-label={`${t("editor.isDefault")}: ${name()}`}
                          checked={v().is_default}
                          onChange={() =>
                            props.onVariants(
                              props.variants.map((x, j) => ({ ...x, is_default: j === i })),
                            )
                          }
                        />
                      </td>
                      <td class={`${tdClass} text-right`}>
                        <Button
                          variant="ghost"
                          onClick={() => props.onVariants(props.variants.filter((_, j) => j !== i))}
                        >
                          {t("common.remove")}
                          <span class="sr-only">: {name()}</span>
                        </Button>
                      </td>
                    </tr>
                  );
                }}
              </Index>
            </tbody>
          </table>
        </div>
      </Show>
    </div>
  );
}
