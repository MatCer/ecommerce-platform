import { onCleanup, onMount } from "solid-js";

/**
 * Behaviour for the popover sheets (mobile navigation, filters); renders nothing. A popover is
 * non-modal, so keyboard focus could wander behind an open sheet: when focus moves outside an
 * open sheet, the sheet closes. Escape and outside clicks are native popover behaviour.
 */
export default function Sheets() {
  onMount(() => {
    const onFocus = (e: FocusEvent) => {
      if (!(e.target instanceof Node)) return;
      for (const sheet of document.querySelectorAll<HTMLElement>("[popover].sheet:popover-open"))
        if (!sheet.contains(e.target)) sheet.hidePopover();
    };
    document.addEventListener("focusin", onFocus);
    onCleanup(() => document.removeEventListener("focusin", onFocus));
  });
  return null;
}
