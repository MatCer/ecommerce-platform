import { createSignal, Show } from "solid-js";

/** Placeholder for the one-page checkout contact step (WP10): client-side validation only. */
export default function ContactStep() {
  const [error, setError] = createSignal("");
  return (
    <form
      class="grid gap-3"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        const email = new FormData(e.currentTarget).get("email");
        setError(
          typeof email === "string" && /.+@.+\..+/.test(email) ? "" : "Zadejte platný e-mail.",
        );
      }}
    >
      <label class="grid gap-1 text-sm font-semibold">
        E-mail
        <input
          name="email"
          type="email"
          autocomplete="email"
          aria-invalid={error() !== ""}
          aria-describedby="email-error"
          class="h-11 rounded-md border border-border bg-card px-3 font-normal"
        />
      </label>
      <Show when={error()}>
        <p id="email-error" role="alert" class="text-sm text-sale">
          {error()}
        </p>
      </Show>
      <button
        type="submit"
        class="h-12 rounded-md bg-buy font-display font-bold hover:bg-buy-hover"
      >
        Pokračovat
      </button>
    </form>
  );
}
