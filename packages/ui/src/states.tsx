import { Progress } from "@kobalte/core/progress";
import { For, type JSX, Show } from "solid-js";
import type { Tone } from "./badge.tsx";
import { Icon, type IconName } from "./icon.tsx";

/** Pajamas loading icon; decorative, pair it with visible text or `aria-busy` on the owner. */
export function Spinner(props: { size?: "sm" | "md" }) {
  const size = () => (props.size === "sm" ? "size-3.5" : "size-5");
  return (
    <span
      aria-hidden="true"
      class={`${size()} inline-block shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent opacity-80`}
    />
  );
}

/** A region that announces loading politely. */
export function LoadingState(props: { label: string }) {
  return (
    <div
      role="status"
      class="flex items-center justify-center gap-2 px-1 py-10 text-sm text-muted-foreground"
      aria-live="polite"
    >
      <Spinner />
      {props.label}
    </div>
  );
}

/** Pajamas skeleton loader: grey bars in the shape of the content that is loading. */
export function Skeleton(props: { lines?: number; class?: string }) {
  return (
    <div aria-hidden="true" class={`flex animate-pulse flex-col gap-2 ${props.class ?? ""}`}>
      <For each={Array.from({ length: props.lines ?? 3 }, (_, i) => i)}>
        {(i) => (
          <span
            class="h-3 rounded-sm bg-neutral-50"
            style={{ width: i === (props.lines ?? 3) - 1 ? "60%" : "100%" }}
          />
        )}
      </For>
    </div>
  );
}

const progressTones: Record<Tone, string> = {
  success: "bg-success-600",
  warning: "bg-warning-600",
  error: "bg-error-600",
  info: "bg-info-600",
  neutral: "bg-primary",
};

/** Pajamas progress bar (determinate), labelled for assistive tech. */
export function ProgressBar(props: {
  value: number;
  max?: number;
  label: string;
  showLabel?: boolean;
  tone?: Tone;
}) {
  return (
    <Progress value={props.value} maxValue={props.max ?? 100} class="flex flex-col gap-1">
      <div class="flex justify-between text-sm" classList={{ "sr-only": !props.showLabel }}>
        <Progress.Label>{props.label}</Progress.Label>
        <Progress.ValueLabel class="figures text-muted-foreground" />
      </div>
      <Progress.Track class="h-1 overflow-hidden rounded-full bg-neutral-50">
        <Progress.Fill
          class={`h-full w-(--kb-progress-fill-width) rounded-full ${progressTones[props.tone ?? "neutral"]}`}
        />
      </Progress.Track>
    </Progress>
  );
}

const alertTones: Record<Tone, { box: string; icon: IconName; iconClass: string; title: string }> =
  {
    info: {
      box: "border-info-200 bg-info-50",
      icon: "information-o",
      iconClass: "text-info-600",
      title: "text-info-700",
    },
    success: {
      box: "border-success-200 bg-success-50",
      icon: "check-circle",
      iconClass: "text-success-600",
      title: "text-success-700",
    },
    warning: {
      box: "border-warning-200 bg-warning-50",
      icon: "warning",
      iconClass: "text-warning-600",
      title: "text-warning-700",
    },
    error: {
      box: "border-error-200 bg-error-50",
      icon: "error",
      iconClass: "text-error-600",
      title: "text-error-700",
    },
    neutral: {
      box: "border-neutral-200 bg-subtle",
      icon: "information-o",
      iconClass: "text-neutral-600",
      title: "text-heading",
    },
  };

export interface AlertProps {
  tone?: Tone;
  title?: string;
  children?: JSX.Element;
  /** Buttons under the message (primary action first). */
  actions?: JSX.Element;
  /** Localised label of the dismiss (X) button; shown only together with `onDismiss`. */
  dismissLabel?: string;
  onDismiss?: () => void;
  class?: string;
}

/**
 * Pajamas inline alert. Errors and warnings are announced (`role="alert"`), the rest politely
 * (`role="status"`).
 */
export function Alert(props: AlertProps) {
  const tone = () => alertTones[props.tone ?? "info"];
  return (
    <div
      role={props.tone === "error" || props.tone === "warning" ? "alert" : "status"}
      class={`relative flex gap-3 rounded-md border py-3 pr-10 pl-3 text-sm text-foreground ${tone().box} ${props.class ?? ""}`}
      classList={{ "pr-3": !props.onDismiss }}
    >
      <Icon name={tone().icon} class={`mt-0.5 ${tone().iconClass}`} />
      <div class="flex min-w-0 flex-1 flex-col gap-1">
        <Show when={props.title}>
          <p class={`font-semibold ${tone().title}`}>{props.title}</p>
        </Show>
        <Show when={props.children}>
          <div class="break-words">{props.children}</div>
        </Show>
        <Show when={props.actions}>
          <div class="flex flex-wrap gap-2 pt-2">{props.actions}</div>
        </Show>
      </div>
      <Show when={props.onDismiss && props.dismissLabel}>
        <button
          type="button"
          aria-label={props.dismissLabel}
          class="absolute top-2 right-2 grid size-control-sm place-items-center rounded-sm text-muted-foreground hover:bg-muted"
          onClick={() => props.onDismiss?.()}
        >
          <Icon name="close" />
        </button>
      </Show>
    </div>
  );
}

interface StateProps {
  title: string;
  description?: string;
  action?: JSX.Element;
}

/** Pajamas empty state: centred title, one sentence on what to do next, one action. */
export function EmptyState(props: StateProps & { icon?: IconName }) {
  return (
    <div class="flex flex-col items-center gap-2 px-4 py-10 text-center">
      <Show when={props.icon}>
        {(name) => (
          <span class="mb-2 grid size-12 place-items-center rounded-full bg-subtle text-muted-foreground">
            <Icon name={name()} size={24} />
          </span>
        )}
      </Show>
      <p class="text-base font-semibold text-heading">{props.title}</p>
      <Show when={props.description}>
        <p class="max-w-prose text-sm text-muted-foreground">{props.description}</p>
      </Show>
      <Show when={props.action}>
        <div class="flex flex-wrap justify-center gap-2 pt-2">{props.action}</div>
      </Show>
    </div>
  );
}

/** A failed load or action, announced to assistive tech (danger alert with a retry action). */
export function ErrorState(props: StateProps) {
  return (
    <Alert tone="error" title={props.title} actions={props.action}>
      {props.description}
    </Alert>
  );
}

/** The signed-in member lacks the role for this screen. */
export function PermissionDenied(props: StateProps) {
  return <EmptyState icon="lock" {...props} />;
}
