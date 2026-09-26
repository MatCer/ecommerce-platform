import { type JSX, Show } from "solid-js";

/**
 * Pajamas table styles for a plain `<table>`: subtle header row, 1px row rules, 40px rows.
 * Wrap in `<div class="overflow-x-auto">`; inside a `Card padding="none"` it goes edge to edge.
 */
export const tableClass =
  "w-full border-collapse text-sm [&_tbody_tr]:border-b [&_tbody_tr]:border-border [&_tbody_tr:last-child]:border-b-0 " +
  "[&_thead_tr]:border-b [&_thead_tr]:border-border [&_thead]:bg-subtle";
export const tdClass = "h-row px-3 py-2 align-middle";

/** Table header cell. */
export function Th(props: { children?: JSX.Element; class?: string; srOnly?: boolean }) {
  return (
    <th
      scope="col"
      class={`h-10 px-3 text-left align-middle text-sm font-semibold whitespace-nowrap text-heading ${props.class ?? ""}`}
    >
      <Show when={props.srOnly} fallback={props.children}>
        <span class="sr-only">{props.children}</span>
      </Show>
    </th>
  );
}
