import { Show } from "solid-js";

/** Labelled `datetime-local` input (local time; callers convert to UTC for the API). */
export function DateTimeField(props: {
  label: string;
  hint?: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
}) {
  return (
    <label class="flex flex-col gap-1 text-xs font-medium text-muted-foreground">
      {props.label}
      <input
        type="datetime-local"
        class="h-control rounded-md border border-input bg-card px-2.5 text-sm font-normal text-foreground"
        value={props.value}
        disabled={props.disabled}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      />
      <Show when={props.hint}>
        <span class="font-normal text-faint-foreground">{props.hint}</span>
      </Show>
    </label>
  );
}
