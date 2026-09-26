import { Alert, Button, fileInputClass, labelClass, ProgressBar, SelectField } from "@platform/ui";
import { createQuery } from "@tanstack/solid-query";
import { createSignal, createUniqueId, onCleanup, Show } from "solid-js";
import { t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { mediaUrl } from "../lib/config.ts";
import { contentError } from "../lib/content-api.ts";
import { tenantKey } from "../lib/me.ts";
import { ACCEPTED_TYPES, type Asset, checkFile, uploadImage } from "../lib/upload.ts";
import { QueryState } from "./Page.tsx";

export function ContentAsset(props: {
  value: string;
  onChange: (id: string) => void;
  label: string;
}) {
  const id = createUniqueId();
  const [busy, setBusy] = createSignal(false),
    [progress, setProgress] = createSignal(0),
    [error, setError] = createSignal<string>();
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  const assets = createQuery(() => ({
    queryKey: tenantKey("content-assets"),
    queryFn: async () => {
      const header = tenantHeader();
      const items: Asset[] = [];
      let cursor: string | undefined;
      do {
        const page = await unwrap(
          api.GET("/admin/v1/assets", { params: { header, query: { limit: 100, cursor } } }),
        );
        items.push(...page.items);
        if (page.next_cursor && page.next_cursor === cursor)
          throw new Error("Repeated asset cursor");
        cursor = page.next_cursor ?? undefined;
      } while (cursor);
      return { items };
    },
    refetchInterval: (query) =>
      query.state.data?.items.some((a) => a.status === "pending" || a.status === "processing")
        ? 2000
        : false,
  }));
  const upload = async (file: File | undefined) => {
    if (!file) return;
    if (checkFile(file) !== "ok") {
      setError(t("editor.uploadHint"));
      return;
    }
    setBusy(true);
    setError(undefined);
    setProgress(0);
    try {
      const asset = await uploadImage(file, setProgress);
      if (alive) {
        props.onChange(asset.id);
        await assets.refetch();
      }
    } catch (e) {
      if (alive) setError(contentError(e));
    } finally {
      if (alive) setBusy(false);
    }
  };
  return (
    <fieldset class="flex min-w-0 flex-col gap-2">
      <legend class={`mb-2 ${labelClass}`}>{props.label}</legend>
      <QueryState query={assets}>
        {(data) => (
          <SelectField
            label={t("content.asset")}
            value={props.value}
            options={[
              { value: "", label: t("common.none") },
              ...(!data.items.some((a) => a.id === props.value) && props.value
                ? [{ value: props.value, label: props.value }]
                : []),
              ...data.items
                .filter((a) => a.status !== "failed")
                .map((a) => ({
                  value: a.id,
                  label: `${a.filename ?? a.id} (${t(({ pending: "editor.assetPending", processing: "editor.assetProcessing", ready: "editor.assetReady", failed: "editor.assetFailed" } as const)[a.status])})`,
                })),
            ]}
            onChange={props.onChange}
            disabled={busy()}
          />
        )}
      </QueryState>
      <Show when={assets.data?.items.find((a) => a.id === props.value)?.variants[0]}>
        {(variant) => (
          <img
            src={mediaUrl(variant().key)}
            alt={props.label}
            class="max-h-48 max-w-full rounded-md border border-border object-contain"
          />
        )}
      </Show>
      <label for={id} class="text-sm text-muted-foreground">
        {t("content.upload")}
      </label>
      <input
        id={id}
        type="file"
        accept={ACCEPTED_TYPES.join(",")}
        disabled={busy()}
        onChange={(e) => {
          void upload(e.currentTarget.files?.[0]);
          e.currentTarget.value = "";
        }}
        class={fileInputClass}
      />
      <Show when={props.value}>
        <Button
          class="self-start"
          category="tertiary"
          size="small"
          icon="remove"
          onClick={() => props.onChange("")}
        >
          {t("common.remove")}
        </Button>
      </Show>
      <Show when={busy()}>
        <div role="status" class="max-w-sm">
          <ProgressBar value={Math.round(progress() * 100)} label={t("content.upload")} showLabel />
        </div>
      </Show>
      <Show when={error()}>
        <Alert tone="error">{error()}</Alert>
      </Show>
    </fieldset>
  );
}
