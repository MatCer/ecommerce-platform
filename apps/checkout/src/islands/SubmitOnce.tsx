import { onMount } from "solid-js";

export default function SubmitOnce() {
  onMount(() => {
    let sent = new WeakSet<HTMLFormElement>();
    for (const form of document.querySelectorAll<HTMLFormElement>("form[data-details]"))
      form.addEventListener("submit", (e) => {
        if (sent.has(form)) e.preventDefault();
        else sent.add(form);
      });
    addEventListener("pageshow", (e) => {
      if (e.persisted) sent = new WeakSet();
    });
  });
  return null;
}
