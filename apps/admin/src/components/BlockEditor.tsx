import { Button, Menu, SelectField, TextField } from "@platform/ui";
import { Index, Match, Switch } from "solid-js";
import { t } from "../i18n/index.ts";
import {
  BLOCK_TYPES,
  type Block,
  blankBlock,
  moveItem,
  removeItem,
  validContentHref,
} from "../lib/content-form.ts";
import { ContentAsset } from "./ContentAsset.tsx";
import { ProductPicker } from "./ProductPicker.tsx";
import { RichText } from "./RichText.tsx";

function BlockFields(props: { block: Block; onChange: (block: Block) => void }) {
  return (
    <Switch>
      <Match when={props.block.type === "heading"}>
        {(() => {
          const b = () => props.block as Extract<Block, { type: "heading" }>;
          return (
            <>
              <TextField
                label={t("content.headingText")}
                value={b().text}
                onChange={(text) => props.onChange({ ...b(), text })}
              />
              <SelectField
                label={t("content.level")}
                value={String(b().level ?? 2)}
                options={[
                  { value: "2", label: "H2" },
                  { value: "3", label: "H3" },
                ]}
                onChange={(v) => props.onChange({ ...b(), level: Number(v) })}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "rich_text"}>
        {(() => {
          const b = () => props.block as Extract<Block, { type: "rich_text" }>;
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
          const b = () => props.block as Extract<Block, { type: "image" }>;
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
                onChange={(alt) => props.onChange({ ...b(), alt })}
              />
              <TextField
                label={t("content.caption")}
                value={b().caption ?? ""}
                onChange={(caption) => props.onChange({ ...b(), caption })}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "button"}>
        {(() => {
          const b = () => props.block as Extract<Block, { type: "button" }>;
          return (
            <>
              <TextField
                label={t("content.label")}
                value={b().label}
                onChange={(label) => props.onChange({ ...b(), label })}
              />
              <TextField
                label={t("content.href")}
                description={t("content.hrefHint")}
                error={!!b().href && !validContentHref(b().href) && t("content.hrefHint")}
                value={b().href}
                onChange={(href) => props.onChange({ ...b(), href })}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "product_grid"}>
        {(() => {
          const b = () => props.block as Extract<Block, { type: "product_grid" }>;
          return (
            <>
              <TextField
                label={t("content.gridTitle")}
                value={b().title ?? ""}
                onChange={(title) => props.onChange({ ...b(), title })}
              />
              <p class="text-xs">
                {t("content.gridLimit")} ({b().product_ids.length}/24)
              </p>
              <ProductPicker
                value={b().product_ids}
                onChange={(product_ids) => {
                  if (product_ids.length <= 24) props.onChange({ ...b(), product_ids });
                }}
              />
            </>
          );
        })()}
      </Match>
      <Match when={props.block.type === "faq"}>
        {(() => {
          const b = () => props.block as Extract<Block, { type: "faq" }>;
          return (
            <>
              <Index each={b().items}>
                {(item, i) => (
                  <fieldset class="flex min-w-0 flex-col gap-2 border-b border-border pb-3">
                    <legend class="text-sm">
                      {t("content.question")} {i + 1}
                    </legend>
                    <TextField
                      label={t("content.question")}
                      value={item().question}
                      onChange={(question) =>
                        props.onChange({
                          ...b(),
                          items: b().items.map((x, j) => (i === j ? { ...x, question } : x)),
                        })
                      }
                    />
                    <RichText
                      label={t("content.answer")}
                      value={item().answer_html}
                      onChange={(answer_html) =>
                        props.onChange({
                          ...b(),
                          items: b().items.map((x, j) => (i === j ? { ...x, answer_html } : x)),
                        })
                      }
                    />
                    <Button
                      aria-label={`${t("common.remove")}: ${t("content.question")} ${i + 1}`}
                      onClick={() => props.onChange({ ...b(), items: removeItem(b().items, i) })}
                    >
                      {t("common.remove")}
                    </Button>
                  </fieldset>
                )}
              </Index>
              <Button
                onClick={() =>
                  props.onChange({
                    ...b(),
                    items: [...b().items, { question: "", answer_html: "" }],
                  })
                }
              >
                {t("content.addQuestion")}
              </Button>
            </>
          );
        })()}
      </Match>
    </Switch>
  );
}
export function BlockEditor(props: { blocks: Block[]; onChange: (blocks: Block[]) => void }) {
  return (
    <section class="flex flex-col gap-3" aria-label={t("content.blocks")}>
      <h2 class="text-sm font-semibold">{t("content.blocks")}</h2>
      <Index each={props.blocks}>
        {(block, i) => (
          <fieldset class="min-w-0 rounded-md border border-border p-3">
            <legend class="px-1 text-sm font-medium">
              {i + 1}. {t(`content.${block().type}`)}
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
          </fieldset>
        )}
      </Index>
      <Menu
        trigger={t("content.addBlock")}
        triggerLabel={t("content.addBlock")}
        items={BLOCK_TYPES.map((type) => ({
          label: t(`content.${type}`),
          onSelect: () => props.onChange([...props.blocks, blankBlock(type)]),
        }))}
      />
    </section>
  );
}
