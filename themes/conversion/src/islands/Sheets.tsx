import { onCleanup, onMount } from "solid-js";

const OPEN_SHEETS = "[popover].sheet:popover-open, #search-sheet:popover-open";

/**
 * Behaviour for the popover sheets (mobile navigation, filters, phone search); renders nothing.
 * A popover is non-modal, so keyboard focus could wander behind an open sheet: when focus moves
 * outside an open sheet, the sheet closes. Escape and outside clicks are native popover
 * behaviour. The phone search sheet focuses its field when it opens (no `autofocus` attribute:
 * from 48rem the same field is the inline header search and would steal focus on load), also
 * when it was opened before this island hydrated, and it closes when the viewport grows into
 * the desktop layout (the top-layer copy would cover the header), keeping the field focused.
 * Stays `client:idle`: `client:load` cost ~75 ms of LCP on the product page (budget breach).
 */
export default function Sheets() {
  onMount(() => {
    const onFocus = (e: FocusEvent) => {
      if (!(e.target instanceof Node)) return;
      for (const sheet of document.querySelectorAll<HTMLElement>(OPEN_SHEETS))
        if (!sheet.contains(e.target)) sheet.hidePopover();
    };
    const onToggle = (e: Event) => {
      const el = e.target;
      if (el instanceof HTMLElement && el.id === "search-sheet" && el.matches(":popover-open"))
        el.querySelector("input")?.focus();
    };
    const search = document.getElementById("search-sheet");
    if (search?.matches(":popover-open")) search.querySelector("input")?.focus();
    const desktop = matchMedia("(min-width: 48rem)");
    const onResize = () => {
      if (!desktop.matches || !search?.matches(":popover-open")) return;
      const field = search.querySelector("input");
      const hadFocus = field !== null && document.activeElement === field;
      search.hidePopover();
      if (hadFocus) field.focus();
    };
    document.addEventListener("focusin", onFocus);
    // `toggle` does not bubble: listen in the capture phase.
    document.addEventListener("toggle", onToggle, true);
    desktop.addEventListener("change", onResize);
    onCleanup(() => {
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("toggle", onToggle, true);
      desktop.removeEventListener("change", onResize);
    });
  });
  return null;
}
