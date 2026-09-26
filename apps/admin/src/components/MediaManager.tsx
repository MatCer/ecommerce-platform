import { Badge, Button, SelectField, showToast, TextField, type Tone } from "@platform/ui";
import { createQuery } from "@tanstack/solid-query";
import { createSignal, For, Index, onCleanup, Show } from "solid-js";
import { contentLocales, errorMessage, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { mediaUrl } from "../lib/config.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES, move, type ProductMedia } from "../lib/product-form.ts";
import { ACCEPTED_TYPES, type Asset, checkFile, uploadImage } from "../lib/upload.ts";

const statusTone: Record<Asset["status"], Tone> = {
  pending: "neutral",
  processing: "info",
  ready: "success",
  failed: "error",
};

function thumbnail(asset: Asset): string | undefined {
  const sorted = [...asset.variants].sort((a, b) => a.width - b.width);
  const pick = sorted.find((v) => v.format === "webp") ?? sorted[0];
  return pick ? mediaUrl(pick.key) : undefined;
}

/** One asset's status, polled while the worker renders its variants. */
function useAsset(id: () => string) {
  return createQuery(() => ({
    queryKey: tenantKey("asset", id()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/assets/{id}", {
          params: { header: tenantHeader(), path: { id: id() } },
        }),
      ),
    refetchInterval: (q) => {
      const s = q.state.data?.status;
      return s === "pending" || s === "processing" ? 1500 : false;
    },
  }));
}

function MediaRow(props: {
  media: ProductMedia;
  index: number;
  count: number;
  skus: string[];
  onChange: (m: ProductMedia) => void;
  onMove: (delta: number) => void;
  onRemove: () => void;
}) {
  const asset = useAsset(() => props.media.asset_id);
  const label = () => t("editor.image", { n: props.index + 1 });
  const statusLabel = (s: Asset["status"]) =>
    ({
      pending: t("editor.assetPending"),
      processing: t("editor.assetProcessing"),
      ready: t("editor.assetReady"),
      failed: t("editor.assetFailed"),
    })[s];
  return (
    <li class="flex flex-wrap items-start gap-3 border-b border-border py-3" aria-label={label()}>
      <div class="grid size-24 shrink-0 place-items-center overflow-hidden rounded-md border border-border bg-muted">
        <Show
          when={asset.data?.status === "ready" && asset.data}
          fallback={<span class="text-xs text-muted-foreground">{label()}</span>}
        >
          {(a) => (
            <img
              src={thumbnail(a())}
              alt={props.media.alt_i18n?.[contentLocales()[0] ?? "cs"] || label()}
              class="size-full object-cover"
              loading="lazy"
            />
          )}
        </Show>
      </div>
      <div class="flex min-w-0 flex-1 flex-col gap-2">
        <div class="flex flex-wrap items-center gap-2">
          <span class="text-sm font-medium">{label()}</span>
          <Show when={asset.data}>
            {(a) => (
              <>
                <Badge tone={statusTone[a().status]}>{statusLabel(a().status)}</Badge>
                <span class="truncate text-xs text-faint-foreground">{a().filename}</span>
              </>
            )}
          </Show>
          <Show when={asset.data?.error}>
            <span class="text-xs text-error-700">{asset.data?.error}</span>
          </Show>
        </div>
        <div class="grid gap-2 sm:grid-cols-3">
          <For each={CONTENT_LOCALES}>
            {(l) => (
              <TextField
                label={`${t("editor.altText")} (${l})`}
                value={props.media.alt_i18n?.[l] ?? ""}
                maxLength={500}
                onChange={(v) =>
                  props.onChange({ ...props.media, alt_i18n: { ...props.media.alt_i18n, [l]: v } })
                }
              />
            )}
          </For>
        </div>
        <Show when={props.skus.length > 1}>
          <SelectField
            class="max-w-xs"
            label={t("editor.appliesTo")}
            value={props.media.variant_sku ?? ""}
            options={[
              { value: "", label: t("editor.wholeProduct") },
              ...props.skus.map((s) => ({ value: s, label: s })),
            ]}
            onChange={(v) => props.onChange({ ...props.media, variant_sku: v || null })}
          />
        </Show>
      </div>
      <div class="flex gap-1">
        <Button
          category="tertiary"
          aria-label={`${t("common.moveUp")}: ${label()}`}
          disabled={props.index === 0}
          onClick={() => props.onMove(-1)}
        >
          ↑
        </Button>
        <Button
          category="tertiary"
          aria-label={`${t("common.moveDown")}: ${label()}`}
          disabled={props.index === props.count - 1}
          onClick={() => props.onMove(1)}
        >
          ↓
        </Button>
        <Button category="tertiary" onClick={props.onRemove}>
          {t("common.remove")}
          <span class="sr-only">: {label()}</span>
        </Button>
      </div>
    </li>
  );
}

interface Uploading {
  id: string;
  name: string;
  progress: number;
}

/** Product images: presigned upload with progress, processing status, alt texts, ordering. */
export function MediaManager(props: {
  media: ProductMedia[];
  skus: string[];
  onChange: (media: ProductMedia[]) => void;
}) {
  const [uploads, setUploads] = createSignal<Uploading[]>([]);
  let input!: HTMLInputElement;

  const update = (id: string, progress: number) =>
    setUploads((list) => list.map((u) => (u.id === id ? { ...u, progress } : u)));

  let alive = true;
  onCleanup(() => {
    alive = false;
  });

  const onFiles = async (files: FileList | null) => {
    for (const file of Array.from(files ?? [])) {
      const check = checkFile(file);
      if (check !== "ok") {
        showToast({
          title: t("editor.uploadFailed", { name: file.name }),
          description: t(`errors.${check}`),
          tone: "error",
          closeLabel: t("common.close"),
        });
        continue;
      }
      const id = crypto.randomUUID();
      setUploads((list) => [...list, { id, name: file.name, progress: 0 }]);
      try {
        const asset = await uploadImage(file, (p) => update(id, p));
        if (alive) props.onChange([...props.media, { asset_id: asset.id, alt_i18n: {} }]);
      } catch (err) {
        if (alive)
          showToast({
            title: t("editor.uploadFailed", { name: file.name }),
            description: errorMessage(err),
            tone: "error",
            closeLabel: t("common.close"),
          });
      } finally {
        setUploads((list) => list.filter((u) => u.id !== id));
      }
    }
    input.value = "";
  };

  return (
    <div class="flex flex-col gap-2">
      <div class="flex flex-wrap items-center gap-3">
        <input
          ref={input}
          id="media-upload"
          type="file"
          multiple
          tabIndex={-1}
          aria-label={t("editor.upload")}
          accept={ACCEPTED_TYPES.join(",")}
          class="sr-only"
          onChange={(e) => void onFiles(e.currentTarget.files)}
        />
        <Button onClick={() => input.click()}>{t("editor.upload")}</Button>
        <span class="text-xs text-muted-foreground">{t("editor.uploadHint")}</span>
      </div>
      <For each={uploads()}>
        {(u) => (
          <div class="flex items-center gap-3 text-xs">
            <span class="w-48 truncate">{t("editor.uploading", { name: u.name })}</span>
            <progress
              class="h-1.5 w-48 accent-accent-600"
              max={100}
              value={Math.round(u.progress * 100)}
              aria-label={t("editor.uploading", { name: u.name })}
            />
            <span class="figures">{Math.round(u.progress * 100)} %</span>
          </div>
        )}
      </For>
      <Show
        when={props.media.length > 0}
        fallback={<p class="py-2 text-sm text-muted-foreground">{t("editor.noImages")}</p>}
      >
        <ol class="border-t border-border">
          <Index each={props.media}>
            {(m, i) => (
              <MediaRow
                media={m()}
                index={i}
                count={props.media.length}
                skus={props.skus}
                onChange={(next) => props.onChange(props.media.map((x, j) => (j === i ? next : x)))}
                onMove={(d) => props.onChange(move(props.media, i, d))}
                onRemove={() => props.onChange(props.media.filter((_, j) => j !== i))}
              />
            )}
          </Index>
        </ol>
      </Show>
    </div>
  );
}
