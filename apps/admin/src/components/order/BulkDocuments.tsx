import { Alert, Button } from "@platform/ui";
import { createSignal, For, onCleanup, Show } from "solid-js";
import { locale, t } from "../../i18n/index.ts";
import { ApiError, api, tenantHeader, unwrap } from "../../lib/api.ts";
import { requestDocument } from "../../lib/documents.ts";
import { DocumentError, pollDocument } from "../../lib/fulfillment.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { downloadTab, showDownload, useFulfillmentRefresh } from "./shared.tsx";

export function BulkDocuments(props: { orders: { id: string; number: string }[] }) {
  const [pending, setPending] = createSignal(false);
  const [ready, setReady] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  const [failures, setFailures] = createSignal<{ number: string; error: string }[]>([]);
  const refresh = useFulfillmentRefresh();
  let controller: AbortController | undefined;
  let tab: Window | undefined;
  onCleanup(() => {
    controller?.abort();
    if (pending()) tab?.close();
  });
  const run = async (kind: "labels" | "packing_slips") => {
    if (pending() || props.orders.length < 1 || props.orders.length > 100) return;
    const orders = [...props.orders];
    setPending(true);
    setReady(false);
    setError(undefined);
    setFailures([]);
    controller = new AbortController();
    const signal = controller.signal;
    const timer = setTimeout(
      () => controller?.abort(new DocumentError("document_timeout")),
      60_000,
    );
    try {
      tab = undefined;
      tab = downloadTab();
      const header = tenantHeader();
      const requested = await requestDocument(
        { kind, order_ids: orders.map((o) => o.id), locale: locale() },
        signal,
      );
      setFailures(
        requested.failures.map((f) => ({
          number: orders.find((o) => o.id === f.order_id)?.number ?? f.order_id,
          error: f.error,
        })),
      );
      const doc = await pollDocument(
        () =>
          unwrap(
            api.GET("/admin/v1/documents/{id}", {
              params: { header, path: { id: requested.id } },
              signal,
            }),
          ),
        signal,
      );
      signal.throwIfAborted();
      if (!doc.url) throw new DocumentError("document_failed");
      showDownload(tab, doc.url);
      setReady(true);
    } catch (error) {
      tab?.close();
      if (signal.reason instanceof DocumentError) setError(new ApiError(0, signal.reason.code));
      else if (!signal.aborted)
        setError(
          error instanceof DocumentError ? new ApiError(0, error.code, error.detail) : error,
        );
    } finally {
      clearTimeout(timer);
      setPending(false);
      void refresh();
    }
  };
  return (
    <div class="mb-4 grid gap-2">
      <div class="flex flex-wrap items-center gap-2">
        <Button
          icon="download"
          disabled={!props.orders.length || props.orders.length > 100}
          loading={pending()}
          onClick={() => void run("labels")}
        >
          {t("fulfillment.printLabels")}
        </Button>
        <Button
          icon="download"
          disabled={!props.orders.length || props.orders.length > 100}
          loading={pending()}
          onClick={() => void run("packing_slips")}
        >
          {t("fulfillment.packingSlips")}
        </Button>
        <span class="figures text-sm text-muted-foreground">
          {t("fulfillment.selected", { count: props.orders.length })}
        </span>
      </div>
      <div role="status" aria-live="polite" class="text-sm text-muted-foreground">
        <Show when={pending()}>{t("fulfillment.generating")}</Show>
        <Show when={ready()}>{t("fulfillment.documentReady")}</Show>
      </div>
      <ApiProblem error={error()} />
      <Show when={failures().length}>
        <Alert tone="error">
          <ul>
            <For each={failures()}>
              {(failure) => (
                <li>
                  {failure.number}: {failure.error}
                </li>
              )}
            </For>
          </ul>
        </Alert>
      </Show>
    </div>
  );
}
