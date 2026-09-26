import { SegmentedControl as KSegmented } from "@kobalte/core/segmented-control";
import { For, type JSX, Show } from "solid-js";
import { Icon, type IconName } from "./icon.tsx";

/** Pajamas link colour; use on `<a>`/router `A` inside text and tables. */
export const linkClass = "text-accent-700 underline-offset-2 hover:underline";

/** Page title row: h1, optional description, actions on the right (Pajamas page heading). */
export function PageHeading(props: {
  title: string;
  description?: JSX.Element;
  actions?: JSX.Element;
  /** Content above the title (e.g. a back link). */
  before?: JSX.Element;
}) {
  return (
    <header class="mb-4 flex flex-wrap items-start justify-between gap-x-4 gap-y-3 pt-2">
      <div class="flex min-w-0 flex-col gap-1">
        {props.before}
        <h1 class="text-2xl font-semibold tracking-tight text-heading">{props.title}</h1>
        <Show when={props.description}>
          <p class="max-w-3xl text-sm text-muted-foreground">{props.description}</p>
        </Show>
      </div>
      <Show when={props.actions}>
        <div class="flex flex-wrap items-center gap-2">{props.actions}</div>
      </Show>
    </header>
  );
}

export interface CardProps {
  /** Heading text (rendered as h2). */
  title?: JSX.Element;
  /** Item count shown after the title (Pajamas CRUD component). */
  count?: number;
  countIcon?: IconName;
  description?: JSX.Element;
  /** Header actions, e.g. a small "Add" button. */
  actions?: JSX.Element;
  footer?: JSX.Element;
  children: JSX.Element;
  /** Body padding; use "none" when the body is a full-bleed table. */
  padding?: "none" | "normal";
  class?: string;
}

/**
 * Pajamas card / CRUD component: a bordered box with a subtle header (title, count, actions),
 * a white body and an optional footer.
 */
export function Card(props: CardProps) {
  const hasHeader = () => props.title !== undefined || props.actions !== undefined;
  return (
    <section
      class={`min-w-0 overflow-hidden rounded-lg border border-border bg-subtle ${props.class ?? ""}`}
    >
      <Show when={hasHeader()}>
        <div class="flex min-h-12 flex-wrap items-center justify-between gap-x-3 gap-y-2 px-4 py-2.5">
          <div class="flex min-w-0 flex-col gap-0.5">
            <div class="flex items-center gap-3">
              <h2 class="text-sm font-semibold text-heading">{props.title}</h2>
              <Show when={props.count !== undefined}>
                <span class="figures inline-flex items-center gap-1 text-sm text-muted-foreground">
                  <Show when={props.countIcon}>{(name) => <Icon name={name()} />}</Show>
                  {props.count}
                </span>
              </Show>
            </div>
            <Show when={props.description}>
              <p class="text-sm text-muted-foreground">{props.description}</p>
            </Show>
          </div>
          <Show when={props.actions}>
            <div class="flex flex-wrap items-center gap-2">{props.actions}</div>
          </Show>
        </div>
      </Show>
      <div
        class="bg-background"
        classList={{
          "p-4": props.padding !== "none",
          "border-t border-border": hasHeader(),
          "border-b border-border": props.footer !== undefined,
        }}
      >
        {props.children}
      </div>
      <Show when={props.footer}>
        <div class="flex flex-wrap items-center gap-2 px-4 py-3">{props.footer}</div>
      </Show>
    </section>
  );
}

export interface BreadcrumbItem {
  label: string;
  href?: string;
}

/** Pajamas breadcrumb; the last item is the current page. Plain links (the router intercepts). */
export function Breadcrumb(props: { label: string; items: readonly BreadcrumbItem[] }) {
  return (
    <nav aria-label={props.label} class="min-w-0">
      <ol class="flex min-w-0 items-center gap-1 text-sm">
        <For each={props.items}>
          {(item, i) => {
            const last = () => i() === props.items.length - 1;
            return (
              <li class="flex min-w-0 items-center gap-1">
                <Show when={i() > 0}>
                  <span aria-hidden="true" class="text-faint-foreground">
                    /
                  </span>
                </Show>
                <Show
                  when={item.href && !last()}
                  fallback={
                    <span
                      class="truncate"
                      classList={{ "text-heading": last(), "text-muted-foreground": !last() }}
                      aria-current={last() ? "page" : undefined}
                    >
                      {item.label}
                    </span>
                  }
                >
                  <a
                    href={item.href}
                    class="truncate rounded-sm text-muted-foreground hover:text-heading hover:underline"
                  >
                    {item.label}
                  </a>
                </Show>
              </li>
            );
          }}
        </For>
      </ol>
    </nav>
  );
}

const avatarHues = [
  "bg-info-50 text-info-700",
  "bg-success-50 text-success-700",
  "bg-warning-50 text-warning-700",
  "bg-error-50 text-error-700",
  "bg-neutral-50 text-neutral-700",
];

const avatarSizes = { 16: "size-4 text-[0.5rem]", 24: "size-6 text-xs", 32: "size-8 text-sm" };

/** Pajamas avatar: image, or the first letter on a hue derived from the name. Decorative. */
export function Avatar(props: {
  name: string;
  src?: string;
  size?: 16 | 24 | 32;
  shape?: "circle" | "square";
}) {
  const hue = () => {
    let h = 0;
    for (const ch of props.name) h = (h * 31 + (ch.codePointAt(0) ?? 0)) % 997;
    return avatarHues[h % avatarHues.length];
  };
  const box = () =>
    `${avatarSizes[props.size ?? 24]} ${props.shape === "square" ? "rounded-md" : "rounded-full"} shrink-0`;
  return (
    <Show
      when={props.src}
      fallback={
        <span
          aria-hidden="true"
          class={`${box()} ${hue()} grid place-items-center font-semibold uppercase`}
        >
          {props.name.trim().charAt(0) || "?"}
        </span>
      }
    >
      {(src) => <img src={src()} alt="" class={`${box()} object-cover`} />}
    </Show>
  );
}

/** Pajamas collapse: native disclosure (details/summary) with a chevron. */
export function Collapse(props: {
  summary: JSX.Element;
  children: JSX.Element;
  open?: boolean;
  class?: string;
}) {
  return (
    <details open={props.open} class={`group ${props.class ?? ""}`}>
      <summary class="flex min-h-control cursor-pointer list-none items-center gap-1 rounded-md text-sm font-semibold text-heading [&::-webkit-details-marker]:hidden">
        <Icon
          name="chevron-right"
          class="text-muted-foreground transition-transform group-open:rotate-90"
        />
        {props.summary}
      </summary>
      <div class="pt-2 pl-5">{props.children}</div>
    </details>
  );
}

export interface SegmentOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** Pajamas segmented control: pick one of 2-5 views/filters (a radio group underneath). */
export function SegmentedControl(props: {
  label: string;
  options: readonly SegmentOption[];
  value: string;
  onChange: (value: string) => void;
  hideLabel?: boolean;
}) {
  return (
    <KSegmented
      value={props.value}
      onChange={props.onChange}
      class="flex flex-col gap-2"
      orientation="horizontal"
    >
      <KSegmented.Label class={props.hideLabel ? "sr-only" : "text-sm font-semibold text-heading"}>
        {props.label}
      </KSegmented.Label>
      <div role="presentation" class="inline-flex w-fit rounded-md border border-border-strong p-0.5">
        <For each={props.options}>
          {(o) => (
            <KSegmented.Item value={o.value} disabled={o.disabled} class="relative">
              <KSegmented.ItemInput class="peer" />
              <KSegmented.ItemLabel
                class="flex h-7 cursor-pointer items-center rounded-sm px-3 text-sm text-foreground hover:bg-muted
                  peer-focus-visible:outline-2 peer-focus-visible:outline-ring data-[checked]:bg-primary
                  data-[checked]:text-primary-foreground data-[disabled]:cursor-not-allowed data-[disabled]:text-faint-foreground"
              >
                {o.label}
              </KSegmented.ItemLabel>
            </KSegmented.Item>
          )}
        </For>
      </div>
    </KSegmented>
  );
}
