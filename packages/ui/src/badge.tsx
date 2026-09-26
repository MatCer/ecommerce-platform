import { type JSX, Show } from "solid-js";
import { Icon, type IconName } from "./icon.tsx";

/** Status hues each carry one meaning (see packages/config/tailwind/theme.css). */
export type Tone = "success" | "warning" | "error" | "info" | "neutral";

const tones: Record<Tone, string> = {
  success: "bg-(--badge-success-bg) text-(--badge-success-fg)",
  warning: "bg-(--badge-warning-bg) text-(--badge-warning-fg)",
  error: "bg-(--badge-error-bg) text-(--badge-error-fg)",
  info: "bg-(--badge-info-bg) text-(--badge-info-fg)",
  neutral: "bg-(--badge-neutral-bg) text-(--badge-neutral-fg)",
};

/** Pajamas badge: a pill for a status or a count. Never interactive. */
export function Badge(props: { tone?: Tone; icon?: IconName; children?: JSX.Element }) {
  return (
    <span
      class={`inline-flex h-5 min-w-5 shrink-0 items-center justify-center gap-1 rounded-full px-1.5 text-xs whitespace-nowrap ${tones[props.tone ?? "neutral"]}`}
    >
      <Show when={props.icon}>{(name) => <Icon name={name()} size={12} />}</Show>
      {props.children}
    </span>
  );
}
