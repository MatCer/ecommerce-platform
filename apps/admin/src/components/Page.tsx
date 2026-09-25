import { Button, ErrorState, LoadingState, PermissionDenied } from "@platform/ui";
import { A } from "@solidjs/router";
import { type JSX, Match, Show, Switch } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import { ApiError } from "../lib/api.ts";

export function PageHeader(props: {
  title: string;
  description?: string;
  actions?: JSX.Element;
  back?: { href: string; label: string };
}) {
  return (
    <header class="mb-4 flex flex-wrap items-end justify-between gap-3 border-b border-border pb-3">
      <div class="flex min-w-0 flex-col gap-0.5">
        <Show when={props.back}>
          {(b) => (
            <A href={b().href} class="text-xs text-accent-700 hover:underline">
              ← {b().label}
            </A>
          )}
        </Show>
        <h1 class="text-lg font-semibold tracking-tight">{props.title}</h1>
        <Show when={props.description}>
          <p class="text-sm text-muted-foreground">{props.description}</p>
        </Show>
      </div>
      <Show when={props.actions}>
        <div class="flex flex-wrap items-center gap-2">{props.actions}</div>
      </Show>
    </header>
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

/** Table header cell (11px uppercase label). */
export function Th(props: { children?: JSX.Element; class?: string; srOnly?: boolean }) {
  return (
    <th scope="col" class={`col-label h-8 px-2 text-left align-middle ${props.class ?? ""}`}>
      <Show when={props.srOnly} fallback={props.children}>
        <span class="sr-only">{props.children}</span>
      </Show>
    </th>
  );
}

export const tableClass =
  "w-full border-collapse text-sm [&_tbody_tr]:border-b [&_tbody_tr]:border-border [&_thead_tr]:border-b [&_thead_tr]:border-border-strong";
export const tdClass = "h-row px-2 align-middle";
