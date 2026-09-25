import type { JSX } from "solid-js";
import { Show } from "solid-js";

/** Decorative spinner; pair it with visible text or `aria-busy` on the owner. */
export function Spinner(props: { size?: "sm" | "md" }) {
  const size = () => (props.size === "sm" ? "size-3.5" : "size-5");
  return (
    <span
      aria-hidden="true"
      class={`${size()} inline-block animate-spin rounded-full border-2 border-current border-r-transparent`}
    />
  );
}

/** A region that announces loading politely. */
export function LoadingState(props: { label: string }) {
  return (
    <div
      role="status"
      class="flex items-center gap-2 px-1 py-8 text-sm text-muted-foreground"
      aria-live="polite"
    >
      <Spinner />
      {props.label}
    </div>
  );
}

interface StateProps {
  title: string;
  description?: string;
  action?: JSX.Element;
}

function StateBlock(props: StateProps & { tone: "neutral" | "error"; role?: "alert" }) {
  return (
    <div
      role={props.role}
      class="flex flex-col items-start gap-2 border border-dashed border-border-strong px-4 py-6"
      classList={{ "border-error-600/50": props.tone === "error" }}
    >
      <p
        class="font-semibold"
        classList={{
          "text-foreground": props.tone === "neutral",
          "text-error-700": props.tone === "error",
        }}
      >
        {props.title}
      </p>
      <Show when={props.description}>
        <p class="max-w-prose text-muted-foreground">{props.description}</p>
      </Show>
      <Show when={props.action}>
        <div class="pt-1">{props.action}</div>
      </Show>
    </div>
  );
}

/** Empty collection: say what to do next, with one action. */
export function EmptyState(props: StateProps) {
  return <StateBlock tone="neutral" {...props} />;
}

/** A failed load or action, announced to assistive tech. */
export function ErrorState(props: StateProps) {
  return <StateBlock tone="error" role="alert" {...props} />;
}

/** The signed-in member lacks the role for this screen. */
export function PermissionDenied(props: StateProps) {
  return <StateBlock tone="neutral" {...props} />;
}
