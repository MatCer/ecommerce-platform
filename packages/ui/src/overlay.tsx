import { Dialog as KDialog } from "@kobalte/core/dialog";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import { Tabs as KTabs } from "@kobalte/core/tabs";
import { Toast, toaster } from "@kobalte/core/toast";
import { Tooltip as KTooltip } from "@kobalte/core/tooltip";
import { For, type JSX, Show, splitProps } from "solid-js";
import { Portal } from "solid-js/web";
import type { Tone } from "./badge.tsx";
import { Button, type ButtonProps } from "./button.tsx";
import { Icon, type IconName } from "./icon.tsx";

const closeButtonClass =
  "grid size-control shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground";

export interface DialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children: JSX.Element;
  /** Footer actions (buttons): cancel first, the confirm/danger action last. */
  footer?: JSX.Element;
  size?: "sm" | "md";
  /** Localised label of the header's close (X) button; the button is omitted without it. */
  closeLabel?: string;
}

/** Pajamas modal: focus trap, Escape closes, labelled by its title (Kobalte Dialog). */
export function Dialog(props: DialogProps) {
  return (
    <KDialog open={props.open} onOpenChange={props.onOpenChange}>
      <KDialog.Portal>
        <KDialog.Overlay class="fixed inset-0 z-40 bg-overlay" />
        <div class="fixed inset-0 z-50 grid place-items-center overflow-y-auto p-4">
          <KDialog.Content
            class="flex w-full flex-col rounded-xl bg-card shadow-overlay"
            classList={{ "max-w-lg": props.size === "sm", "max-w-2xl": props.size !== "sm" }}
          >
            <div class="flex items-start gap-2 px-4 pt-4 pb-2">
              <div class="flex min-w-0 flex-1 flex-col gap-1 pt-1">
                <KDialog.Title class="text-base font-semibold text-heading">
                  {props.title}
                </KDialog.Title>
                <Show when={props.description}>
                  <KDialog.Description class="text-sm text-muted-foreground">
                    {props.description}
                  </KDialog.Description>
                </Show>
              </div>
              <Show when={props.closeLabel}>
                <KDialog.CloseButton aria-label={props.closeLabel} class={closeButtonClass}>
                  <Icon name="close" />
                </KDialog.CloseButton>
              </Show>
            </div>
            <div class="flex flex-col gap-4 px-4 py-2 empty:hidden">{props.children}</div>
            <Show when={props.footer}>
              <div class="flex flex-wrap justify-end gap-2 px-4 pt-2 pb-4">{props.footer}</div>
            </Show>
          </KDialog.Content>
        </div>
      </KDialog.Portal>
    </KDialog>
  );
}

export interface ConfirmProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  confirmLabel: string;
  cancelLabel: string;
  onConfirm: () => void;
  pending?: boolean;
  danger?: boolean;
}

export function ConfirmDialog(props: ConfirmProps) {
  return (
    <Dialog
      open={props.open}
      onOpenChange={props.onOpenChange}
      title={props.title}
      description={props.description}
      size="sm"
      footer={
        <>
          <Button onClick={() => props.onOpenChange(false)}>{props.cancelLabel}</Button>
          <Button
            variant={props.danger ? "danger" : "confirm"}
            loading={props.pending}
            onClick={() => props.onConfirm()}
          >
            {props.confirmLabel}
          </Button>
        </>
      }
    >
      {null}
    </Dialog>
  );
}

export interface DrawerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  /** Keep the title for assistive tech only (e.g. a navigation drawer). */
  hideTitle?: boolean;
  closeLabel: string;
  side?: "left" | "right";
  children: JSX.Element;
}

/** Pajamas drawer: a modal side panel for secondary content or small-screen navigation. */
export function Drawer(props: DrawerProps) {
  return (
    <KDialog open={props.open} onOpenChange={props.onOpenChange}>
      <KDialog.Portal>
        <KDialog.Overlay class="fixed inset-0 z-40 bg-overlay" />
        <KDialog.Content
          class="fixed inset-y-0 z-50 flex w-sidebar max-w-[calc(100vw-3rem)] flex-col bg-subtle shadow-overlay outline-none"
          classList={{ "left-0": props.side !== "right", "right-0": props.side === "right" }}
        >
          <div class="flex items-center justify-between gap-2 px-3 pt-2">
            <KDialog.Title
              class={props.hideTitle ? "sr-only" : "text-base font-semibold text-heading"}
            >
              {props.title}
            </KDialog.Title>
            <KDialog.CloseButton aria-label={props.closeLabel} class={closeButtonClass}>
              <Icon name="close" />
            </KDialog.CloseButton>
          </div>
          <div class="min-h-0 flex-1 overflow-y-auto">{props.children}</div>
        </KDialog.Content>
      </KDialog.Portal>
    </KDialog>
  );
}

const toastIcons: Partial<Record<Tone, { name: IconName; class: string }>> = {
  success: { name: "check-circle", class: "text-(--badge-success-bg)" },
  error: { name: "error", class: "text-(--badge-error-bg)" },
  warning: { name: "warning", class: "text-(--badge-warning-bg)" },
};

/** Place once near the app root. */
export function ToastRegion(props: { label?: string }) {
  return (
    <Portal>
      <Toast.Region aria-label={props.label}>
        <Toast.List
          as="div"
          class="fixed bottom-4 left-4 z-[60] flex w-96 max-w-[calc(100vw-2rem)] flex-col gap-2 outline-none"
        />
      </Toast.Region>
    </Portal>
  );
}

/** Pajamas toast (dark, bottom-left); errors stay until dismissed, everything else for 5 s. */
export function showToast(opts: {
  title: string;
  description?: string;
  tone?: Tone;
  closeLabel: string;
}): number {
  const tone = opts.tone ?? "success";
  const icon = toastIcons[tone];
  return toaster.show((p) => (
    <Toast
      as="div"
      toastId={p.toastId}
      priority={tone === "error" ? "high" : "low"}
      persistent={tone === "error"}
      duration={5000}
      class="flex items-start gap-3 rounded-md bg-toast py-3 pr-2 pl-4 text-toast-foreground shadow-overlay"
    >
      <Show when={icon}>
        {(i) => <Icon name={i().name} class={`mt-0.5 ${i().class}`} />}
      </Show>
      <div class="flex min-w-0 flex-1 flex-col gap-0.5 py-0.5">
        <Toast.Title class="text-sm">{opts.title}</Toast.Title>
        <Show when={opts.description}>
          <Toast.Description class="text-sm opacity-80">{opts.description}</Toast.Description>
        </Show>
      </div>
      <Toast.CloseButton
        class="grid size-control-sm shrink-0 place-items-center rounded-sm hover:bg-white/15"
        aria-label={opts.closeLabel}
      >
        <Icon name="close" />
      </Toast.CloseButton>
    </Toast>
  ));
}

export interface TabItem {
  value: string;
  label: JSX.Element;
  /** Pajamas tab counter badge. */
  count?: number;
  content: () => JSX.Element;
}

/** Keyboard-navigable tabs (arrow keys) with the panel labelled by its tab. */
export function Tabs(props: {
  items: readonly TabItem[];
  value?: string;
  onChange?: (value: string) => void;
  label: string;
}) {
  return (
    <KTabs value={props.value} onChange={props.onChange} class="flex flex-col gap-4">
      <KTabs.List
        aria-label={props.label}
        class="relative flex overflow-x-auto border-b border-border"
      >
        <For each={props.items}>
          {(item) => (
            <KTabs.Trigger
              value={item.value}
              class="-mb-px inline-flex h-10 shrink-0 items-center gap-2 border-b-2 border-transparent px-3 text-sm text-muted-foreground
                hover:border-border-strong hover:text-foreground data-[selected]:border-primary data-[selected]:font-semibold
                data-[selected]:text-heading"
            >
              {item.label}
              <Show when={item.count !== undefined}>
                <span class="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-neutral-50 px-1.5 text-xs font-normal text-muted-foreground">
                  {item.count}
                </span>
              </Show>
            </KTabs.Trigger>
          )}
        </For>
      </KTabs.List>
      <For each={props.items}>
        {(item) => <KTabs.Content value={item.value}>{item.content()}</KTabs.Content>}
      </For>
    </KTabs>
  );
}

export interface MenuItem {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  icon?: IconName;
  /** Destructive action (red text); keep it last. */
  danger?: boolean;
  /** Draw a divider above this item (groups). */
  separatorBefore?: boolean;
}

export const menuPanelClass =
  "z-50 min-w-48 max-w-80 rounded-md border border-border-strong bg-card p-1 shadow-overlay outline-none";
export const menuItemClass =
  "flex min-h-control cursor-default items-center gap-2 rounded-sm px-2 text-sm text-foreground outline-none " +
  "data-[highlighted]:bg-muted data-[disabled]:text-faint-foreground";

/** Pajamas disclosure dropdown: a button that opens a menu of actions (arrows, typeahead, Escape). */
export function Menu(props: {
  trigger: JSX.Element;
  triggerLabel: string;
  items: readonly MenuItem[];
  header?: JSX.Element;
  triggerClass?: string;
  placement?: "bottom-start" | "bottom-end" | "right-start" | "top-start";
}) {
  return (
    <DropdownMenu placement={props.placement ?? "bottom-end"} gutter={4}>
      <DropdownMenu.Trigger
        aria-label={props.triggerLabel}
        class={
          props.triggerClass ??
          "inline-flex h-control max-w-full items-center gap-2 rounded-md px-2 text-sm text-foreground hover:bg-muted"
        }
      >
        {props.trigger}
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content class={menuPanelClass}>
          <Show when={props.header}>
            <div class="px-2 py-2 text-sm text-muted-foreground">{props.header}</div>
            <DropdownMenu.Separator class="-mx-1 my-1 border-border" />
          </Show>
          <For each={props.items}>
            {(item) => (
              <>
                <Show when={item.separatorBefore}>
                  <DropdownMenu.Separator class="-mx-1 my-1 border-border" />
                </Show>
                <DropdownMenu.Item
                  disabled={item.disabled}
                  onSelect={item.onSelect}
                  class={`${menuItemClass} ${item.danger ? "text-error-700" : ""}`}
                >
                  <Show when={item.icon}>
                    {(name) => <Icon name={name()} class="text-muted-foreground" />}
                  </Show>
                  {item.label}
                </DropdownMenu.Item>
              </>
            )}
          </For>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu>
  );
}

/** A button with a Pajamas tooltip (hover and keyboard focus); mostly for icon-only buttons. */
export function TooltipButton(
  props: Omit<ButtonProps, "type"> & {
    tooltip: string;
    placement?: "top" | "bottom" | "right" | "left";
  },
) {
  const [local, rest] = splitProps(props, ["tooltip", "placement"]);
  return (
    <KTooltip placement={local.placement ?? "bottom"} openDelay={300} gutter={6}>
      <KTooltip.Trigger as={Button} {...rest} />
      <KTooltip.Portal>
        <KTooltip.Content class="z-[70] max-w-64 rounded-md bg-toast px-2 py-1 text-xs text-toast-foreground">
          {local.tooltip}
        </KTooltip.Content>
      </KTooltip.Portal>
    </KTooltip>
  );
}
