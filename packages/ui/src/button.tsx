import { Button as KButton } from "@kobalte/core/button";
import { type ComponentProps, type JSX, Show, splitProps } from "solid-js";
import { Spinner } from "./states.tsx";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

const base =
  "inline-flex h-control shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-md px-3 text-sm font-medium " +
  "transition-colors duration-75 disabled:cursor-not-allowed disabled:opacity-55";

const variants: Record<ButtonVariant, string> = {
  primary: "bg-accent-600 text-white hover:bg-accent-700 dark:hover:bg-accent-600/85",
  secondary: "border border-input bg-card text-foreground hover:bg-muted",
  ghost: "text-foreground hover:bg-muted",
  danger: "bg-error-600 text-white hover:bg-error-700 dark:hover:bg-error-600/85",
};

/** Class list for button-looking elements (e.g. router links). */
export function buttonClass(variant: ButtonVariant = "secondary", extra = ""): string {
  return `${base} ${variants[variant]} ${extra}`;
}

export interface ButtonProps extends ComponentProps<"button"> {
  variant?: ButtonVariant;
  /** Shows a spinner and disables the button; the label stays for screen readers. */
  loading?: boolean;
  icon?: JSX.Element;
}

export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ["variant", "loading", "icon", "class", "children"]);
  return (
    <KButton
      type="button"
      {...rest}
      disabled={rest.disabled || local.loading}
      aria-busy={local.loading || undefined}
      class={buttonClass(local.variant, local.class)}
    >
      <Show when={local.loading} fallback={local.icon}>
        <Spinner size="sm" />
      </Show>
      {local.children}
    </KButton>
  );
}
