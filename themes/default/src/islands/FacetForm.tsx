import { onCleanup, onMount } from "solid-js";

/**
 * Behaviour for the server-rendered filter form (components/Facets.astro); renders nothing.
 * From 48rem a change submits at once and open dropdowns close on outside click or Escape.
 * In the phone sheet changes wait for "Show results", so the sheet does not reload per tap.
 * Hydrated with client:idle (a client:visible island with no box would never hydrate).
 */
export default function FacetForm(props: { form: string }) {
  onMount(() => {
    const form = document.getElementById(props.form);
    if (!(form instanceof HTMLFormElement)) return;
    const wide = matchMedia("(min-width: 48rem)");
    form.dataset.enhanced = "";
    const openDetails = () => [...form.querySelectorAll("details[open]")];
    const onChange = (e: Event) => {
      if (wide.matches || (e.target instanceof HTMLSelectElement && e.target.name === "sort"))
        form.requestSubmit();
    };
    const onClick = (e: MouseEvent) => {
      for (const d of openDetails()) if (!d.contains(e.target as Node)) d.removeAttribute("open");
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !wide.matches) return;
      for (const d of openDetails()) {
        d.removeAttribute("open");
        d.querySelector("summary")?.focus();
      }
    };
    form.addEventListener("change", onChange);
    document.addEventListener("click", onClick);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      form.removeEventListener("change", onChange);
      document.removeEventListener("click", onClick);
      document.removeEventListener("keydown", onKey);
    });
  });
  return null;
}
