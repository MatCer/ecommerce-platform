import { createSignal } from "solid-js";
import { call } from "../lib/client";
import { Button } from "../ui.tsx";
import HydratedControls from "./HydratedControls";

export default function SignOut(props: { label: string }) {
  const [busy, setBusy] = createSignal(false);
  return (
    <HydratedControls class="inline-block">
      <Button
        variant="secondary"
        loading={busy()}
        onClick={async () => {
          setBusy(true);
          await call("POST", "/_p/account/logout", {});
          window.location.assign("/account");
        }}
      >
        {props.label}
      </Button>
    </HydratedControls>
  );
}
