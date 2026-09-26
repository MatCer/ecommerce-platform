import { Button, Dialog, showToast } from "@platform/ui";
import { createMutation } from "@tanstack/solid-query";
import { createSignal, Show } from "solid-js";
import { t } from "../../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../../lib/api.ts";
import { useMembership } from "../../lib/me.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { useFulfillmentRefresh } from "./shared.tsx";

export function RefundException(props: { orderId: string }) {
  const { can } = useMembership();
  const refresh = useFulfillmentRefresh();
  const [open, setOpen] = createSignal(false);
  const refund = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/exception/refund", {
          params: { header: tenantHeader(), path: { id: props.orderId } },
        }),
      ),
    onSuccess: () => {
      setOpen(false);
      showToast({ title: t("pay.resolved"), closeLabel: t("common.close") });
      void refresh();
    },
  }));
  return (
    <Show when={can("admin")}>
      <Button
        onClick={() => {
          refund.reset();
          setOpen(true);
        }}
      >
        {t("fulfillment.refundException")}
      </Button>
      <Dialog
        open={open()}
        onOpenChange={(value) => !refund.isPending && setOpen(value)}
        title={t("fulfillment.refundException")}
        description={t("fulfillment.refundExceptionConfirm")}
        size="sm"
        footer={
          <>
            <Button disabled={refund.isPending} onClick={() => setOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="confirm" loading={refund.isPending} onClick={() => refund.mutate()}>
              {t("fulfillment.refundException")}
            </Button>
          </>
        }
      >
        <ApiProblem error={refund.error} />
      </Dialog>
    </Show>
  );
}
