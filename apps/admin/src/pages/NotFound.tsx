import { buttonClass, EmptyState } from "@platform/ui";
import { A } from "@solidjs/router";
import { t } from "../i18n/index.ts";

export default function NotFound() {
  return (
    <EmptyState
      icon="search"
      title={t("common.notFoundTitle")}
      description={t("common.notFoundDesc")}
      action={
        <A href="/" class={buttonClass()}>
          {t("common.goHome")}
        </A>
      }
    />
  );
}
