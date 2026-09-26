import { controlClass, FormGroup } from "@platform/ui";
import { createUniqueId } from "solid-js";

/** Labelled `datetime-local` input (local time; callers convert to UTC for the API). */
export function DateTimeField(props: {
  label: string;
  hint?: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
}) {
  const id = createUniqueId();
  return (
    <FormGroup label={props.label} for={id} description={props.hint}>
      <input
        id={id}
        type="datetime-local"
        class={`${controlClass} figures`}
        value={props.value}
        disabled={props.disabled}
        aria-describedby={props.hint ? `${id}-desc` : undefined}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      />
    </FormGroup>
  );
}
