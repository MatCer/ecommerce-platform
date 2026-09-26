import {
  Avatar,
  Badge,
  Breadcrumb,
  type BreadcrumbItem,
  Button,
  Drawer,
  EmptyState,
  Icon,
  type IconName,
  LoadingState,
  Menu,
  TooltipButton,
} from "@platform/ui";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createSignal, For, type JSX, Match, on, Show, Switch } from "solid-js";
import { LOCALES, type Locale, locale, setLocale, t } from "../i18n/index.ts";
import { queryClient, type Role, setTenantId } from "../lib/api.ts";
import { useMembership } from "../lib/me.ts";
import { claims, signOut } from "../lib/session.ts";
import { setTheme, theme } from "../lib/theme.ts";
import { QueryState } from "./Page.tsx";

interface NavItem {
  href: string;
  label: () => string;
  min: Role;
  end?: boolean;
  /** Platform operators only (`me.is_superadmin`), whatever their shop role. */
  superadmin?: boolean;
}

const groups: { icon: IconName; label: () => string; items: NavItem[] }[] = [
  {
    icon: "list-task",
    label: () => t("orders.title"),
    items: [
      { href: "/orders", label: () => t("orders.title"), min: "staff" },
      { href: "/customers", label: () => t("customers.title"), min: "staff" },
      { href: "/withdrawals", label: () => t("fulfillment.withdrawals"), min: "staff" },
      { href: "/payments/exceptions", label: () => t("nav.exceptions"), min: "staff" },
      { href: "/payments/bank", label: () => t("nav.bank"), min: "staff" },
    ],
  },
  {
    icon: "package",
    label: () => t("nav.catalog"),
    items: [
      { href: "/products", label: () => t("nav.products"), min: "staff" },
      { href: "/categories", label: () => t("nav.categories"), min: "staff" },
      { href: "/parameters", label: () => t("nav.parameters"), min: "staff" },
      { href: "/inventory", label: () => t("nav.inventory"), min: "staff" },
      { href: "/collections", label: () => t("nav.collections"), min: "staff" },
      { href: "/ai/bulk-edit", label: () => t("nav.aiBulk"), min: "staff" },
    ],
  },
  {
    icon: "tag",
    label: () => t("nav.pricing"),
    items: [
      { href: "/price-lists", label: () => t("nav.priceLists"), min: "staff" },
      { href: "/sales", label: () => t("nav.sales"), min: "staff" },
      { href: "/coupons", label: () => t("nav.coupons"), min: "staff" },
    ],
  },
  {
    icon: "bullhorn",
    label: () => t("nav.marketing"),
    items: [
      { href: "/marketing/campaigns", label: () => t("nav.campaigns"), min: "staff" },
      { href: "/marketing/segments", label: () => t("nav.segments"), min: "staff" },
      { href: "/marketing/subscribers", label: () => t("nav.subscribers"), min: "staff" },
      { href: "/marketing/reviews", label: () => t("nav.reviews"), min: "staff" },
      { href: "/marketing/flows", label: () => t("nav.flows"), min: "staff" },
    ],
  },
  {
    icon: "document",
    label: () => t("content.content"),
    items: [
      { href: "/content/pages", label: () => t("content.pages"), min: "staff" },
      { href: "/content/blog", label: () => t("content.blog"), min: "staff" },
      { href: "/content/menus", label: () => t("content.menus"), min: "staff" },
      { href: "/content/legal", label: () => t("content.legal"), min: "staff" },
      { href: "/content/redirects", label: () => t("redirects.title"), min: "staff" },
      { href: "/themes", label: () => t("nav.themes"), min: "staff" },
    ],
  },
  {
    icon: "export",
    label: () => t("content.channels"),
    items: [
      { href: "/imports", label: () => t("content.imports"), min: "admin" },
      { href: "/feeds", label: () => t("content.feeds"), min: "staff" },
    ],
  },
  {
    icon: "archive",
    label: () => t("data.nav"),
    items: [
      { href: "/data/imports", label: () => t("data.imports"), min: "admin" },
      { href: "/data/archived-orders", label: () => t("data.archive"), min: "staff" },
      { href: "/data/privacy", label: () => t("data.privacy"), min: "admin" },
    ],
  },
  {
    icon: "settings",
    label: () => t("nav.settings"),
    items: [
      { href: "/settings/search", label: () => t("content.search"), min: "staff" },
      {
        href: "/settings/recommendations",
        label: () => t("nav.recommendations"),
        min: "staff",
      },
      { href: "/settings/ai", label: () => t("nav.aiSettings"), min: "staff" },
      { href: "/markets", label: () => t("nav.markets"), min: "staff" },
      { href: "/settings/carriers", label: () => t("fulfillment.carriers"), min: "admin" },
      { href: "/settings/shipping", label: () => t("shipping.title"), min: "staff" },
      { href: "/settings/payments", label: () => t("payments.title"), min: "staff" },
      { href: "/settings/tax", label: () => t("nav.taxProfile"), min: "staff" },
      { href: "/settings/email-branding", label: () => t("nav.emailBranding"), min: "staff" },
      { href: "/settings/emails", label: () => t("nav.emails"), min: "admin" },
      { href: "/staff", label: () => t("nav.staff"), min: "admin" },
      { href: "/audit-log", label: () => t("nav.auditLog"), min: "admin" },
      { href: "/settings/webhooks", label: () => t("nav.webhooks"), min: "admin" },
      { href: "/settings/ad-tracking", label: () => t("nav.adTracking"), min: "admin" },
    ],
  },
  {
    icon: "admin",
    label: () => t("nav.platform"),
    items: [{ href: "/platform/jobs", label: () => t("nav.jobs"), min: "staff", superadmin: true }],
  },
];

const SIDEBAR_KEY = "admin.sidebar";
const COLLAPSED_KEY = "admin.nav.collapsed";

function readCollapsed(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]");
    return new Set(Array.isArray(raw) ? raw.filter((x) => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

/** The nav item whose href is the longest prefix of the path (detail pages keep their parent). */
function matchItem(path: string): { group: (typeof groups)[number]; item: NavItem } | undefined {
  let best: { group: (typeof groups)[number]; item: NavItem } | undefined;
  for (const group of groups)
    for (const item of group.items)
      if (
        (path === item.href || path.startsWith(`${item.href}/`)) &&
        item.href.length > (best?.item.href.length ?? 0)
      )
        best = { group, item };
  return best;
}

const itemClass =
  "relative flex min-h-8 items-center gap-3 rounded-md px-2 py-1 text-sm text-foreground hover:bg-muted hover:text-heading";
// Pajamas super sidebar: selected item gets a grey wash, bold label and a blue marker bar.
const activeClass =
  "bg-selected font-semibold text-heading hover:bg-selected before:absolute before:top-1.5 before:bottom-1.5 " +
  "before:left-0.5 before:w-[3px] before:rounded-full before:bg-accent-600";

export function Shell(props: { children: JSX.Element }) {
  const { me, current, can } = useMembership();
  const navigate = useNavigate();
  const location = useLocation();
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [sidebarOpen, setSidebarOpen] = createSignal(
    localStorage.getItem(SIDEBAR_KEY) !== "hidden",
  );
  const [collapsed, setCollapsed] = createSignal(readCollapsed());

  const toggleSidebar = (open: boolean) => {
    localStorage.setItem(SIDEBAR_KEY, open ? "shown" : "hidden");
    setSidebarOpen(open);
  };
  const toggleGroup = (key: string) => {
    const next = new Set(collapsed());
    if (!next.delete(key)) next.add(key);
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]));
    setCollapsed(next);
  };

  // Close the small-screen drawer after navigating.
  createEffect(
    on(
      () => location.pathname,
      () => setDrawerOpen(false),
    ),
  );

  const switchTenant = (id: string) => {
    setTenantId(id);
    queryClient.removeQueries({ queryKey: ["t"] });
    navigate("/");
  };

  const logout = async () => {
    await signOut();
    queryClient.clear();
    navigate("/login");
  };

  const visible = (i: NavItem) => (i.superadmin ? me.data?.is_superadmin === true : can(i.min));
  const active = () => matchItem(location.pathname);

  // Navigating into a collapsed section expands it, so the current page is always in view.
  createEffect(
    on(active, (hit) => {
      if (hit && collapsed().has(hit.group.icon)) toggleGroup(hit.group.icon);
    }),
  );

  const nav = () => (
    <nav aria-label={t("nav.main")} class="flex flex-col gap-1 px-2 pb-4">
      <A href="/" end class={itemClass} activeClass={activeClass}>
        <Icon name="home" class="text-muted-foreground" />
        {t("nav.dashboard")}
      </A>
      <For each={groups}>
        {(g) => {
          const key = g.icon;
          const open = () => !collapsed().has(key);
          return (
            <Show when={g.items.some(visible)}>
              <div class="flex flex-col gap-0.5">
                <button
                  type="button"
                  class={`${itemClass} w-full text-left`}
                  aria-expanded={open()}
                  onClick={() => toggleGroup(key)}
                >
                  <Icon name={g.icon} class="text-muted-foreground" />
                  <span class="flex-1 truncate">{g.label()}</span>
                  <Icon
                    name="chevron-down"
                    size={14}
                    class={`text-muted-foreground transition-transform ${open() ? "" : "-rotate-90"}`}
                  />
                </button>
                <Show when={open()}>
                  <ul class="flex flex-col gap-0.5">
                    <For each={g.items.filter(visible)}>
                      {(item) => (
                        <li>
                          <A href={item.href} class={`${itemClass} pl-9`} activeClass={activeClass}>
                            <span class="truncate">{item.label()}</span>
                          </A>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </div>
            </Show>
          );
        }}
      </For>
    </nav>
  );

  return (
    <QueryState query={me}>
      {(data) => (
        <Switch>
          <Match when={data.memberships.length === 0}>
            <main class="mx-auto max-w-md p-8">
              <EmptyState
                title={t("app.noAccessTitle")}
                description={t("app.noAccessDesc")}
                action={<Button onClick={logout}>{t("auth.signOut")}</Button>}
              />
            </main>
          </Match>
          <Match when={current()}>
            {(m) => {
              const shop = () => (
                <div class="flex min-w-0 flex-1 items-center gap-2 text-left">
                  <Avatar name={m().name} shape="square" size={32} />
                  <span class="flex min-w-0 flex-col">
                    <span class="text-xs text-muted-foreground">{t("app.shop")}</span>
                    <span class="truncate text-sm font-semibold text-heading">{m().name}</span>
                  </span>
                </div>
              );
              const sidebar = (withHide: boolean) => (
                <div class="flex h-full flex-col gap-2">
                  <div class="flex h-12 shrink-0 items-center justify-between gap-2 px-3">
                    <A
                      href="/"
                      class="flex min-w-0 items-center gap-2 rounded-md font-semibold text-heading"
                    >
                      <span class="grid size-6 place-items-center rounded-md bg-primary text-primary-foreground">
                        <Icon name="package" size={14} />
                      </span>
                      <span class="truncate text-sm">{t("app.name")}</span>
                    </A>
                    <Show when={withHide}>
                      <TooltipButton
                        category="tertiary"
                        iconOnly
                        icon="sidebar"
                        aria-label={t("app.hideSidebar")}
                        tooltip={t("app.hideSidebar")}
                        onClick={() => toggleSidebar(false)}
                      />
                    </Show>
                  </div>
                  <div class="px-2">
                    <Show
                      when={data.memberships.length > 1}
                      fallback={<div class="flex rounded-md px-2 py-1.5">{shop()}</div>}
                    >
                      <Menu
                        triggerLabel={`${t("app.shop")}: ${m().name}`}
                        triggerClass="flex w-full items-center gap-2 rounded-md border border-border bg-background px-2 py-1.5 hover:bg-subtle"
                        placement="bottom-start"
                        trigger={
                          <>
                            {shop()}
                            <Icon name="chevron-down" class="text-muted-foreground" />
                          </>
                        }
                        items={data.memberships.map((x) => ({
                          label: x.name,
                          icon: x.tenant_id === m().tenant_id ? ("check" as const) : undefined,
                          onSelect: () => switchTenant(x.tenant_id),
                        }))}
                      />
                    </Show>
                  </div>
                  <div class="min-h-0 flex-1 overflow-y-auto pt-1">{nav()}</div>
                </div>
              );
              const crumbs = (): BreadcrumbItem[] => {
                const hit = active();
                const root = { label: m().name, href: "/" };
                if (location.pathname === "/") return [root, { label: t("nav.dashboard") }];
                if (!hit) return [root];
                const exact = location.pathname === hit.item.href;
                return [
                  root,
                  { label: hit.group.label() },
                  { label: hit.item.label(), href: exact ? undefined : hit.item.href },
                ];
              };
              return (
                <div class="flex min-h-dvh bg-subtle">
                  <a
                    href="#main"
                    class="sr-only z-50 rounded-md bg-card px-3 py-2 focus:not-sr-only focus:fixed focus:top-2 focus:left-2"
                  >
                    {t("app.skip")}
                  </a>
                  <Show when={sidebarOpen()}>
                    <aside class="sticky top-0 hidden h-dvh w-sidebar shrink-0 md:block">
                      {sidebar(true)}
                    </aside>
                  </Show>
                  <Drawer
                    open={drawerOpen()}
                    onOpenChange={setDrawerOpen}
                    title={t("app.menu")}
                    hideTitle
                    closeLabel={t("common.close")}
                  >
                    {sidebar(false)}
                  </Drawer>
                  <div class="flex min-w-0 flex-1 flex-col">
                    <header class="flex h-12 shrink-0 items-center gap-2 px-3 md:px-4">
                      <Button
                        category="tertiary"
                        iconOnly
                        icon="hamburger"
                        class="md:hidden"
                        aria-label={t("app.menu")}
                        aria-expanded={drawerOpen()}
                        onClick={() => setDrawerOpen(true)}
                      />
                      <Show when={!sidebarOpen()}>
                        <TooltipButton
                          category="tertiary"
                          iconOnly
                          icon="sidebar"
                          class="hidden md:inline-flex"
                          aria-label={t("app.showSidebar")}
                          tooltip={t("app.showSidebar")}
                          onClick={() => toggleSidebar(true)}
                        />
                      </Show>
                      <div class="min-w-0 flex-1">
                        <Breadcrumb label={t("app.breadcrumb")} items={crumbs()} />
                      </div>
                      <Menu
                        triggerLabel={t("app.language")}
                        trigger={
                          <>
                            <Icon name="earth" class="text-muted-foreground" />
                            <span class="uppercase">{locale()}</span>
                          </>
                        }
                        items={LOCALES.map((l: Locale) => ({
                          label: t(`common.locale_${l}`),
                          icon: l === locale() ? ("check" as const) : undefined,
                          onSelect: () => setLocale(l),
                        }))}
                      />
                      <Menu
                        triggerLabel={t("app.accountMenu")}
                        trigger={
                          <>
                            <Avatar name={claims()?.email ?? "?"} size={24} />
                            <Icon name="chevron-down" size={14} class="text-muted-foreground" />
                          </>
                        }
                        header={
                          <span class="flex flex-col items-start gap-1.5">
                            <span class="break-all">
                              {t("auth.signedInAs", { email: claims()?.email ?? "" })}
                            </span>
                            <Badge>{t(`roles.${m().role}`)}</Badge>
                          </span>
                        }
                        items={[
                          {
                            label: t("nav.security"),
                            icon: "lock",
                            onSelect: () => navigate("/account/security"),
                          },
                          {
                            label: theme() === "dark" ? t("app.themeLight") : t("app.themeDark"),
                            icon: theme() === "dark" ? "sun" : "moon",
                            onSelect: () => setTheme(theme() === "dark" ? "light" : "dark"),
                          },
                          {
                            label: t("auth.signOut"),
                            icon: "power",
                            separatorBefore: true,
                            onSelect: () => void logout(),
                          },
                        ]}
                      />
                    </header>
                    <main
                      id="main"
                      tabindex="-1"
                      class="mx-0 mb-0 flex-1 border-border bg-background px-4 py-4 outline-none md:mr-2 md:mb-2 md:rounded-lg md:border md:px-6 md:py-5"
                      classList={{ "md:ml-2": !sidebarOpen() }}
                    >
                      {props.children}
                    </main>
                  </div>
                </div>
              );
            }}
          </Match>
          <Match when={true}>
            <LoadingState label={t("common.loading")} />
          </Match>
        </Switch>
      )}
    </QueryState>
  );
}
