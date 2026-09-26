import { Button as KButton } from "@kobalte/core/button";
import { type ComponentProps, type JSX, Show, splitProps } from "solid-js";
import { Icon, type IconName } from "./icon.tsx";
import { Spinner } from "./states.tsx";

/** Pajamas button variants: `confirm` for the one primary action, `danger` for destructive ones. */
export type ButtonVariant = "default" | "confirm" | "danger";
/** Emphasis: primary = filled, secondary = outlined, tertiary = no chrome until hover. */
export type ButtonCategory = "primary" | "secondary" | "tertiary";
export type ButtonSize = "small" | "medium";

export interface ButtonStyle {
  variant?: ButtonVariant;
  category?: ButtonCategory;
  size?: ButtonSize;
  /** Square button that only holds an icon (give it an `aria-label`). */
  iconOnly?: boolean;
  /** Full width. */
  block?: boolean;
}

// Filled/outlined buttons grey out when disabled; tertiary ones keep no chrome (only muted text).
const filledDisabled = "disabled:border-border disabled:bg-subtle";

// No colour transitions: state changes are instant (and axe never samples a half-faded colour).
const base =
  "inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md border text-sm " +
  "disabled:cursor-not-allowed disabled:text-faint-foreground aria-disabled:cursor-not-allowed aria-[pressed=true]:border-primary " +
  "aria-[pressed=true]:bg-primary aria-[pressed=true]:text-primary-foreground";

const styles: Record<ButtonVariant, Record<ButtonCategory, string>> = {
  default: {
    primary: `border-border-strong bg-card text-foreground hover:bg-subtle active:bg-neutral-50 ${filledDisabled}`,
    secondary: `border-border-strong bg-card text-foreground hover:bg-subtle active:bg-neutral-50 ${filledDisabled}`,
    tertiary:
      "border-transparent text-foreground not-disabled:hover:bg-muted not-disabled:active:bg-muted",
  },
  confirm: {
    primary: `border-primary bg-primary text-primary-foreground hover:border-primary-hover hover:bg-primary-hover ${filledDisabled}`,
    secondary: `border-primary bg-card text-foreground hover:bg-muted ${filledDisabled}`,
    tertiary: "border-transparent text-foreground not-disabled:hover:bg-muted",
  },
  danger: {
    primary: `border-error-600 bg-error-600 text-danger-foreground hover:brightness-95 ${filledDisabled}`,
    secondary: `border-error-600 bg-card text-error-700 hover:bg-error-50 ${filledDisabled}`,
    tertiary: "border-transparent text-error-700 not-disabled:hover:bg-error-50",
  },
};

/** Class list for button-looking elements (e.g. router links styled as buttons). */
export function buttonClass(style: ButtonStyle = {}, extra = ""): string {
  const small = style.size === "small";
  const size = style.iconOnly
    ? small
      ? "size-control-sm"
      : "size-control"
    : small
      ? "min-h-control-sm px-2"
      : "min-h-control px-3";
  return [
    base,
    styles[style.variant ?? "default"][style.category ?? "primary"],
    size,
    style.block ? "w-full" : "",
    extra,
  ].join(" ");
}

export interface ButtonProps extends ComponentProps<"button">, ButtonStyle {
  /** Shows a spinner and disables the button; the label stays for screen readers. */
  loading?: boolean;
  /** Leading Pajamas icon. */
  icon?: IconName;
  /** Toggle state (sets `aria-pressed`), e.g. in a button group used as a filter. */
  selected?: boolean;
}

export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, [
    "variant",
    "category",
    "size",
    "iconOnly",
    "block",
    "loading",
    "icon",
    "selected",
    "class",
    "children",
  ]);
  return (
    <KButton
      type="button"
      aria-pressed={local.selected}
      {...rest}
      disabled={rest.disabled || local.loading}
      aria-busy={local.loading || undefined}
      class={buttonClass(local, local.class)}
    >
      <Show
        when={local.loading}
        fallback={<Show when={local.icon}>{(name) => <Icon name={name()} />}</Show>}
      >
        <Spinner size="sm" />
      </Show>
      {local.children}
    </KButton>
  );
}

/** Joined buttons (Pajamas button group): related actions or a small toggle set. */
export function ButtonGroup(props: { label?: string; children: JSX.Element; class?: string }) {
  return (
    <fieldset
      aria-label={props.label}
      class={`inline-flex [&>*:not(:first-child)]:-ml-px [&>*:not(:first-child)]:rounded-l-none [&>*:not(:last-child)]:rounded-r-none [&>*:focus-visible]:z-10 ${props.class ?? ""}`}
    >
      {props.children}
    </fieldset>
  );
}
