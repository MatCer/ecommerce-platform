import { type Messages, t } from "@platform/storefront-sdk/format";
import { createSignal, Show } from "solid-js";

/** Newsletter sign-up (double opt-in happens server-side, spec §11.5). */
export default function Newsletter(props: { labels: Messages }) {
  const l = (key: string) => t(props.labels, key);
  const [state, setState] = createSignal<"idle" | "busy" | "done" | "error">("idle");

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const email = new FormData(e.currentTarget as HTMLFormElement).get("email");
    setState("busy");
    const res = await fetch("/_p/newsletter", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email }),
    }).catch(() => null);
    setState(res?.ok ? "done" : "error");
  }

  return (
    <Show
      when={state() !== "done"}
      fallback={
        <p role="status" class="font-semibold text-card">
          {l("newsletter.done")}
        </p>
      }
    >
      <form onSubmit={submit} class="flex max-w-sm flex-col gap-2 sm:flex-row">
        <label for="nl-email" class="sr-only">
          {l("newsletter.email")}
        </label>
        <input
          id="nl-email"
          name="email"
          type="email"
          required
          autocomplete="email"
          placeholder={l("newsletter.placeholder")}
          aria-invalid={state() === "error"}
          aria-describedby={state() === "error" ? "nl-error" : undefined}
          class="h-11 min-w-0 flex-1 rounded-md border border-white/25 bg-white/10 px-3 text-card placeholder:text-panel-foreground/70 focus-visible:outline-card"
        />
        <button
          type="submit"
          disabled={state() === "busy"}
          class="btn bg-card text-panel hover:bg-panel-foreground"
        >
          {l("newsletter.submit")}
        </button>
      </form>
      <Show when={state() === "error"}>
        <p id="nl-error" role="alert" class="mt-2 text-sm text-card">
          {l("newsletter.invalid")}
        </p>
      </Show>
    </Show>
  );
}
