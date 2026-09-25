import type { JSX } from "solid-js";

/** Status hues each carry one meaning (see packages/config/tailwind/theme.css). */
export type Tone = "success" | "warning" | "error" | "info" | "neutral";

const tones: Record<Tone, string> = {
  success: "bg-success-50 text-success-700",
  warning: "bg-warning-50 text-warning-700",
  error: "bg-error-50 text-error-700",
  info: "bg-info-50 text-info-700",
  neutral: "bg-neutral-50 text-neutral-700",
};

export function Badge(props: { tone?: Tone; children: JSX.Element }) {
  return (
    <span
      class={`inline-flex h-5 items-center rounded-sm px-1.5 text-xs font-semibold ${tones[props.tone ?? "neutral"]}`}
    >
      {props.children}
    </span>
  );
}
