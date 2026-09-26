import { createSignal, type JSX, onMount } from "solid-js";

/** SSR controls must not accept input before their island's handlers are attached. */
export default function HydratedControls(props: { children: JSX.Element; class: string }) {
  const [ready, setReady] = createSignal(false);
  onMount(() => setReady(true));
  return (
    <fieldset disabled={!ready()} class={`min-w-0 ${props.class}`}>
      {props.children}
    </fieldset>
  );
}
