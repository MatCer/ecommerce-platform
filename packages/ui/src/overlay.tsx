import { Dialog as KDialog } from "@kobalte/core/dialog";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import { Tabs as KTabs } from "@kobalte/core/tabs";
import { Toast, toaster } from "@kobalte/core/toast";
import { For, type JSX, Show } from "solid-js";
import { Portal } from "solid-js/web";
import type { Tone } from "./badge.tsx";
import { Button } from "./button.tsx";

export interface DialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  children: JSX.Element;
  /** Footer actions (buttons). */
  footer?: JSX.Element;
  size?: "sm" | "md";
}

/** Modal dialog: focus trap, Escape closes, labelled by its title (Kobalte Dialog). */
export function Dialog(props: DialogProps) {
  return (
    <KDialog open={props.open} onOpenChange={props.onOpenChange}>
      <KDialog.Portal>
        <KDialog.Overlay class="fixed inset-0 z-40 bg-black/40" />
        <div class="fixed inset-0 z-50 grid place-items-center overflow-y-auto p-4">
          <KDialog.Content
            class="flex w-full flex-col gap-4 rounded-lg border border-border bg-card p-5 shadow-overlay"
            classList={{ "max-w-sm": props.size === "sm", "max-w-lg": props.size !== "sm" }}
          >
            <div class="flex flex-col gap-1">
              <KDialog.Title class="text-base font-semibold">{props.title}</KDialog.Title>
              <Show when={props.description}>
                <KDialog.Description class="text-sm text-muted-foreground">
                  {props.description}
                </KDialog.Description>
              </Show>
            </div>
            {props.children}
            <Show when={props.footer}>
              <div class="flex flex-wrap justify-end gap-2">{props.footer}</div>
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
            variant={props.danger ? "danger" : "primary"}
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

const toastTones: Record<Tone, string> = {
  success: "border-l-success-600",
  error: "border-l-error-600",
  warning: "border-l-warning-700",
  info: "border-l-accent-600",
  neutral: "border-l-border-strong",
};

/** Place once near the app root. */
export function ToastRegion(props: { label?: string }) {
  return (
    <Portal>
      <Toast.Region aria-label={props.label}>
        <Toast.List
          as="div"
          class="fixed right-4 bottom-4 z-[60] flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2 outline-none"
        />
      </Toast.Region>
    </Portal>
  );
}

/** Shows a toast; errors stay until dismissed, everything else for 5 s. */
export function showToast(opts: {
  title: string;
  description?: string;
  tone?: Tone;
  closeLabel: string;
}): number {
  const tone = opts.tone ?? "success";
  return toaster.show((p) => (
    <Toast
      as="div"
      toastId={p.toastId}
      priority={tone === "error" ? "high" : "low"}
      persistent={tone === "error"}
      duration={5000}
      class={`flex items-start gap-3 rounded-md border border-l-4 border-border bg-card p-3 shadow-overlay ${toastTones[tone]}`}
    >
      <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <Toast.Title class="text-sm font-semibold">{opts.title}</Toast.Title>
        <Show when={opts.description}>
          <Toast.Description class="text-xs text-muted-foreground">
            {opts.description}
          </Toast.Description>
        </Show>
      </div>
      <Toast.CloseButton
        class="grid size-6 place-items-center rounded-sm text-muted-foreground hover:bg-muted"
        aria-label={opts.closeLabel}
      >
        <span aria-hidden="true">×</span>
      </Toast.CloseButton>
    </Toast>
  ));
}

export interface TabItem {
  value: string;
  label: JSX.Element;
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
    <KTabs value={props.value} onChange={props.onChange} class="flex flex-col gap-3">
      <KTabs.List aria-label={props.label} class="relative flex gap-1 border-b border-border">
        <For each={props.items}>
          {(item) => (
            <KTabs.Trigger
              value={item.value}
              class="-mb-px h-control border-b-2 border-transparent px-3 text-sm text-muted-foreground hover:text-foreground
                data-[selected]:border-accent-600 data-[selected]:font-medium data-[selected]:text-foreground"
            >
              {item.label}
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
}

/** A button that opens a menu of actions (arrow keys, typeahead, Escape). */
export function Menu(props: {
  trigger: JSX.Element;
  triggerLabel: string;
  items: readonly MenuItem[];
  header?: JSX.Element;
}) {
  return (
    <DropdownMenu>
      <DropdownMenu.Trigger
        aria-label={props.triggerLabel}
        class="inline-flex h-control max-w-full items-center gap-2 rounded-md px-2 text-sm hover:bg-muted"
      >
        {props.trigger}
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content class="z-50 min-w-48 rounded-md border border-border bg-card p-1 shadow-overlay outline-none">
          <Show when={props.header}>
            <div class="px-2 py-1.5 text-xs text-muted-foreground">{props.header}</div>
            <DropdownMenu.Separator class="my-1 border-border" />
          </Show>
          <For each={props.items}>
            {(item) => (
              <DropdownMenu.Item
                disabled={item.disabled}
                onSelect={item.onSelect}
                class="flex h-control cursor-default items-center rounded-sm px-2 text-sm outline-none
                  data-[highlighted]:bg-muted data-[disabled]:opacity-55"
              >
                {item.label}
              </DropdownMenu.Item>
            )}
          </For>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu>
  );
}
