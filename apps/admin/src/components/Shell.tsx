import { Button, EmptyState, LoadingState, Menu, SelectField } from "@platform/ui";
import { A, useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createSignal, For, type JSX, Match, Show, Switch } from "solid-js";
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

const groups: { label: () => string; items: NavItem[] }[] = [
  {
    label: () => t("orders.title"),
    items: [
      { href: "/orders", label: () => t("orders.title"), min: "staff" },
      { href: "/withdrawals", label: () => t("fulfillment.withdrawals"), min: "staff" },
      { href: "/payments/exceptions", label: () => t("nav.exceptions"), min: "staff" },
      { href: "/payments/bank", label: () => t("nav.bank"), min: "staff" },
    ],
  },
  {
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
    label: () => t("nav.pricing"),
    items: [
      { href: "/price-lists", label: () => t("nav.priceLists"), min: "staff" },
      { href: "/sales", label: () => t("nav.sales"), min: "staff" },
      { href: "/coupons", label: () => t("nav.coupons"), min: "staff" },
    ],
  },
  {
    label: () => t("nav.marketing"),
    items: [
      { href: "/marketing/campaigns", label: () => t("nav.campaigns"), min: "staff" },
      { href: "/marketing/segments", label: () => t("nav.segments"), min: "staff" },
      { href: "/marketing/subscribers", label: () => t("nav.subscribers"), min: "staff" },
    ],
  },
  {
    label: () => t("content.content"),
    items: [
      { href: "/content/pages", label: () => t("content.pages"), min: "staff" },
      { href: "/content/blog", label: () => t("content.blog"), min: "staff" },
      { href: "/content/menus", label: () => t("content.menus"), min: "staff" },
      { href: "/content/legal", label: () => t("content.legal"), min: "staff" },
      { href: "/content/redirects", label: () => t("redirects.title"), min: "staff" },
    ],
  },
  {
    label: () => t("content.channels"),
    items: [
      { href: "/imports", label: () => t("content.imports"), min: "admin" },
      { href: "/feeds", label: () => t("content.feeds"), min: "staff" },
    ],
  },
  {
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
    label: () => t("nav.platform"),
    items: [{ href: "/platform/jobs", label: () => t("nav.jobs"), min: "staff", superadmin: true }],
  },
];

const linkClass =
  "flex h-8 items-center rounded-md px-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground";
const activeClass =
  "bg-accent-50 font-medium text-accent-700 hover:bg-accent-50 hover:text-accent-700";

export function Shell(props: { children: JSX.Element }) {
  const { me, current, can } = useMembership();
  const navigate = useNavigate();
  const location = useLocation();
  const [navOpen, setNavOpen] = createSignal(false);

  // Close the small-screen navigation after navigating.
  createEffect(() => {
    location.pathname;
    setNavOpen(false);
  });

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

  const nav = () => (
    <nav aria-label={t("nav.main")} class="flex flex-col gap-4">
      <A href="/" end class={linkClass} activeClass={activeClass}>
        {t("nav.dashboard")}
      </A>
      <For each={groups}>
        {(g) => (
          <Show when={g.items.some(visible)}>
            <div class="flex flex-col gap-0.5">
              <p class="col-label px-2 pb-1">{g.label()}</p>
              <ul class="flex flex-col gap-0.5">
                <For each={g.items.filter(visible)}>
                  {(item) => (
                    <li>
                      <A href={item.href} class={linkClass} activeClass={activeClass}>
                        {item.label()}
                      </A>
                    </li>
                  )}
                </For>
              </ul>
            </div>
          </Show>
        )}
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
            {(m) => (
              <div class="flex min-h-dvh">
                <a
                  href="#main"
                  class="sr-only z-50 rounded-md bg-card px-3 py-2 focus:not-sr-only focus:fixed focus:top-2 focus:left-2"
                >
                  {t("app.skip")}
                </a>
                <aside
                  class="fixed inset-y-0 left-0 z-30 w-sidebar flex-col gap-5 overflow-y-auto border-r border-border bg-card p-3 md:sticky md:top-0 md:flex md:h-dvh"
                  classList={{ flex: navOpen(), hidden: !navOpen() }}
                >
                  <p class="px-2 pt-1 text-sm font-semibold tracking-tight">{t("app.name")}</p>
                  <Show
                    when={data.memberships.length > 1}
                    fallback={
                      <div class="flex flex-col px-2">
                        <span class="col-label">{t("app.shop")}</span>
                        <span class="truncate font-medium">{m().name}</span>
                      </div>
                    }
                  >
                    <SelectField
                      label={t("app.shop")}
                      value={m().tenant_id}
                      options={data.memberships.map((x) => ({ value: x.tenant_id, label: x.name }))}
                      onChange={switchTenant}
                    />
                  </Show>
                  {nav()}
                </aside>
                <Show when={navOpen()}>
                  <button
                    type="button"
                    class="fixed inset-0 z-20 bg-black/30 md:hidden"
                    aria-label={t("common.close")}
                    onClick={() => setNavOpen(false)}
                  />
                </Show>
                <div class="flex min-w-0 flex-1 flex-col">
                  <header class="sticky top-0 z-10 flex h-12 items-center justify-between gap-2 border-b border-border bg-card px-3 md:px-5">
                    <Button
                      variant="ghost"
                      class="md:invisible"
                      aria-expanded={navOpen()}
                      onClick={() => setNavOpen(!navOpen())}
                    >
                      {t("app.menu")}
                    </Button>
                    <div class="flex items-center gap-1">
                      <Menu
                        triggerLabel={t("app.language")}
                        trigger={<span class="uppercase">{locale()}</span>}
                        items={LOCALES.map((l: Locale) => ({
                          label: t(`common.locale_${l}`),
                          onSelect: () => setLocale(l),
                        }))}
                      />
                      <Menu
                        triggerLabel={t("app.accountMenu")}
                        trigger={
                          <span class="flex min-w-0 items-center gap-2">
                            <span class="hidden max-w-48 truncate sm:inline">
                              {claims()?.email}
                            </span>
                            <span class="rounded-sm bg-neutral-50 px-1.5 text-xs text-neutral-700">
                              {t(`roles.${m().role}`)}
                            </span>
                          </span>
                        }
                        header={t("auth.signedInAs", { email: claims()?.email ?? "" })}
                        items={[
                          {
                            label: t("nav.security"),
                            onSelect: () => navigate("/account/security"),
                          },
                          {
                            label: theme() === "dark" ? t("app.themeLight") : t("app.themeDark"),
                            onSelect: () => setTheme(theme() === "dark" ? "light" : "dark"),
                          },
                          { label: t("auth.signOut"), onSelect: () => void logout() },
                        ]}
                      />
                    </div>
                  </header>
                  <main
                    id="main"
                    tabindex="-1"
                    class="flex-1 px-3 py-4 outline-none md:px-6 md:py-5"
                  >
                    {props.children}
                  </main>
                </div>
              </div>
            )}
          </Match>
          <Match when={true}>
            <LoadingState label={t("common.loading")} />
          </Match>
        </Switch>
      )}
    </QueryState>
  );
}
