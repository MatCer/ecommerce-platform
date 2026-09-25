import { Button, Menu, SelectField, TextField } from "@platform/ui";
import { Index, Match, Show, Switch } from "solid-js";
import { t } from "../i18n/index.ts";
import { moveItem, removeItem, validContentHref } from "../lib/content-form.ts";
import {
  blankEmailBlock,
  blockValid,
  EMAIL_BLOCK_TYPES,
  type EmailBlock,
  MAX_BLOCKS,
  MAX_GRID,
} from "../lib/marketing.ts";
import { ContentAsset } from "./ContentAsset.tsx";
import { ProductPicker } from "./ProductPicker.tsx";
import { RichText } from "./RichText.tsx";

type Of<T extends EmailBlock["type"]> = Extract<EmailBlock, { type: T }>;

function hrefError(href: string | undefined) {
  return !!href && !validContentHref(href) && t("content.hrefHint");
}

function BlockFields(props: { block: EmailBlock; onChange: (block: EmailBlock) => void }) {
  return (
    <Switch>
      <Match when={props.block.type === "heading"}>
        {(() => {
          const b = () => props.block as Of<"heading">;
          return (
            <TextField
              label={t("content.headingText")}
              value={b().text}
              maxLength={200}
              onChange={(text) => props.onChange({ ...b(), text })}
            />
          );
        })()}
      </Match>
      <Match when={props.block.type === "text"}>
        {(() => {
          const b = () => props.block as Of<"text">;
          return (
            <RichText
              label={t("content.body")}
              value={b().html}
              onChange={(html) => props.onChange({ ...b(), html })}
            />
          );
        })()}
      </Match>
      <Match when={props.block.type === "image"}>
        {(() => {
          const b = () => props.block as Of<"image">;
          return (
            <>
              <ContentAsset
                label={t("content.image")}
                value={b().asset_id}
                onChange={(asset_id) => props.onChange({ ...b(), asset_id })}
              />
              <TextField
                label={t("content.alt")}
                value={b().alt ?? ""}
                maxLength={300}
                onChange={(alt) => props.onChange({ ...b(), alt })}
              />
              <TextField
                label={t("marketing.imageLink")}
                description={t("content.hrefHint")}
                error={hrefError(b().href)}
                value={b().href ?? ""}
                onChange={(href) => props.onChange({ ...b(), href })}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "button"}>
        {(() => {
          const b = () => props.block as Of<"button">;
          return (
            <>
              <TextField
                label={t("content.label")}
                value={b().label}
                maxLength={100}
                onChange={(label) => props.onChange({ ...b(), label })}
              />
              <TextField
                label={t("content.href")}
                description={t("content.hrefHint")}
                error={hrefError(b().href)}
                value={b().href}
                onChange={(href) => props.onChange({ ...b(), href })}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "product_grid"}>
        {(() => {
          const b = () => props.block as Of<"product_grid">;
          return (
            <>
              <TextField
                label={t("content.gridTitle")}
                value={b().title ?? ""}
                maxLength={200}
                onChange={(title) => props.onChange({ ...b(), title })}
              />
              <p class="text-xs">
                {t("marketing.gridLimit", { max: String(MAX_GRID) })} ({b().product_ids.length}/
                {MAX_GRID})
              </p>
              <ProductPicker
                value={b().product_ids}
                onChange={(product_ids) => {
                  if (product_ids.length <= MAX_GRID) props.onChange({ ...b(), product_ids });
                }}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "personalized_products"}>
        {(() => {
          const b = () => props.block as Of<"personalized_products">;
          return (
            <>
              <TextField
                label={t("content.gridTitle")}
                value={b().title ?? ""}
                maxLength={200}
                onChange={(title) => props.onChange({ ...b(), title })}
              />
              <SelectField
                label={t("marketing.productCount")}
                value={String(b().limit ?? 4)}
                options={[2, 3, 4, 5, 6, 7, 8].map((n) => ({ value: String(n), label: String(n) }))}
                onChange={(v) => props.onChange({ ...b(), limit: Number(v) })}
              />
              <p class="text-xs text-muted-foreground">{t("marketing.personalizedHint")}</p>
            </>
          );
        })()}
      </Match>
    </Switch>
  );
}

/** Campaign blocks of one language: add, reorder, remove, edit. */
export function EmailBlockEditor(props: {
  blocks: EmailBlock[];
  onChange: (blocks: EmailBlock[]) => void;
  invalid?: number;
}) {
  return (
    <section class="flex flex-col gap-3" aria-label={t("content.blocks")}>
      <h2 class="text-sm font-semibold">{t("content.blocks")}</h2>
      <Index each={props.blocks}>
        {(block, i) => (
          <fieldset
            class="min-w-0 rounded-md border p-3"
            classList={{
              "border-error-600": props.invalid === i,
              "border-border": props.invalid !== i,
            }}
          >
            <legend class="px-1 text-sm font-medium">
              {i + 1}. {t(`marketing.block_${block().type}`)}
            </legend>
            <div class="mb-3 flex flex-wrap gap-1">
              <Button
                disabled={i === 0}
                aria-label={`${t("common.moveUp")}: ${i + 1}`}
                onClick={() => props.onChange(moveItem(props.blocks, i, -1))}
              >
                {t("common.moveUp")}
              </Button>
              <Button
                disabled={i === props.blocks.length - 1}
                aria-label={`${t("common.moveDown")}: ${i + 1}`}
                onClick={() => props.onChange(moveItem(props.blocks, i, 1))}
              >
                {t("common.moveDown")}
              </Button>
              <Button
                aria-label={`${t("common.remove")}: ${i + 1}`}
                onClick={() => props.onChange(removeItem(props.blocks, i))}
              >
                {t("common.remove")}
              </Button>
            </div>
            <div class="flex flex-col gap-3">
              <BlockFields
                block={block()}
                onChange={(next) =>
                  props.onChange(props.blocks.map((b, j) => (i === j ? next : b)))
                }
              />
            </div>
            <Show when={props.invalid === i && !blockValid(block())}>
              <p role="alert" class="mt-2 text-xs font-medium text-error-700">
                {t("marketing.blockIncomplete")}
              </p>
            </Show>
          </fieldset>
        )}
      </Index>
      <Show when={props.blocks.length < MAX_BLOCKS}>
        <div>
          <Menu
            trigger={t("content.addBlock")}
            triggerLabel={t("content.addBlock")}
            items={EMAIL_BLOCK_TYPES.map((type) => ({
              label: t(`marketing.block_${type}`),
              onSelect: () => props.onChange([...props.blocks, blankEmailBlock(type)]),
            }))}
          />
        </div>
      </Show>
    </section>
  );
}
