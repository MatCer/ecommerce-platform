import { Checkbox as KCheckbox } from "@kobalte/core/checkbox";
import { TextField as KTextField } from "@kobalte/core/text-field";
import { createUniqueId, For, type JSX, Show } from "solid-js";

export const controlClass =
  "h-control w-full rounded-md border border-input bg-card px-2.5 text-sm text-foreground " +
  "placeholder:text-faint-foreground disabled:cursor-not-allowed disabled:bg-muted disabled:text-muted-foreground " +
  "aria-[invalid=true]:border-error-600 data-[invalid]:border-error-600";

const labelClass = "text-xs font-medium text-muted-foreground";
const errorClass = "text-xs font-medium text-error-700";
const hintClass = "text-xs text-faint-foreground";

export interface TextFieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  description?: string;
  error?: string | false | undefined;
  type?: "text" | "email" | "password" | "number" | "url" | "search" | "tel";
  name?: string;
  required?: boolean;
  disabled?: boolean;
  readOnly?: boolean;
  multiline?: boolean;
  rows?: number;
  placeholder?: string;
  autocomplete?: string;
  inputMode?: "text" | "numeric" | "decimal" | "email" | "search" | "url";
  maxLength?: number;
  class?: string;
  inputClass?: string;
  /** Visually hide the label (it stays the accessible name). */
  hideLabel?: boolean;
  ref?: (el: HTMLInputElement) => void;
}

/** Labelled text input with description and error wired to the input (Kobalte TextField). */
export function TextField(props: TextFieldProps) {
  return (
    <KTextField
      class={`flex flex-col gap-1 ${props.class ?? ""}`}
      value={props.value}
      onChange={props.onChange}
      name={props.name}
      required={props.required}
      disabled={props.disabled}
      readOnly={props.readOnly}
      validationState={props.error ? "invalid" : "valid"}
    >
      <KTextField.Label class={props.hideLabel ? "sr-only" : labelClass}>
        {props.label}
        <Show when={props.required}>
          <span aria-hidden="true"> *</span>
        </Show>
      </KTextField.Label>
      <Show
        when={props.multiline}
        fallback={
          <KTextField.Input
            ref={props.ref}
            type={props.type ?? "text"}
            placeholder={props.placeholder}
            autocomplete={props.autocomplete}
            inputMode={props.inputMode}
            maxLength={props.maxLength}
            class={`${controlClass} ${props.inputClass ?? ""}`}
          />
        }
      >
        <KTextField.TextArea
          rows={props.rows ?? 3}
          placeholder={props.placeholder}
          maxLength={props.maxLength}
          class={`${controlClass} h-auto py-1.5 ${props.inputClass ?? ""}`}
        />
      </Show>
      <Show when={props.description}>
        <KTextField.Description class={hintClass}>{props.description}</KTextField.Description>
      </Show>
      <KTextField.ErrorMessage class={errorClass}>{props.error}</KTextField.ErrorMessage>
    </KTextField>
  );
}

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectFieldProps {
  label: string;
  value: string;
  options: readonly SelectOption[];
  onChange: (value: string) => void;
  description?: string;
  error?: string | false | undefined;
  disabled?: boolean;
  required?: boolean;
  hideLabel?: boolean;
  class?: string;
  name?: string;
}

/** Native select: the most robust accessible picker for short option lists. */
export function SelectField(props: SelectFieldProps) {
  const id = createUniqueId();
  const hint = () => (props.error ? `${id}-err` : props.description ? `${id}-desc` : undefined);
  return (
    <div class={`flex flex-col gap-1 ${props.class ?? ""}`}>
      <label for={id} class={props.hideLabel ? "sr-only" : labelClass}>
        {props.label}
      </label>
      <select
        id={id}
        name={props.name}
        class={`${controlClass} pr-7`}
        value={props.value}
        disabled={props.disabled}
        required={props.required}
        aria-invalid={props.error ? true : undefined}
        aria-describedby={hint()}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      >
        <For each={props.options}>
          {(o) => (
            <option value={o.value} selected={o.value === props.value}>
              {o.label}
            </option>
          )}
        </For>
      </select>
      <Show when={props.description && !props.error}>
        <p id={`${id}-desc`} class={hintClass}>
          {props.description}
        </p>
      </Show>
      <Show when={props.error}>
        <p id={`${id}-err`} class={errorClass}>
          {props.error}
        </p>
      </Show>
    </div>
  );
}

export interface CheckboxProps {
  label: JSX.Element;
  checked: boolean;
  onChange: (checked: boolean) => void;
  description?: string;
  disabled?: boolean;
  class?: string;
}

export function Checkbox(props: CheckboxProps) {
  return (
    <KCheckbox
      class={`flex items-start gap-2 ${props.class ?? ""}`}
      checked={props.checked}
      onChange={props.onChange}
      disabled={props.disabled}
    >
      <KCheckbox.Input class="peer" />
      <KCheckbox.Control
        class="mt-0.5 grid size-4 shrink-0 place-items-center rounded-sm border border-input bg-card text-white
          data-[checked]:border-accent-600 data-[checked]:bg-accent-600 peer-focus-visible:outline-2 peer-focus-visible:outline-ring
          data-[disabled]:opacity-55"
      >
        <KCheckbox.Indicator>
          <svg viewBox="0 0 12 12" class="size-3" aria-hidden="true">
            <path d="M2.5 6.5l2.5 2.5 4.5-5" fill="none" stroke="currentColor" stroke-width="1.8" />
          </svg>
        </KCheckbox.Indicator>
      </KCheckbox.Control>
      <div class="flex flex-col">
        <KCheckbox.Label class="text-sm text-foreground">{props.label}</KCheckbox.Label>
        <Show when={props.description}>
          <KCheckbox.Description class={hintClass}>{props.description}</KCheckbox.Description>
        </Show>
      </div>
    </KCheckbox>
  );
}

/** A titled group of fields (fieldset + legend). */
export function FieldGroup(props: { legend: string; description?: string; children: JSX.Element }) {
  return (
    <fieldset class="flex min-w-0 flex-col gap-3 border-t border-border pt-4">
      <legend class="float-left mb-1 w-full text-sm font-semibold text-foreground">
        {props.legend}
      </legend>
      <Show when={props.description}>
        <p class="-mt-2 text-xs text-muted-foreground">{props.description}</p>
      </Show>
      {props.children}
    </fieldset>
  );
}
