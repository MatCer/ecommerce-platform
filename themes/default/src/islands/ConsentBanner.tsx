import { readConsent, writeConsent } from "@platform/storefront-sdk/client";
import type { ConsentPurpose } from "@platform/storefront-sdk/types";
import { createSignal, For, onMount, Show } from "solid-js";

const LABELS: Record<ConsentPurpose, string> = {
  analytics: "Měření návštěvnosti",
  ads: "Reklama",
  personalization: "Personalizace",
  email_marketing: "E-mailový marketing",
  review_invites: "Žádosti o recenze",
};

/**
 * Consent banner (A20). Rendered hidden on the server so cached HTML is identical for everyone;
 * shown after hydration only when no choice is stored. Fixed position: no layout shift.
 */
export default function ConsentBanner(props: { purposes: ConsentPurpose[]; policyUrl: string }) {
  const [show, setShow] = createSignal(false);
  const [custom, setCustom] = createSignal(false);
  const [chosen, setChosen] = createSignal<ConsentPurpose[]>([]);
  onMount(() => setShow(readConsent() === null));
  const save = (p: ConsentPurpose[]) => {
    writeConsent(p);
    setShow(false);
  };

  return (
    <Show when={show()}>
      <section
        aria-label="Souhlas s cookies"
        class="fixed inset-x-2 bottom-2 z-30 mx-auto max-w-2xl rounded-xl border border-border bg-card p-4 text-sm shadow-xl md:bottom-4"
      >
        <p class="mb-3">
          Cookies pro měření a reklamu použijeme jen s vaším souhlasem.{" "}
          <a href={props.policyUrl} class="text-identity-ink underline">
            Více informací
          </a>
        </p>
        <Show when={custom()}>
          <div class="mb-3 grid gap-2">
            <For each={props.purposes}>
              {(p) => (
                <label class="flex items-center gap-2">
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
                  {LABELS[p]}
                </label>
              )}
            </For>
          </div>
        </Show>
        <div class="flex flex-wrap gap-2">
          <button
            type="button"
            class="h-10 rounded-md bg-identity px-4 font-semibold text-card"
            onClick={() => save(props.purposes)}
          >
            Přijmout vše
          </button>
          <button
            type="button"
            class="h-10 rounded-md border border-border px-4 font-semibold"
            onClick={() => save([])}
          >
            Odmítnout
          </button>
          <button
            type="button"
            class="h-10 rounded-md px-4 underline"
            onClick={() => (custom() ? save(chosen()) : setCustom(true))}
          >
            {custom() ? "Uložit výběr" : "Nastavení"}
          </button>
        </div>
      </section>
    </Show>
  );
}
