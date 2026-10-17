import { onMount } from "solid-js";

export default function FreshOnRestore(props: { href: string }) {
  onMount(() => {
    addEventListener("pagehide", (e) => {
      if (e.persisted) document.body.inert = true;
    });
    addEventListener("pageshow", (e) => {
      if (!e.persisted) return;
      document.body.inert = true;
      location.replace(props.href);
    });
  });
  return null;
}
