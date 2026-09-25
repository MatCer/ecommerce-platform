import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { CONSENT_OPEN, readConsent, saveConsent } from "./consent.ts";
import type { ConsentPurpose } from "./types.ts";
import "./consent-banner.css";

export interface ConsentBannerProps {
  /** Purposes the shop asks for (`ShopModel.consent.purposes`). */
  purposes: ConsentPurpose[];
  /** Cookie policy page (`ShopModel.consent.policy_url`). */
  policyUrl: string;
  /** `consent.*` and `legal.cookies` messages of the active locale. */
  labels: Record<string, string>;
}

/**
 * The platform consent banner (spec §11.3, A20). A platform component rather than theme code:
 * themes place it (`client:idle`) and it follows their tokens, but they cannot turn it into a
 * dark pattern. Accept and reject are equally prominent; nothing is preselected.
 *
 * It renders nothing on the server (cached HTML is the same for everyone) and appears after
 * hydration only while no choice exists. Any `[data-consent-settings]` element, or
 * `openConsentSettings()`, reopens it with the current choice. Fixed position: no layout shift.
 */
export default function ConsentBanner(props: ConsentBannerProps) {
  const l = (key: string) => props.labels[key] ?? key;
  const [open, setOpen] = createSignal(false);
  const [custom, setCustom] = createSignal(false);
  const [chosen, setChosen] = createSignal<ConsentPurpose[]>([]);
  let heading: HTMLHeadingElement | undefined;
  let returnTo: HTMLElement | null = null;

  const reopen = () => {
    returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setChosen(readConsent() ?? []);
    setCustom(true);
    setOpen(true);
    setTimeout(() => heading?.focus());
  };
  const close = () => {
    setOpen(false);
    setCustom(false);
    returnTo?.focus();
    returnTo = null;
  };
  const save = (purposes: ConsentPurpose[]) => {
    void saveConsent(purposes);
    close();
  };
  const onClick = (e: MouseEvent) => {
    if (e.target instanceof Element && e.target.closest("[data-consent-settings]")) {
      e.preventDefault();
      reopen();
    }
  };

  onMount(() => {
    setOpen(readConsent() === null);
    addEventListener(CONSENT_OPEN, reopen);
    document.addEventListener("click", onClick);
  });
  onCleanup(() => {
    removeEventListener(CONSENT_OPEN, reopen);
    document.removeEventListener("click", onClick);
  });

  return (
    <Show when={open()}>
      <section
        class="pf-consent"
        aria-labelledby="pf-consent-title"
        onKeyDown={(e) => {
          // Escape only dismisses a reopened banner; a first visit still needs a choice.
          if (e.key === "Escape" && readConsent() !== null) close();
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
                        e.currentTarget.checked
                          ? [...chosen(), p]
                          : chosen().filter((x) => x !== p),
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
    </Show>
  );
}
