import { A } from "@solidjs/router";
import { For, Show } from "solid-js";
import { PageHeader } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import type { Role } from "../lib/api.ts";
import { useMembership } from "../lib/me.ts";

/** Placeholder overview until analytics (WP14): where to go next. */
export default function Dashboard() {
  const { current, can } = useMembership();
  const steps: { href: string; label: () => string; min: Role }[] = [
    { href: "/categories", label: () => t("dashboard.stepCategories"), min: "staff" },
    { href: "/products/new", label: () => t("dashboard.stepProducts"), min: "staff" },
    { href: "/staff", label: () => t("dashboard.stepStaff"), min: "admin" },
    { href: "/account/security", label: () => t("dashboard.stepSecurity"), min: "staff" },
  ];
  return (
    <Show when={current()}>
      {(m) => (
        <>
          <PageHeader
            title={t("dashboard.title")}
            description={t("dashboard.lead", { shop: m().name, role: t(`roles.${m().role}`) })}
          />
          <section aria-labelledby="next-steps" class="max-w-xl">
            <h2 id="next-steps" class="col-label mb-2">
              {t("dashboard.nextSteps")}
            </h2>
            <ol class="flex flex-col border-t border-border">
              <For each={steps.filter((s) => can(s.min))}>
                {(s, i) => (
                  <li class="border-b border-border">
                    <A
                      href={s.href}
                      class="flex h-10 items-center gap-3 px-1 text-sm hover:bg-muted"
                    >
                      <span class="figures w-5 text-faint-foreground">{i() + 1}</span>
                      <span class="text-accent-700">{s.label()}</span>
                    </A>
                  </li>
                )}
              </For>
            </ol>
          </section>
        </>
      )}
    </Show>
  );
}
