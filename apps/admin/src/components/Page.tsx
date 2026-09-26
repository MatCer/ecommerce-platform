import { Button, ErrorState, LoadingState, PageHeading, PermissionDenied } from "@platform/ui";
import { A } from "@solidjs/router";
import { type JSX, Match, Show, Switch } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import { ApiError } from "../lib/api.ts";

/** Page title row (Pajamas page heading) with an optional back link above the title. */
export function PageHeader(props: {
  title: string;
  description?: string;
  actions?: JSX.Element;
  back?: { href: string; label: string };
}) {
  return (
    <PageHeading
      title={props.title}
      description={props.description}
      actions={props.actions}
      before={
        <Show when={props.back}>
          {(b) => (
            <A
              href={b().href}
              class="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground hover:text-heading hover:underline"
            >
              ← {b().label}
            </A>
          )}
        </Show>
      }
    />
  );
}

export function isForbidden(err: unknown): boolean {
  return err instanceof ApiError && err.status === 403;
}

/** Loading, permission-denied and error states for one query; children render its data. */
export function QueryState<T>(props: {
  query: {
    data: T | undefined;
    isPending: boolean;
    isError: boolean;
    error: unknown;
    refetch: () => unknown;
  };
  children: (data: T) => JSX.Element;
}) {
  return (
    <Switch>
      <Match when={props.query.isPending}>
        <LoadingState label={t("common.loading")} />
      </Match>
      <Match when={props.query.isError && isForbidden(props.query.error)}>
        <PermissionDenied
          title={t("common.forbiddenTitle")}
          description={t("common.forbiddenDesc")}
        />
      </Match>
      <Match when={props.query.isError}>
        <ErrorState
          title={t("common.errorLoad")}
          description={errorMessage(props.query.error)}
          action={<Button onClick={() => props.query.refetch()}>{t("common.retry")}</Button>}
        />
      </Match>
      <Match when={props.query.data !== undefined}>{props.children(props.query.data as T)}</Match>
    </Switch>
  );
}

// Table styles live in @platform/ui; re-exported so existing pages keep their imports.
export { Th, tableClass, tdClass } from "@platform/ui";
