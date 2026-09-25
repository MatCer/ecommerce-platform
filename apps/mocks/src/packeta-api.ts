import type { Hono } from "hono";
import { POINTS } from "./packeta.ts";
import { labelPdf } from "./pdf.ts";

/** Packeta REST/XML API: https://www.zasilkovna.cz/api/rest. Faults intentionally use HTTP 200. */
const packets = new Map<string, Packet>();
const MAX_PACKETS = 10_000;
let nextId = 2_000_000_001;
interface Packet {
  id: string;
  barcode: string;
  number: string;
  addressId: string;
  cod: string | null;
  value: string;
  currency: string;
  statusCode: number;
  recipient: string;
  updated: string;
}
const statuses: Record<number, string> = {
  1: "received data",
  2: "arrived",
  3: "prepared for departure",
  4: "departed",
  5: "ready for pickup",
  6: "handed to carrier",
  7: "delivered",
  9: "posted back",
  10: "returned",
  11: "cancelled",
};
const pointIds = new Set(
  Object.values(POINTS)
    .flat()
    .map((point) => point.id),
);
const escapeXml = (value: string) =>
  value.replace(
    /[&<>"']/g,
    (char) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[char] ?? char,
  );
const decodeXml = (value: string) =>
  value.replace(
    /&(amp|lt|gt|quot|apos);/g,
    (_, entity: string) => ({ amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" })[entity] ?? "",
  );
const field = (xml: string, name: string) =>
  decodeXml(new RegExp(`<${name}>([^<]*)</${name}>`).exec(xml)?.[1]?.trim() ?? "");
const fault = (name: string, message: string, detail = "") =>
  `<response><status>fault</status><fault>${name}</fault><string>${escapeXml(message)}</string>${detail}</response>`;
const ok = (result: string) => `<response><status>ok</status><result>${result}</result></response>`;

// Deliberately small mock XML dialect: balanced plain tags, optional declaration, no DTDs.
function rootMethod(body: string): string | null {
  const xml = body.replace(/^\s*<\?xml[^?]*\?>/, "").trim();
  if (!xml || xml.length > 32_000 || /&(?!(?:amp|lt|gt|quot|apos);)/.test(xml)) return null;
  const stack: string[] = [];
  let root: string | null = null;
  for (const token of xml.match(/<[^>]*>|[^<]+/g) ?? []) {
    if (!token.startsWith("<")) {
      if (!stack.length && token.trim()) return null;
      continue;
    }
    const tag = /^<(\/?)([A-Za-z][A-Za-z0-9]*)(\s*\/?)>$/.exec(token);
    if (!tag) return null;
    const name = tag[2] ?? "";
    if (tag[1]) {
      if (tag[3]?.trim() || stack.pop() !== name) return null;
    } else {
      if (!stack.length) {
        if (root) return null;
        root = name;
      }
      if (!tag[3]?.includes("/")) stack.push(name);
    }
  }
  return stack.length === 0 && !xml.endsWith("<") ? root : null;
}

export function packetaApiRoutes(app: Hono) {
  app.post("/packeta/api/rest", async (c) => {
    const body = await c.req.text();
    c.header("content-type", "application/xml; charset=utf-8");
    const method = rootMethod(body);
    if (!method || !["createPacket", "packetLabelPdf", "packetStatus"].includes(method))
      return c.body(fault("IncorrectDataFault", "Invalid XML or method"));
    if (!/^[a-f0-9]{32}$/.test(field(body, "apiPassword")))
      return c.body(fault("IncorrectApiPasswordFault", "Incorrect API password"));
    if (method === "createPacket") {
      const attributes = /<packetAttributes>([\s\S]*?)<\/packetAttributes>/.exec(body)?.[1] ?? "";
      const get = (name: string) => field(attributes, name);
      const errors: [string, string][] = [];
      for (const name of ["number", "name", "surname", "value", "eshop"])
        if (!get(name) || get(name).length > 200)
          errors.push([name, "Required, maximum 200 characters"]);
      const addressId = get("addressId");
      const home = ["106", "131"].includes(addressId);
      if (!home && !pointIds.has(addressId)) errors.push(["addressId", "Unknown address id"]);
      if (home)
        for (const name of ["street", "city", "zip"])
          if (!get(name)) errors.push([name, "Required for home delivery"]);
      if (!get("email") && !get("phone")) errors.push(["email", "Email or phone required"]);
      if (get("email") && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(get("email")))
        errors.push(["email", "Invalid email"]);
      if (get("phone") && !/^\+?[0-9 ()-]{6,30}$/.test(get("phone")))
        errors.push(["phone", "Invalid phone"]);
      for (const name of ["value", "cod", "weight"]) {
        const value = get(name);
        if (
          value &&
          (!/^\d+(?:\.\d+)?$/.test(value) ||
            !Number.isFinite(Number(value)) ||
            (name === "weight" && Number(value) <= 0))
        )
          errors.push([name, "Invalid positive decimal"]);
      }
      if (get("currency") && !/^[A-Z]{3}$/.test(get("currency")))
        errors.push(["currency", "Invalid currency"]);
      if (errors.length)
        return c.body(
          fault(
            "PacketAttributesFault",
            "Invalid packet attributes",
            `<detail><attributes>${errors.map(([name, message]) => `<fault><name>${name}</name><fault>${message}</fault></fault>`).join("")}</attributes></detail>`,
          ),
        );
      if (packets.size >= MAX_PACKETS)
        return c.body(fault("IncorrectDataFault", "Packet storage full"));
      const id = String(nextId++);
      const packet: Packet = {
        id,
        barcode: `Z${id}`,
        number: get("number"),
        addressId,
        cod: get("cod") || null,
        value: get("value"),
        currency: get("currency") || "CZK",
        statusCode: 1,
        recipient: `${get("name")} ${get("surname")}`,
        updated: new Date().toISOString().slice(0, 19),
      };
      packets.set(id, packet);
      return c.body(
        ok(
          `<id>${id}</id><barcode>${packet.barcode}</barcode><barcodeText>Z ${id.slice(0, 3)} ${id.slice(3, 7)} ${id.slice(7)}</barcodeText>`,
        ),
      );
    }
    const packet = packets.get(field(body, "packetId"));
    if (!packet) return c.body(fault("PacketIdFault", "Unknown packet"));
    if (method === "packetLabelPdf")
      return c.body(
        ok(labelPdf("PACKETA (mock)", packet.barcode, packet.recipient).toString("base64")),
      );
    return c.body(
      ok(
        `<dateTime>${packet.updated}</dateTime><statusCode>${packet.statusCode}</statusCode><codeText>${statuses[packet.statusCode]}</codeText><statusText>${packet.statusCode === 1 ? "Packet data received" : statuses[packet.statusCode]}</statusText><branchId>0</branchId><destinationBranchId>${escapeXml(packet.addressId)}</destinationBranchId><externalTrackingCode></externalTrackingCode><isReturning>${[9, 10].includes(packet.statusCode)}</isReturning><storedUntil></storedUntil>`,
      ),
    );
  });
  app.post("/packeta/_packets/:id/status", async (c) => {
    const packet = packets.get(c.req.param("id"));
    if (!packet) return c.json({ error: "Unknown packet" }, 404);
    const body: unknown = await c.req.json().catch(() => null);
    const code =
      typeof body === "object" && body !== null && "statusCode" in body ? body.statusCode : null;
    if (typeof code !== "number" || !Object.hasOwn(statuses, code))
      return c.json({ error: "Unknown status code" }, 400);
    packet.statusCode = code;
    packet.updated = new Date().toISOString().slice(0, 19);
    return c.json({ statusCode: code });
  });
  app.get("/packeta/_packets", (c) =>
    c.json(
      [...packets.values()]
        .filter((p) => !c.req.query("number") || p.number === c.req.query("number"))
        .map(({ recipient: _recipient, updated: _updated, ...packet }) => packet),
    ),
  );
  app.post("/packeta/pps/api/widget/v1/validate", async (c) => {
    const raw: unknown = await c.req.json().catch(() => null);
    const body = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {};
    if (typeof body.apiKey !== "string" || !/^[a-f0-9]{16}$/.test(body.apiKey))
      return c.json(
        { isValid: false, errors: [{ code: "InvalidApiKey", description: "Invalid API key" }] },
        401,
      );
    const point = body.point;
    const valid =
      typeof point === "object" &&
      point !== null &&
      "id" in point &&
      typeof point.id === "string" &&
      pointIds.has(point.id);
    return c.json({
      isValid: valid,
      errors: valid ? [] : [{ code: "PointNotFound", description: "Pickup point not found" }],
    });
  });
}
