import { describe, expect, it, vi } from "vitest";
import {
  eventContent,
  needsRefundIban,
  pollDocument,
  refundInput,
  safeDownloadUrl,
} from "./fulfillment.ts";

describe("refund input", () => {
  const form = {
    quantities: ["2", "0"],
    shipping: true,
    paymentFee: false,
    reason: " damaged ",
    iban: " CZ12 3456 ",
  };
  const lines = [
    { id: "line-1", quantity: 3 },
    { id: "line-2", quantity: 1 },
  ];
  it("uses order line IDs, omits zero quantities and normalizes optional fields", () => {
    expect(refundInput(lines, form, true)).toEqual({
      lines: [{ order_line_id: "line-1", quantity: 2 }],
      shipping: true,
      payment_fee: false,
      reason: "damaged",
      iban: "CZ123456",
    });
  });
  it("rejects fractions, negatives, over-refunds and a missing required IBAN", () => {
    for (const quantity of ["", "-1", "1.5", "4", "NaN"])
      expect(refundInput(lines, { ...form, quantities: [quantity, "0"] }, false)).toBeNull();
    expect(refundInput(lines, { ...form, iban: " " }, true)).toBeNull();
  });
  it("requires at least one good or charge", () => {
    expect(
      refundInput(lines, { ...form, quantities: ["0", "0"], shipping: false }, false),
    ).toBeNull();
  });
  it("only requires bank details for bank transfer or COD", () => {
    expect(needsRefundIban("bank_transfer")).toBe(true);
    expect(needsRefundIban("cod")).toBe(true);
    expect(needsRefundIban("stripe")).toBe(false);
  });
});

describe("timeline", () => {
  it("reads notes and transitions, tolerates unknown payloads", () => {
    expect(eventContent("note", { note: "Call customer" }).detail).toBe("Call customer");
    expect(eventContent("status_changed", { from: "confirmed", to: "processing" }).detail).toBe(
      "confirmed → processing",
    );
    expect(eventContent("future_event", { nested: true })).toEqual({
      kind: "future_event",
      key: null,
      detail: '{"nested":true}',
      warning: false,
    });
    expect(eventContent("invoice_delayed", null).warning).toBe(true);
  });
});

describe("document polling", () => {
  it("waits for ready, stops on failed and bounds polling", async () => {
    const wait = vi.fn(async () => {});
    let calls = 0;
    const ready = await pollDocument(
      async () => ({ status: ++calls === 2 ? "ready" : "pending", url: "https://example.com/pdf" }),
      new AbortController().signal,
      wait,
    );
    expect(ready.url).toBe("https://example.com/pdf");
    expect(wait).toHaveBeenCalledTimes(2);
    await expect(
      pollDocument(
        async () => ({ status: "failed", error: "render failed" }),
        new AbortController().signal,
        wait,
      ),
    ).rejects.toThrow("render failed");
    const fetch = vi.fn(async () => ({ status: "pending" }));
    await expect(pollDocument(fetch, new AbortController().signal, wait)).rejects.toThrow(
      "document_timeout",
    );
    expect(fetch).toHaveBeenCalledTimes(40);
  });
  it("does not fetch after cancellation", async () => {
    const controller = new AbortController();
    controller.abort();
    const fetch = vi.fn(async () => ({ status: "ready" }));
    await expect(pollDocument(fetch, controller.signal)).rejects.toThrow();
    expect(fetch).not.toHaveBeenCalled();
  });
  it("cancels an active wait and ignores a late ready response", async () => {
    vi.useFakeTimers();
    try {
      const controller = new AbortController();
      const read = vi.fn(async () => ({ status: "ready" }));
      const pending = pollDocument(read, controller.signal);
      const rejected = expect(pending).rejects.toThrow();
      controller.abort();
      await rejected;
      expect(read).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
      const lateController = new AbortController();
      await expect(
        pollDocument(
          async () => {
            lateController.abort();
            return { status: "ready" };
          },
          lateController.signal,
          async () => {},
        ),
      ).rejects.toThrow();
    } finally {
      vi.useRealTimers();
    }
  });
  it("rejects unsafe links", () => {
    expect(safeDownloadUrl("javascript:alert(1)")).toBeNull();
    expect(safeDownloadUrl("https://example.com/file.pdf")).toBe("https://example.com/file.pdf");
  });
});
