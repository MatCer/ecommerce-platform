import { type Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import type { ConsentPanelProps } from "./ConsentPanel.tsx";
import { CONSENT_OPEN, readConsent } from "./consent.ts";
import type { ConsentPurpose } from "./types.ts";
// Static here, not in the lazy panel: a CSS dependency of a dynamic import needs Vite's
// preload runtime (~0.8 kB) on every page; the stylesheet itself is ~0.7 kB.
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
 * dark pattern.
 *
 * It renders nothing on the server (cached HTML is the same for everyone). After hydration it
 * loads its UI (`ConsentPanel`) only while no choice exists, or when reopened by any
 * `[data-consent-settings]` element or `openConsentSettings()`, so returning visitors never
 * download it. Fixed position: no layout shift.
 */
export default function ConsentBanner(props: ConsentBannerProps) {
  const [Panel, setPanel] = createSignal<Component<ConsentPanelProps>>();
  const [mode, setMode] = createSignal<"first" | "settings" | null>(null);
  let returnTo: HTMLElement | null = null;

  const show = (m: "first" | "settings") => {
    if (m === "settings")
      returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    void import("./ConsentPanel.tsx").then((mod) => {
      setPanel(() => mod.default);
      setMode(m);
    });
  };
  const close = () => {
    setMode(null);
    returnTo?.focus();
    returnTo = null;
  };

  // Browser-only setup and teardown both live in onMount: a top-level onCleanup also runs when
  // the server render is disposed, where `document` does not exist (it hangs the SSR stream).
  onMount(() => {
    if (readConsent() === null) show("first");
    const reopen = () => show("settings");
    const onClick = (e: MouseEvent) => {
      if (e.target instanceof Element && e.target.closest("[data-consent-settings]")) {
        e.preventDefault();
        reopen();
      }
    };
    addEventListener(CONSENT_OPEN, reopen);
    document.addEventListener("click", onClick);
    onCleanup(() => {
      removeEventListener(CONSENT_OPEN, reopen);
      document.removeEventListener("click", onClick);
    });
  });

  return (
    // Keyed by mode: reopening from "Cookie settings" while the first-visit banner shows
    // remounts the panel in settings mode (checkboxes visible, focus moved in).
    <Show when={Panel() && mode()} keyed>
      {(m) => {
        const Loaded = Panel();
        return Loaded && <Loaded {...props} settings={m === "settings"} onClose={close} />;
      }}
    </Show>
  );
}
