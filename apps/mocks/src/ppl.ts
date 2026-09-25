import { randomUUID } from "node:crypto";
import type { Hono } from "hono";
import { labelPdf } from "./pdf.ts";

/** PPL CPL MyAPI2 stand-in: https://api.dhl.com/ecs/ppl/myapi2. In-memory batches and OAuth tokens. */
const tokens = new Map<string, number>();
const batches = new Map<string, Shipment[]>();
const shipments = new Map<string, Shipment>();
const MAX_TOKENS = 1_000;
const MAX_SHIPMENTS = 10_000;
const MAX_EVENTS = 100;
let nextNumber = 40_000_000_001;
const phases = [
  "Order",
  "InTransport",
  "Delivering",
  "PickupPoint",
  "Delivered",
  "Returning",
  "BackToSender",
  "Canceled",
] as const;
type Phase = (typeof phases)[number];
interface Event {
  code: string;
  name: string;
  eventDate: string;
  phase: Phase;
}
interface Shipment {
  referenceId: string;
  shipmentNumber: string;
  productType: string;
  recipient: string;
  events: Event[];
}
function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
function text(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0 && value.length <= 200;
}
function event(phase: Phase): Event {
  return {
    code: String(phases.indexOf(phase) + 1),
    name: phase,
    eventDate: new Date().toISOString(),
    phase,
  };
}

export function pplRoutes(app: Hono) {
  app.post("/ppl/login/getAccessToken", async (c) => {
    const form = new URLSearchParams(await c.req.text());
    if (
      !/^[A-Za-z0-9_-]{3,100}$/.test(form.get("client_id") ?? "") ||
      (form.get("client_secret")?.length ?? 0) < 8 ||
      (form.get("client_secret")?.length ?? 0) > 1000
    )
      return c.json({ error: "invalid_client" }, 401);
    if (form.get("grant_type") !== "client_credentials" || form.get("scope") !== "myapi2")
      return c.json({ error: "invalid_request" }, 400);
    for (const [token, expires] of tokens) if (expires <= Date.now()) tokens.delete(token);
    if (tokens.size >= MAX_TOKENS) return c.json({ error: "token storage full" }, 507);
    const token = randomUUID();
    tokens.set(token, Date.now() + 1_800_000);
    return c.json({ access_token: token, token_type: "Bearer", expires_in: 1800 });
  });
  // Test controls deliberately bypass the provider's authentication.
  app.post("/ppl/_shipments/:shipmentNumber/phase", async (c) => {
    const shipment = shipments.get(c.req.param("shipmentNumber"));
    if (!shipment) return c.json({ error: "Unknown shipment" }, 404);
    const body = record(await c.req.json().catch(() => null));
    const phase = phases.find((phase) => phase === body.phase);
    if (!phase) return c.json({ error: "Unknown phase" }, 400);
    if (shipment.events.length >= MAX_EVENTS) return c.json({ error: "Event storage full" }, 507);
    shipment.events.push(event(phase));
    return c.json({ phase });
  });
  app.use("/ppl/*", async (c, next) => {
    const token = /^Bearer (\S+)$/.exec(c.req.header("authorization") ?? "")?.[1];
    if (!token || (tokens.get(token) ?? 0) <= Date.now())
      return c.json({ error: "unauthorized" }, 401);
    await next();
  });
  app.post("/ppl/shipment/batch", async (c) => {
    const body = record(await c.req.json().catch(() => null));
    const list: unknown[] = Array.isArray(body.shipments) ? body.shipments : [];
    const errors: Record<string, string[]> = {};
    if (list.length < 1 || list.length > 20) errors.shipments = ["Expected 1..20 shipments"];
    const label = record(body.labelSettings);
    if (
      label.format !== "Pdf" ||
      label.dpi !== 300 ||
      record(label.completeLabelSettings).isCompleteLabelRequested !== false
    )
      errors.labelSettings = ["Expected Pdf, dpi 300, isCompleteLabelRequested false"];
    if (body.returnChannel !== undefined && record(body.returnChannel).type !== "None")
      errors.returnChannel = ["Expected type None"];
    const validated: { referenceId: string; productType: string; recipient: string }[] = [];
    for (const [index, raw] of list.slice(0, 20).entries()) {
      const s = record(raw);
      const prefix = `shipments[${index}]`;
      if (!text(s.referenceId))
        errors[`${prefix}.referenceId`] = ["Required, maximum 200 characters"];
      if (!text(s.productType) || !["BUSS", "PRIV", "BUSD", "PRID"].includes(s.productType))
        errors[`${prefix}.productType`] = ["Invalid product type"];
      const recipient = record(s.recipient);
      for (const key of ["name", "street", "city", "zipCode", "country"])
        if (!text(recipient[key]))
          errors[`${prefix}.recipient.${key}`] = ["Required, maximum 200 characters"];
      if (typeof recipient.country !== "string" || !/^[A-Za-z]{2}$/.test(recipient.country))
        errors[`${prefix}.recipient.country`] = ["Expected two-letter country"];
      for (const key of ["email", "phone"])
        if (recipient[key] !== undefined && !text(recipient[key]))
          errors[`${prefix}.recipient.${key}`] = ["Invalid text"];
      if (
        typeof recipient.email === "string" &&
        !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(recipient.email)
      )
        errors[`${prefix}.recipient.email`] = ["Invalid email"];
      if (s.note !== undefined && !text(s.note)) errors[`${prefix}.note`] = ["Invalid text"];
      const needsCod = s.productType === "BUSD" || s.productType === "PRID";
      if (needsCod !== (s.cashOnDelivery !== undefined && s.cashOnDelivery !== null))
        errors[`${prefix}.cashOnDelivery`] = [
          "COD products require cashOnDelivery and other products forbid it",
        ];
      if (needsCod) {
        const cod = record(s.cashOnDelivery);
        if (
          typeof cod.codCurrency !== "string" ||
          !/^[A-Z]{3}$/.test(cod.codCurrency) ||
          typeof cod.codPrice !== "number" ||
          !Number.isFinite(cod.codPrice) ||
          cod.codPrice <= 0 ||
          typeof cod.codVarSym !== "string" ||
          !/^\d{1,10}$/.test(cod.codVarSym)
        )
          errors[`${prefix}.cashOnDelivery`] = [
            "Invalid COD currency, positive price or variable symbol",
          ];
      }
      if (record(s.shipmentSet).numberOfShipments !== 1)
        errors[`${prefix}.shipmentSet`] = ["Expected numberOfShipments 1"];
      if (text(s.referenceId) && text(s.productType) && text(recipient.name))
        validated.push({
          referenceId: s.referenceId,
          productType: s.productType,
          recipient: recipient.name,
        });
    }
    if (Object.keys(errors).length) return c.json({ errors }, 400);
    if (shipments.size + validated.length > MAX_SHIPMENTS)
      return c.json({ error: "Shipment storage full" }, 507);
    const batchId = randomUUID();
    const batch = validated.map(
      (s): Shipment => ({ ...s, shipmentNumber: String(nextNumber++), events: [event("Order")] }),
    );
    batches.set(batchId, batch);
    for (const shipment of batch) shipments.set(shipment.shipmentNumber, shipment);
    c.header("Location", `/shipment/batch/${batchId}`);
    return c.body(null, 201);
  });
  app.get("/ppl/shipment/batch/:batchId", (c) => {
    const batchId = c.req.param("batchId");
    const batch = batches.get(batchId);
    if (!batch) return c.json({ error: "Unknown batch" }, 404);
    return c.json({
      items: batch.map((s) => ({
        referenceId: s.referenceId,
        shipmentNumber: s.shipmentNumber,
        importState: "Complete",
        labelUrl: `${new URL(c.req.url).origin}/ppl/shipment/batch/${batchId}/label?shipmentNumber=${s.shipmentNumber}`,
        errorMessage: null,
      })),
    });
  });
  app.get("/ppl/shipment/batch/:batchId/label", (c) => {
    const shipment = batches
      .get(c.req.param("batchId"))
      ?.find((s) => s.shipmentNumber === c.req.query("shipmentNumber"));
    if (!shipment) return c.json({ error: "Unknown shipment in batch" }, 404);
    return c.body(labelPdf("PPL (mock)", shipment.shipmentNumber, shipment.recipient), 200, {
      "content-type": "application/pdf",
    });
  });
  app.get("/ppl/shipment", (c) => {
    const numbers = c.req.queries("ShipmentNumbers") ?? [];
    if (
      numbers.length < 1 ||
      numbers.length > 50 ||
      numbers.some((number) => !/^\d{11}$/.test(number))
    )
      return c.json(
        { errors: { ShipmentNumbers: ["Expected 1..50 eleven-digit shipment numbers"] } },
        400,
      );
    return c.json(
      numbers.flatMap((number) => {
        const s = shipments.get(number);
        const last = s?.events.at(-1);
        return s && last
          ? [
              {
                shipmentNumber: s.shipmentNumber,
                productType: s.productType,
                trackAndTrace: {
                  lastEventCode: last.code,
                  lastEventDate: last.eventDate,
                  phase: last.phase,
                  events: s.events,
                },
              },
            ]
          : [];
      }),
    );
  });
}
