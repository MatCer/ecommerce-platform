import type { Schemas } from "./api.ts";

export interface RefundForm {
  quantities: string[];
  shipping: boolean;
  paymentFee: boolean;
  reason: string;
  iban: string;
}

export function needsRefundIban(method: string): boolean {
  return method === "bank_transfer" || method === "cod";
}

export function refundInput(
  lines: readonly { id: string; quantity: number }[],
  form: RefundForm,
  requireIban: boolean,
): Schemas["RefundInput"] | null {
  const iban = form.iban.replace(/\s/g, "").toUpperCase();
  if (requireIban && !iban) return null;
  const selected: NonNullable<Schemas["RefundInput"]["lines"]> = [];
  for (const [index, line] of lines.entries()) {
    const value = form.quantities[index] ?? "";
    const quantity = Number(value);
    if (!/^\d+$/.test(value) || !Number.isSafeInteger(quantity) || quantity > line.quantity)
      return null;
    if (quantity === 0) continue;
    selected.push({ order_line_id: line.id, quantity });
  }
  if (!selected.length && !form.shipping && !form.paymentFee) return null;
  return {
    lines: selected,
    shipping: form.shipping,
    payment_fee: form.paymentFee,
    reason: form.reason.trim() || null,
    iban: iban || null,
  };
}

const eventKinds = [
  "status_changed",
  "fulfillment_changed",
  "label_created",
  "label_cancelled",
  "invoice_issued",
  "credit_note_issued",
  "invoice_delayed",
  "refunded",
  "note",
  "address_changed",
  "withdrawal_declared",
  "withdrawal_goods_received",
  "withdrawal_proof_received",
  "returned_to_sender",
  "carrier_exception",
  "cancelled_by_merchant",
  "exception",
  "exception_resolved",
  "payment_status_changed",
  "cod_delivered",
  "cod_collected",
  "cod_remitted",
] as const;

export function eventContent(kind: string, data: unknown) {
  const key: (typeof eventKinds)[number] | "cod_other" | null =
    eventKinds.find((value) => value === kind) ?? (kind.startsWith("cod_") ? "cod_other" : null);
  let detail = data == null ? "" : JSON.stringify(data);
  if (typeof data === "object" && data !== null) {
    const note: unknown = Reflect.get(data, "note");
    const from: unknown = Reflect.get(data, "from");
    const to: unknown = Reflect.get(data, "to");
    if (kind === "note" && typeof note === "string") detail = note;
    else if (typeof from === "string" && typeof to === "string") detail = `${from} → ${to}`;
  }
  return { kind, key, detail: detail === "{}" ? "" : detail, warning: kind === "invoice_delayed" };
}

export function safeDownloadUrl(value: string): string | null {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:" ? url.href : null;
  } catch {
    return null;
  }
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}

export class DocumentError extends Error {
  readonly code: "document_failed" | "document_timeout";
  readonly detail: string | undefined;
  constructor(code: "document_failed" | "document_timeout", detail?: string) {
    super(detail || code);
    this.code = code;
    this.detail = detail;
  }
}

/** Poll at most 40 times (60s); the caller also aborts network requests on navigation. */
export async function pollDocument<T extends { status: string; error?: string | null }>(
  read: () => Promise<T>,
  signal: AbortSignal,
  wait = delay,
): Promise<T> {
  for (let attempt = 0; attempt < 40; attempt++) {
    signal.throwIfAborted();
    await wait(1500, signal);
    signal.throwIfAborted();
    const document = await read();
    signal.throwIfAborted();
    if (document.status === "ready") return document;
    if (document.status === "failed")
      throw new DocumentError("document_failed", document.error ?? undefined);
  }
  throw new DocumentError("document_timeout");
}
