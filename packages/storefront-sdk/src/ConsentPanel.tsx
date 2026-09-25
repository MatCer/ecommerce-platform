import { createSignal, For, onMount, Show } from "solid-js";
import { readConsent, saveConsent } from "./consent.ts";
import type { ConsentPurpose } from "./types.ts";

export interface ConsentPanelProps {
  purposes: ConsentPurpose[];
  policyUrl: string;
  textVersion: string;
  labels: Record<string, string>;
  /** Opened from "Cookie settings": the purpose checkboxes show at once, focus moves in. */
  settings: boolean;
  onClose: () => void;
}

/**
 * The banner's UI, loaded by `ConsentBanner` only when it has to show (no choice yet, or
 * reopened). Accept and reject are equally prominent; nothing is preselected on a first visit.
 */
export default function ConsentPanel(props: ConsentPanelProps) {
  const l = (key: string) => props.labels[key] ?? key;
  const [custom, setCustom] = createSignal(props.settings);
  const [chosen, setChosen] = createSignal<ConsentPurpose[]>(readConsent() ?? []);
  let heading: HTMLHeadingElement | undefined;
  onMount(() => props.settings && heading?.focus());
  const save = (purposes: ConsentPurpose[]) => {
    void saveConsent(purposes, { offered: props.purposes, textVersion: props.textVersion });
    props.onClose();
  };

  return (
    <section
      class="pf-consent"
      aria-labelledby="pf-consent-title"
      onKeyDown={(e) => {
        // Escape only dismisses a reopened banner; a first visit still needs a choice.
        if (e.key === "Escape" && readConsent() !== null) props.onClose();
      }}
    >
      <h2 id="pf-consent-title" ref={heading} tabIndex={-1} class="pf-consent__title">
        {l("consent.title")}
      </h2>
      <p class="pf-consent__text">
        {l("consent.text")} <a href={props.policyUrl}>{l("legal.cookies")}</a>
      </p>
      <Show when={custom()}>
        <fieldset class="pf-consent__purposes">
          <legend class="pf-consent__sr">{l("consent.settings")}</legend>
          <label>
            <input type="checkbox" checked disabled />
            {l("consent.necessary")}
          </label>
          <For each={props.purposes}>
            {(p) => (
              <label>
                <input
                  type="checkbox"
                  checked={chosen().includes(p)}
                  onChange={(e) =>
                    setChosen(
                      e.currentTarget.checked ? [...chosen(), p] : chosen().filter((x) => x !== p),
                    )
                  }
                />
                {l(`consent.${p}`)}
              </label>
            )}
          </For>
        </fieldset>
      </Show>
      <div class="pf-consent__actions">
        <button type="button" class="pf-consent__btn" onClick={() => save([])}>
          {l("consent.reject")}
        </button>
        <button type="button" class="pf-consent__btn" onClick={() => save(props.purposes)}>
          {l("consent.accept")}
        </button>
        <button
          type="button"
          class="pf-consent__link"
          onClick={() => (custom() ? save(chosen()) : setCustom(true))}
        >
          {custom() ? l("consent.save") : l("consent.settings")}
        </button>
      </div>
    </section>
  );
}
