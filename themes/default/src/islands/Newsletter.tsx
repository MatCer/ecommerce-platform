import { createSignal, Show } from "solid-js";

/** Newsletter sign-up (double opt-in happens server-side, spec §11.5). */
export default function Newsletter() {
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
      fallback={<p role="status">Děkujeme! Potvrzovací odkaz jsme poslali na váš e-mail.</p>}
    >
      <form
        onSubmit={submit}
        class="flex max-w-md flex-col gap-2 sm:flex-row"
        method="post"
        action="/_p/newsletter"
      >
        <label for="nl-email" class="sr-only">
          E-mail
        </label>
        <input
          id="nl-email"
          name="email"
          type="email"
          required
          autocomplete="email"
          placeholder="vas@email.cz"
          class="h-11 flex-1 rounded-md border border-white/20 bg-white/10 px-3 text-card placeholder:text-card/60"
        />
        <button
          type="submit"
          disabled={state() === "busy"}
          class="h-11 rounded-md bg-card px-5 font-semibold text-panel"
        >
          Odebírat
        </button>
        <Show when={state() === "error"}>
          <p role="alert" class="text-sm">
            Přihlášení se nezdařilo, zkuste to znovu.
          </p>
        </Show>
      </form>
    </Show>
  );
}
