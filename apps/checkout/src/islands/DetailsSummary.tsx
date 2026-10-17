import { t } from "@platform/storefront-sdk/format";
import type { CheckoutView } from "@platform/storefront-sdk/types";
import { Show } from "solid-js";
import { addressLine, type DetailsStep, regionName } from "../lib/checkout-steps";
import { headingClass, sectionClass } from "../ui.tsx";

const SECTIONS: Record<DetailsStep, { id: string; number: number; title: string }> = {
  contact: { id: "co-contact", number: 1, title: "checkout.contact" },
  addresses: { id: "co-address", number: 2, title: "checkout.address" },
};

export default function DetailsSummary(props: {
  m: Record<string, string>;
  locale: string;
  view: CheckoutView;
  step: DetailsStep;
  editHref: string;
}) {
  const region = regionName(props.locale);
  const section = SECTIONS[props.step];
  const title = t(props.m, section.title);
  return (
    <section aria-labelledby={section.id} class={sectionClass}>
      <div class="mb-2 flex flex-wrap items-center justify-between gap-x-4">
        <h2 id={section.id} class={headingClass}>
          {section.number}. {title}
        </h2>
        <a
          href={props.editHref}
          class="inline-flex h-control items-center text-sm text-identity-ink underline"
        >
          {t(props.m, "address.edit")}
          <span class="sr-only"> {title}</span>
        </a>
      </div>
      <Show
        when={props.step === "addresses"}
        fallback={
          <p class="grid gap-0.5 text-sm [overflow-wrap:anywhere]">
            <span>{props.view.email}</span>
            <Show when={props.view.phone}>
              <span>{props.view.phone}</span>
            </Show>
          </p>
        }
      >
        <div class="grid gap-2 text-sm [overflow-wrap:anywhere]">
          <Show when={props.view.billing_address}>
            {(billing) => <p>{addressLine(billing(), region)}</p>}
          </Show>
          <Show when={props.view.shipping_address}>
            {(shipping) => (
              <p>
                <span class="font-semibold">{t(props.m, "checkout.delivery_address")}:</span>{" "}
                {addressLine(shipping(), region)}
              </p>
            )}
          </Show>
        </div>
      </Show>
    </section>
  );
}
