/**
 * Minimal rich text editor for product/category descriptions: a `contenteditable` region with
 * a toolbar (bold, italic, headings, lists, link). HTML is sanitized with DOMPurify on load,
 * paste and every change, limited to what descriptions need; the API sanitizes again
 * (`ammonia`) and stays the trust boundary. `execCommand` is deprecated but universally
 * supported and has no replacement for this use; a full editor (Tiptap/ProseMirror, ~150 kB)
 * is disproportionate here.
 */
import DOMPurify from "dompurify";
import { createSignal, createUniqueId, For, onCleanup, onMount } from "solid-js";
import { t } from "../i18n/index.ts";

const ALLOWED = {
  ALLOWED_TAGS: ["p", "br", "strong", "b", "em", "i", "h2", "h3", "ul", "ol", "li", "a"],
  ALLOWED_ATTR: ["href"],
};

export function sanitizeHtml(html: string): string {
  return DOMPurify.sanitize(html, ALLOWED);
}

type Command = {
  id: string;
  label: () => string;
  glyph: string;
  run: () => void;
  state?: () => boolean;
};

export function RichText(props: {
  label: string;
  value: string;
  onChange: (html: string) => void;
}) {
  const id = createUniqueId();
  let area!: HTMLDivElement;
  const [active, setActive] = createSignal<Record<string, boolean>>({});

  const exec = (cmd: string, arg?: string) => {
    area.focus();
    document.execCommand(cmd, false, arg);
    emit();
  };
  const block = (tag: string) => {
    const current = document.queryCommandValue("formatBlock").toLowerCase();
    exec("formatBlock", current === tag ? "p" : tag);
  };

  const commands: Command[] = [
    {
      id: "bold",
      label: () => t("rte.bold"),
      glyph: "B",
      run: () => exec("bold"),
      state: () => document.queryCommandState("bold"),
    },
    {
      id: "italic",
      label: () => t("rte.italic"),
      glyph: "I",
      run: () => exec("italic"),
      state: () => document.queryCommandState("italic"),
    },
    {
      id: "h2",
      label: () => t("rte.heading"),
      glyph: "H2",
      run: () => block("h2"),
      state: () => document.queryCommandValue("formatBlock").toLowerCase() === "h2",
    },
    {
      id: "h3",
      label: () => t("rte.subheading"),
      glyph: "H3",
      run: () => block("h3"),
      state: () => document.queryCommandValue("formatBlock").toLowerCase() === "h3",
    },
    {
      id: "ul",
      label: () => t("rte.bulletList"),
      glyph: "•",
      run: () => exec("insertUnorderedList"),
      state: () => document.queryCommandState("insertUnorderedList"),
    },
    {
      id: "ol",
      label: () => t("rte.numberedList"),
      glyph: "1.",
      run: () => exec("insertOrderedList"),
      state: () => document.queryCommandState("insertOrderedList"),
    },
    {
      id: "link",
      label: () => t("rte.link"),
      glyph: "↗",
      run: () => {
        const url = window.prompt(t("rte.linkPrompt"), "https://");
        if (url === null) return;
        if (url.trim() === "") exec("unlink");
        else if (/^(https?:|mailto:)/i.test(url.trim())) exec("createLink", url.trim());
      },
    },
    { id: "clear", label: () => t("rte.clear"), glyph: "⌫", run: () => exec("removeFormat") },
  ];

  const refreshState = () => {
    if (!area.contains(document.getSelection()?.anchorNode ?? null)) return;
    setActive(Object.fromEntries(commands.map((c) => [c.id, c.state?.() ?? false])));
  };

  const emit = () => {
    props.onChange(sanitizeHtml(area.innerHTML));
    refreshState();
  };

  onMount(() => {
    area.innerHTML = sanitizeHtml(props.value);
    document.addEventListener("selectionchange", refreshState);
    onCleanup(() => document.removeEventListener("selectionchange", refreshState));
  });

  const onPaste = (e: ClipboardEvent) => {
    e.preventDefault();
    const html = e.clipboardData?.getData("text/html");
    if (html) {
      document.execCommand("insertHTML", false, sanitizeHtml(html));
    } else {
      document.execCommand("insertText", false, e.clipboardData?.getData("text/plain") ?? "");
    }
    emit();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    const key = e.key.toLowerCase();
    if (key === "b" || key === "i") {
      e.preventDefault();
      exec(key === "b" ? "bold" : "italic");
    }
  };

  return (
    <div class="flex flex-col gap-1">
      <span id={`${id}-label`} class="text-xs font-medium text-muted-foreground">
        {props.label}
      </span>
      <div class="rounded-md border border-input bg-card focus-within:outline-2 focus-within:outline-ring">
        <div
          role="toolbar"
          aria-label={t("rte.toolbar")}
          aria-controls={id}
          class="flex flex-wrap gap-0.5 border-b border-border p-1"
        >
          <For each={commands}>
            {(c) => (
              <button
                type="button"
                class="grid h-7 min-w-7 place-items-center rounded-sm px-1.5 text-xs font-semibold text-muted-foreground
                  hover:bg-muted hover:text-foreground aria-pressed:bg-accent-50 aria-pressed:text-accent-700"
                classList={{ italic: c.id === "italic" }}
                aria-label={c.label()}
                title={c.label()}
                aria-pressed={c.state ? (active()[c.id] ?? false) : undefined}
                // Keep the text selection in the editor while clicking.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => c.run()}
              >
                <span aria-hidden="true">{c.glyph}</span>
              </button>
            )}
          </For>
        </div>
        {/* biome-ignore lint/a11y/useSemanticElements: rich text needs contenteditable, not <textarea> */}
        <div
          ref={area}
          id={id}
          role="textbox"
          aria-multiline="true"
          aria-labelledby={`${id}-label`}
          tabIndex={0}
          contentEditable
          class="prose-admin min-h-32 px-2.5 py-2 text-sm outline-none"
          onInput={emit}
          onPaste={onPaste}
          onKeyDown={onKeyDown}
        />
      </div>
    </div>
  );
}
