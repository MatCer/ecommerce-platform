import { Button } from "@platform/ui";
import { createSignal } from "solid-js";
import { call } from "../lib/client";
import HydratedControls from "./HydratedControls";

export default function SignOut(props: { label: string }) {
  const [busy, setBusy] = createSignal(false);
  return (
    <HydratedControls class="inline-block">
      <Button
       
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
