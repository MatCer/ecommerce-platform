# Admin design system: GitLab Pajamas on SolidJS

The admin looks like current GitLab (design.gitlab.com, @gitlab/ui 137 tokens): light neutral
chrome, super sidebar, white content panel, near-black primary buttons, blue only for links,
focus and "info". Stack stays SolidJS + Kobalte + Tailwind 4. Sources of truth:
`packages/config/tailwind/theme.css` (tokens) and `packages/ui/src` (components, all exported
from `@platform/ui`). Phase 2 converts pages using only this doc and that source.

## Tokens (Tailwind classes)

| Class | Use |
|---|---|
| `bg-subtle` | chrome: page ground, sidebar, card headers, table heads, disabled controls |
| `bg-background` | content panel and card bodies |
| `bg-card` | overlays: dropdowns, modals |
| `bg-muted` | hover/active wash (translucent), never a resting surface |
| `text-heading` / `text-foreground` | headings + strong text / body text |
| `text-muted-foreground` / `text-faint-foreground` | secondary text, labels / metadata, placeholders |
| `border-border` / `border-border-strong` / `border-input` | dividers / default-button + dropdown outline / control outline |
| `bg-primary text-primary-foreground` | the one primary action, selected controls (dark in light mode, light in dark mode) |
| `text-accent-700` (`linkClass`) | links; `accent-600` = focus ring and charts only |
| `{success,warning,error,info}-{50,200,600,700}` | 50 = alert fill, 200 = alert border, 600 = icon/solid fill, 700 = text on 50 |
| `bg-(--badge-X-bg) text-(--badge-X-fg)` | badges only (use `<Badge>`) |
| `figures` | `tabular-nums` for numbers in columns (text face, not mono) |
| `font-mono` | codes/IDs/SKUs/JSON (GitLab Mono) |

Type: body `text-sm` (14px), metadata `text-xs` (12px), card/modal titles `text-base`
(16px semibold), page title `text-2xl` (24px, `PageHeading`). Radii: `rounded-sm` 4px chips,
`rounded-md` 8px buttons/inputs/alerts/dropdowns, `rounded-lg` 12px cards, `rounded-xl` 16px
modals. Sizes: `h-control` 32px, `h-control-sm` 24px, `h-row` 40px table rows, 4px/8px grid.
Contrast: every text token >= 4.5:1 on its surface in both modes (figures in theme.css).

Rules: never use Tailwind `dark:` (it follows the OS, not our `.dark` class); every colour
comes from a token so both modes work. No colour transitions (axe samples mid-fade). No raw
hex or `oklch` in pages.

## Components (`@platform/ui`)

| Component | When (Pajamas rule) |
|---|---|
| `Button` `variant` default/confirm/danger, `category` primary/secondary/tertiary, `size` small/medium, `icon`, `iconOnly`, `loading`, `selected`, `block` | One `confirm` per view (the main action). `danger` only for destructive. `category="tertiary"` for low-emphasis/inline and icon buttons. Icon-only needs `aria-label`. `buttonClass({...})` styles router links. |
| `ButtonGroup` | 2-4 related buttons joined; with `selected` it is a toggle set. |
| `TooltipButton` | Icon-only button that needs a visible hint (hover + focus). |
| `Badge` `tone`, `icon` | Status pill. success = done/active, warning = attention, error = failed, info = in progress, neutral = draft/inactive. Not clickable. |
| `Alert` `tone`, `title`, `actions`, `onDismiss`+`dismissLabel`, `live` | Inline message about the page/section. Plain container by default; `error` is always `role=alert`; set `live` when it appears as the result of an action (warning -> `alert`, others -> `status`). Replaces coloured `<p>` boxes. |
| `Card` `title`, `count`, `countIcon`, `description`, `actions`, `footer`, `padding="none"`, `id`, `labelledBy`, `aria-label` | Every grouped block (Pajamas CRUD component). Tables go in `padding="none"`. `labelledBy="x"` makes it a region named by its title (the h2 gets id `x`); `aria-label` names it explicitly (a table inside can reuse the id: `aria-labelledby="x"`). Never wrap a Card in another `<section>`. The body stretches when the card is a full-height grid cell. |
| `PageHeading` (admin `PageHeader` wraps it + `back`) | Once per page: h1, description, page actions. |
| `Breadcrumb` | Shell only (derived from the nav). |
| `TextField` (`multiline` = textarea), `SelectField` (native), `Checkbox`, `Radio`, `Toggle`, `SearchBox`, `FormGroup` + `describedBy` | Labels above controls, bold. Help text under the control; `error` replaces it. Radio for 2-5 exclusive options, select for more. Toggle only for settings that apply immediately; Checkbox inside forms with a Save button. `SearchBox` for list filters (its `clearLabel` must differ from any "Clear filters" button). `FormGroup` wraps raw inputs/pickers. |
| `FieldGroup` | Titled section inside a long form (fieldset + legend). |
| `SegmentedControl` | Switch between 2-5 views/filters of the same data. |
| `Menu` (`items` with `icon`, `danger`, `separatorBefore`; `header`) | Disclosure dropdown of actions (row "more" menus, account menu). |
| `Dialog` (`closeLabel` adds the X), `ConfirmDialog`, `Drawer` | Modal for focused tasks; footer = cancel then confirm/danger. Drawer = side panel / mobile nav. |
| `Tabs` (`count`) | Sibling views of one object. |
| `showToast` / `ToastRegion` | Short confirmation after an action (dark, bottom-left). Errors persist. |
| `EmptyState` (`icon`), `ErrorState`, `PermissionDenied`, `LoadingState`, `Skeleton`, `Spinner`, `ProgressBar` | Empty = title + one sentence + one action. Prefer `Skeleton` for layout-shaped loads. |
| `Collapse` `open`, `onToggle(open)` | Optional detail (replaces raw `<details>`); `onToggle` for lazy bodies. |
| `Avatar` | Shop/user identity (decorative). |
| `tableClass`, `tdClass`, `Th` | Plain `<table>`; wrap in `<div class="overflow-x-auto">`. |
| `Icon name=` | Pajamas icons; decorative unless `label`. Add icons by importing the SVG in `icon.tsx`. |

`@platform/ui` is admin-only. The checkout is a shop-branded storefront, not GitLab-styled:
it keeps its own small form primitives in `apps/checkout/src/ui.tsx`, styled by the shop's
tokens (`apps/checkout/src/styles/global.css`), so admin restyles never reach it.

Deliberately not built (no admin use yet): numbered Pagination (lists use "Load more"),
sortable headers, Popover, removable Token/Label, Combobox, tier badge. Add them to
`packages/ui` when a page needs one, on Kobalte.

## Page recipes

**List page** (reference: `apps/admin/src/pages/Products.tsx`)
1. `PageHeader title actions={<A class={buttonClass({ variant: "confirm" })}>New</A>}`.
2. Filter bar `div.mb-4.flex.flex-wrap.items-center.gap-2`: `SearchBox class="min-w-60 flex-1"`,
   `SelectField hideLabel` filters (first option "All ..."), tertiary "Clear filters".
3. `QueryState`, then `EmptyState icon=...` (no data / no matches) or
   `Card padding="none" footer={Load more}` holding the table. Row title = `font-semibold
   text-heading` link; codes `font-mono text-xs`; numbers `figures text-right`; status `Badge`.

**Detail page** (reference: `pages/OrderDetail.tsx`)
1. `PageHeader title description back actions`, then a badge row and the action buttons.
2. `div.grid.items-start.gap-4.xl:grid-cols-[minmax(0,1fr)_20rem]`: main column of `Card`s
   (tables `padding="none"`), right `<aside>` of attribute cards (contact, addresses, meta).
3. Wide tables and activity (attempts, timeline, notes) go full width below the grid, so a
   1280px viewport never scrolls a table sideways (axe `scrollable-region-focusable`).
4. Attribute lists: `dl.grid.grid-cols-2.gap-x-4.gap-y-2 [&_dt]:text-muted-foreground`.

**Settings form** (reference: `pages/TaxProfile.tsx`)
`form.flex.max-w-2xl.flex-col.gap-6`: status `Alert`s first, `fieldset.flex.flex-col.gap-5`
of fields (grids `gap-5`), radios in a `fieldset` with `legend class={labelClass}`, error as
`Alert tone="error"`, footer `div.border-t.border-border.pt-5` with one confirm Save.
Long settings: one `FieldGroup` (or `Card`) per topic.

**Editor page** (product/content editors): `PageHeader back actions={Save confirm}`; same
two-column grid as the detail page with the form `Card`s in the main column and
status/visibility/media cards in the aside; sticky nothing; one Save.

## Migration checklist (per page)

- [ ] `rounded-md border border-border p-4` boxes, `order/shared.tsx` `Section` → `Card`.
- [ ] Coloured notes `<p class="bg-warning-50 ...">`, `role="alert"` paragraphs → `Alert`.
- [ ] `variant="primary"` is gone: use `variant="confirm"` (one per view); secondary
  actions are default buttons; inline/row actions `category="tertiary"`, `size="small"` in
  dense rows; icon glyphs (`×`, `…`, arrows) → `Button iconOnly icon="close"` + `aria-label`.
- [ ] Row "more" menus → `Menu` with `icon="ellipsis_v"` trigger, destructive item `danger`.
- [ ] Tables: wrap in `Card padding="none"`, headers `Th`, right-align numbers.
- [ ] Raw `<input type="radio">` → `Radio`; raw inputs → `FormGroup` + `controlClass`;
  `<details>` → `Collapse`; `<progress>` → `ProgressBar`; search inputs → `SearchBox`.
- [ ] Empty lists → `EmptyState` with an `icon` and the page's create action.
- [ ] Replace hard-coded colours (`bg-white`, `text-gray-*`, `bg-accent-600 text-white`) with
  tokens; remove any `dark:` variant.
- [ ] Links in text/tables: `linkClass` (or `font-semibold text-heading` for row titles).
- [ ] Keep every accessible name, role, label and visible text the e2e suite uses (search
  `e2e/admin/*.spec.ts` for the page's strings first). Keep `← label` back links (`PageHeader
  back`), keep native `<select>` (tests call `selectOption`).
- [ ] New UI strings go into `apps/admin/src/i18n/{en,cs,sk}.ts` (typed; all three).
- [ ] Check 1280px (e2e viewport), 390px and dark mode; run the page's e2e spec (axe).

## Shell facts

`components/Shell.tsx`: super sidebar (16rem, `bg-subtle`) with app mark, shop switcher
(`Menu` when the user has several shops), collapsible nav sections (state in localStorage
`admin.nav.collapsed`; the section of the current page auto-expands), hide/show sidebar
(`admin.sidebar`), mobile `Drawer`. The top bar has breadcrumbs (derived from the nav),
language and account menus. Content renders in `main#main`, a white rounded panel. Adding a
nav destination = one entry in `groups`. Sections default to expanded because e2e clicks nav
links directly; tab stops before page content are ~50 (keyboard e2e allows 100).

## Screenshots

`docs/screenshots/pajamas/<view>-<light|dark>-<width>.png` for login, dashboard, products,
product-editor, order-detail, settings-form (tax), modal (new coupon) and dropdown (account
menu): light and dark at 1440px, light at 390px. Only seeded demo data with fake identities
(`*.example`, `example.test` addresses); never shoot orders or customers of real people.
