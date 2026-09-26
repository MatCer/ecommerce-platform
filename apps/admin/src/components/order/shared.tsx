import { Badge, Button, Card } from "@platform/ui";
import { useQueryClient } from "@tanstack/solid-query";
import { createSignal, type JSX, onCleanup } from "solid-js";
import { t } from "../../i18n/index.ts";
import { ApiError } from "../../lib/api.ts";
import { safeDownloadUrl } from "../../lib/fulfillment.ts";
import { tenantKey } from "../../lib/me.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";

/** An order-page block: a Pajamas card (use `padding="none"` around full-bleed tables). */
export function Section(props: {
  title: string;
  children: JSX.Element;
  padding?: "none" | "normal";
  actions?: JSX.Element;
}) {
  return (
    <Card title={props.title} padding={props.padding} actions={props.actions}>
      {props.children}
    </Card>
  );
}

export function FulfillmentState(props: { value: string }) {
  const label = () => {
    const values = [
      "pending",
      "succeeded",
      "failed",
      "open",
      "refunded",
      "creating",
      "label_created",
      "shipped",
      "delivered",
      "returned",
      "cancelled",
    ] as const;
    const key = values.find((value) => value === props.value);
    return key ? t(`fulfillment.states.${key}`) : props.value;
  };
  return <Badge tone={props.value === "failed" ? "warning" : "neutral"}>{label()}</Badge>;
}

export function useFulfillmentRefresh() {
  const qc = useQueryClient();
  // Capture the tenant at component creation, so a late mutation cannot invalidate another shop.
  const keys = ["order", "orders", "withdrawals", "payment-exceptions", "inventory"].map((key) =>
    tenantKey(key),
  );
  return () => Promise.all(keys.map((queryKey) => qc.invalidateQueries({ queryKey })));
}

export function downloadTab(): Window {
  const tab = window.open("about:blank", "_blank");
  if (!tab) throw new ApiError(0, "popup_blocked");
  tab.opener = null;
  return tab;
}

export function showDownload(tab: Window, url: string) {
  const safe = safeDownloadUrl(url);
  if (!safe) throw new ApiError(0, "invalid_download_url");
  tab.location.replace(safe);
}

/** Fetch a new presigned URL on every click. It never enters the query cache or component state. */
export function DownloadButton(props: {
  label: string;
  disabled?: boolean;
  read: (signal: AbortSignal) => Promise<{ url: string }>;
}) {
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  let controller: AbortController | undefined;
  let tab: Window | undefined;
  onCleanup(() => {
    controller?.abort();
    if (pending()) tab?.close();
  });
  const download = async () => {
    if (pending()) return;
    setError(undefined);
    setPending(true);
    controller = new AbortController();
    try {
      tab = undefined;
      tab = downloadTab();
      const result = await props.read(controller.signal);
      controller.signal.throwIfAborted();
      showDownload(tab, result.url);
    } catch (error) {
      tab?.close();
      if (!controller.signal.aborted) setError(error);
    } finally {
      setPending(false);
    }
  };
  return (
    <div class="grid gap-1">
      <Button disabled={props.disabled} loading={pending()} onClick={() => void download()}>
        {props.label}
      </Button>
      <ApiProblem error={error()} />
    </div>
  );
}
