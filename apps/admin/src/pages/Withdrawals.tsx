import { Card, Checkbox } from "@platform/ui";
import { createQuery } from "@tanstack/solid-query";
import { createSignal } from "solid-js";
import { WithdrawalTable } from "../components/order/WithdrawalTable.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

export default function Withdrawals() {
  const [all, setAll] = createSignal(false);
  const query = createQuery(() => ({
    queryKey: tenantKey("withdrawals", all()),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/withdrawals", {
          params: { header: tenantHeader(), query: { open: !all() } },
        }),
      ),
  }));
  return (
    <>
      <PageHeader title={t("fulfillment.withdrawals")} />
      <Checkbox
        class="mb-4"
        label={t("fulfillment.allWithdrawals")}
        checked={all()}
        onChange={setAll}
      />
      <QueryState query={query}>
        {(data) => (
          <Card padding="none">
            <WithdrawalTable items={data.items} />
          </Card>
        )}
      </QueryState>
    </>
  );
}
