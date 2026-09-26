import { Checkbox as KCheckbox } from "@kobalte/core/checkbox";
import { Switch as KSwitch } from "@kobalte/core/switch";
import { TextField as KTextField } from "@kobalte/core/text-field";
import { type ComponentProps, createUniqueId, For, type JSX, Show, splitProps } from "solid-js";
import { Icon } from "./icon.tsx";

/** Pajamas form input: 32px, 8px radius, darker outline on hover, near-black on focus. */
export const controlClass =
  "h-control w-full rounded-md border border-input bg-control px-3 text-sm text-foreground " +
  "placeholder:text-faint-foreground hover:border-input-hover focus:border-heading " +
  "disabled:cursor-not-allowed disabled:border-border disabled:bg-subtle disabled:text-faint-foreground " +
  "aria-[invalid=true]:border-error-600 data-[invalid]:border-error-600";

export const labelClass = "text-sm font-semibold text-heading";
export const errorClass = "text-sm text-error-700";
export const hintClass = "text-sm text-muted-foreground";

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
      class={`flex flex-col gap-2 ${props.class ?? ""}`}
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
          class={`${controlClass} h-auto py-1.5 leading-5 ${props.inputClass ?? ""}`}
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
    <div class={`flex flex-col gap-2 ${props.class ?? ""}`}>
      <label for={id} class={props.hideLabel ? "sr-only" : labelClass}>
        {props.label}
      </label>
      <div class="relative">
      <select
        id={id}
        name={props.name}
        class={`${controlClass} appearance-none truncate pr-8`}
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
      <Icon
        name="chevron-down"
        class="pointer-events-none absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground"
      />
      </div>
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
        class="mt-0.5 grid size-4 shrink-0 place-items-center rounded-sm border border-input bg-control text-primary-foreground
          hover:border-input-hover data-[checked]:border-primary data-[checked]:bg-primary peer-focus-visible:outline-2
          peer-focus-visible:outline-offset-1 peer-focus-visible:outline-ring data-[disabled]:border-border data-[disabled]:bg-subtle"
      >
        <KCheckbox.Indicator>
          <Icon name="check" size={12} />
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

/** A titled group of fields (fieldset + legend), like a Pajamas settings section. */
export function FieldGroup(props: { legend: string; description?: string; children: JSX.Element }) {
  return (
    <fieldset class="flex min-w-0 flex-col gap-4 border-t border-border pt-5">
      <legend class="float-left w-full text-base font-semibold text-heading">{props.legend}</legend>
      <Show when={props.description}>
        <p class="-mt-3 text-sm text-muted-foreground">{props.description}</p>
      </Show>
      {props.children}
    </fieldset>
  );
}

export interface FormGroupProps {
  label: string;
  /** Id of the control the label points at (`for`). Use `useFormGroupIds` or your own id. */
  for: string;
  /** Help text under the control. */
  description?: string;
  /** Invalid feedback; replaces the help text. Give the control `aria-invalid`. */
  error?: string | false | undefined;
  /** Localised "(optional)" marker, shown after the label (Pajamas marks optional, not required). */
  optional?: string;
  hideLabel?: boolean;
  class?: string;
  children: JSX.Element;
}

/**
 * Label + control + help/error for controls without their own field wrapper (raw inputs,
 * pickers). Point the control's `aria-describedby` at `describedBy(id, props)`.
 */
export function FormGroup(props: FormGroupProps) {
  return (
    <div class={`flex flex-col gap-2 ${props.class ?? ""}`}>
      <label for={props.for} class={props.hideLabel ? "sr-only" : labelClass}>
        {props.label}
        <Show when={props.optional}>
          <span class="font-normal text-muted-foreground"> {props.optional}</span>
        </Show>
      </label>
      {props.children}
      <Show
        when={props.error}
        fallback={
          <Show when={props.description}>
            <p id={`${props.for}-desc`} class={hintClass}>
              {props.description}
            </p>
          </Show>
        }
      >
        <p id={`${props.for}-err`} class={errorClass}>
          {props.error}
        </p>
      </Show>
    </div>
  );
}

/** The `aria-describedby` value matching what `FormGroup` renders for that control id. */
export function describedBy(
  id: string,
  group: { description?: string; error?: string | false | undefined },
): string | undefined {
  if (group.error) return `${id}-err`;
  return group.description ? `${id}-desc` : undefined;
}

/** Native radio styled as a Pajamas radio (ring that fills with the primary colour). */
export const radioClass =
  "size-4 shrink-0 cursor-pointer appearance-none rounded-full border border-input bg-control hover:border-input-hover " +
  "checked:border-[5px] checked:border-primary disabled:cursor-not-allowed disabled:border-border disabled:bg-subtle";

/** One labelled native radio. Group several in a `<fieldset>` with a legend. */
export function Radio(
  props: Omit<ComponentProps<"input">, "type"> & { label: JSX.Element; description?: string },
) {
  const [local, rest] = splitProps(props, ["label", "description", "class"]);
  return (
    <label class={`flex items-start gap-2 text-sm text-foreground ${local.class ?? ""}`}>
      <input type="radio" {...rest} class={`mt-0.5 ${radioClass}`} />
      <span class="flex flex-col">
        {local.label}
        <Show when={local.description}>
          <span class={hintClass}>{local.description}</span>
        </Show>
      </span>
    </label>
  );
}

export interface ToggleProps {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  description?: string;
  disabled?: boolean;
  hideLabel?: boolean;
  class?: string;
}

/** Pajamas toggle: an on/off setting that applies immediately (use Checkbox inside forms). */
export function Toggle(props: ToggleProps) {
  return (
    <KSwitch
      class={`flex items-start gap-3 ${props.class ?? ""}`}
      checked={props.checked}
      onChange={props.onChange}
      disabled={props.disabled}
    >
      <KSwitch.Input class="peer" />
      <KSwitch.Control
        class="inline-flex h-6 w-10 shrink-0 items-center rounded-full border border-input bg-control p-0.5 transition-colors
          data-[checked]:border-primary data-[checked]:bg-primary peer-focus-visible:outline-2 peer-focus-visible:outline-offset-1
          peer-focus-visible:outline-ring data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50"
      >
        <KSwitch.Thumb
          class="grid size-4 place-items-center rounded-full bg-primary text-primary transition-transform
            data-[checked]:translate-x-4 data-[checked]:bg-primary-foreground"
        >
          <Show when={props.checked}>
            <Icon name="check" size={12} />
          </Show>
        </KSwitch.Thumb>
      </KSwitch.Control>
      <div class="flex flex-col" classList={{ "sr-only": props.hideLabel }}>
        <KSwitch.Label class="text-sm text-foreground">{props.label}</KSwitch.Label>
        <Show when={props.description}>
          <KSwitch.Description class={hintClass}>{props.description}</KSwitch.Description>
        </Show>
      </div>
    </KSwitch>
  );
}

export interface SearchBoxProps {
  /** Accessible name (kept visually hidden, the placeholder carries the hint). */
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  /** Localised label of the clear button. */
  clearLabel: string;
  class?: string;
}

/** Pajamas search box: magnifier, input, clear button. */
export function SearchBox(props: SearchBoxProps) {
  return (
    <KTextField class={`relative ${props.class ?? ""}`} value={props.value} onChange={props.onChange}>
      <KTextField.Label class="sr-only">{props.label}</KTextField.Label>
      <Icon
        name="search"
        class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground"
      />
      <KTextField.Input
        type="search"
        placeholder={props.placeholder}
        class={`${controlClass} px-9 [&::-webkit-search-cancel-button]:hidden`}
      />
      <Show when={props.value}>
        <button
          type="button"
          aria-label={props.clearLabel}
          class="absolute top-1/2 right-1 grid size-6 -translate-y-1/2 place-items-center rounded-sm text-muted-foreground hover:bg-muted"
          onClick={() => props.onChange("")}
        >
          <Icon name="clear" />
        </button>
      </Show>
    </KTextField>
  );
}
