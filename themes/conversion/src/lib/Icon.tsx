/** Icon for Solid islands; `d` is a path from `lib/icons.ts`. Decorative (aria-hidden). */
export default function Icon(props: { d: string; class?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={`shrink-0 ${props.class ?? "size-5"}`}
    >
      <path d={props.d} />
    </svg>
  );
}
