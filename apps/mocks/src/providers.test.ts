import { afterEach, expect, test, vi } from "vitest";
import { app } from "./app.ts";
import { fixingDate, isBusinessDay } from "./cnb.ts";

const password = "a".repeat(32);
const attributes =
  "<number>100001</number><name>Jana</name><surname>Nováková</surname><email>a@b.cz</email><addressId>4101</addressId><cod>129.00</cod><value>129.00</value><currency>CZK</currency><weight>1.5</weight><eshop>Demo</eshop>";
const xml = async (method: string, content: string, apiPassword = password) => {
  const response = await app.request("/packeta/api/rest", {
    method: "POST",
    headers: { "content-type": "text/xml" },
    body: `<${method}><apiPassword>${apiPassword}</apiPassword>${content}</${method}>`,
  });
  expect(response.status).toBe(200);
  expect(response.headers.get("content-type")).toContain("xml");
  return response.text();
};
const jsonPost = (path: string, body: unknown, token?: string) =>
  app.request(path, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(body),
  });
afterEach(() => vi.useRealTimers());

test("Packeta create, PDF, status and control preserve provider shapes", async () => {
  const created = await xml("createPacket", `<packetAttributes>${attributes}</packetAttributes>`);
  const id = /<id>(\d+)<\/id>/.exec(created)?.[1];
  expect(id).toMatch(/^200\d{7}$/);
  expect(created).toContain(`<barcode>Z${id}</barcode>`);
  expect(created).toMatch(/<barcodeText>Z 200 \d{4} \d{3}<\/barcodeText>/);
  const label = await xml(
    "packetLabelPdf",
    `<packetId>${id}</packetId><format>A6 on A6</format><offset>0</offset>`,
  );
  expect(
    Buffer.from(/<result>([^<]+)<\/result>/.exec(label)?.[1] ?? "", "base64").toString(),
  ).toContain("%PDF-1.4");
  expect(await xml("packetStatus", `<packetId>${id}</packetId>`)).toContain(
    "<statusCode>1</statusCode>",
  );
  expect((await jsonPost(`/packeta/_packets/${id}/status`, { statusCode: 7 })).status).toBe(200);
  expect(await xml("packetStatus", `<packetId>${id}</packetId>`)).toContain(
    "<statusCode>7</statusCode>",
  );
  const list = await app.request("/packeta/_packets?number=100001");
  expect(await list.json()).toContainEqual({
    id,
    barcode: `Z${id}`,
    number: "100001",
    addressId: "4101",
    cod: "129.00",
    value: "129.00",
    currency: "CZK",
    statusCode: 7,
  });
  expect((await jsonPost(`/packeta/_packets/${id}/status`, { statusCode: 8 })).status).toBe(400);
  expect((await jsonPost(`/packeta/_packets/${id}/status`, { statusCode: "7" })).status).toBe(400);
  expect((await jsonPost("/packeta/_packets/missing/status", { statusCode: 7 })).status).toBe(404);
});

test("Packeta faults, home delivery and all-country pickup validation", async () => {
  expect(await xml("createPacket", "", "bad")).toContain("IncorrectApiPasswordFault");
  expect(await xml("unknown", "")).toContain("IncorrectDataFault");
  for (const body of [
    "not xml",
    "<createPacket><name></createPacket>",
    "<createPacket><",
    "<createPacket/>&bad;",
    "<!DOCTYPE x><createPacket/>",
  ]) {
    const response = await app.request("/packeta/api/rest", { method: "POST", body });
    expect(await response.text()).toContain("IncorrectDataFault");
  }
  for (const method of ["packetStatus", "packetLabelPdf"])
    expect(await xml(method, "<packetId>404</packetId>")).toContain("PacketIdFault");
  for (const value of [
    attributes.replace("4101", "999"),
    attributes.replace("129.00</value>", "bad</value>"),
    attributes.replace("<email>a@b.cz</email>", ""),
    attributes.replace("4101", "106"),
  ])
    expect(await xml("createPacket", `<packetAttributes>${value}</packetAttributes>`)).toContain(
      "PacketAttributesFault",
    );
  for (const id of ["5101", "106", "131"])
    expect(
      await xml(
        "createPacket",
        `<packetAttributes>${attributes.replace("4101", id)}<street>Dlouhá &amp; Krátká</street><city>Praha</city><zip>11000</zip></packetAttributes>`,
      ),
    ).toContain("<status>ok</status>");
  expect(
    await (
      await jsonPost("/packeta/pps/api/widget/v1/validate", {
        apiKey: "a".repeat(16),
        point: { id: "5101" },
      })
    ).json(),
  ).toEqual({ isValid: true, errors: [] });
  const missing = await jsonPost("/packeta/pps/api/widget/v1/validate", {
    apiKey: "a".repeat(16),
    point: { id: "106" },
  });
  expect(missing.status).toBe(200);
  expect(await missing.json()).toMatchObject({
    isValid: false,
    errors: [{ code: "PointNotFound" }],
  });
  expect(
    (await jsonPost("/packeta/pps/api/widget/v1/validate", { apiKey: "invalid" })).status,
  ).toBe(401);
});
const tokenRequest = (extra = "") =>
  app.request("/ppl/login/getAccessToken", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: `grant_type=client_credentials&client_id=demo&client_secret=secret123&scope=myapi2${extra}`,
  });
async function token(): Promise<string> {
  const response = await tokenRequest();
  expect(response.status).toBe(200);
  const body = (await response.json()) as {
    access_token: string;
    token_type: string;
    expires_in: number;
  };
  expect(body).toMatchObject({ token_type: "Bearer", expires_in: 1800 });
  return body.access_token;
}
const shipment = {
  referenceId: "100001",
  productType: "PRIV",
  recipient: {
    name: "Jana Nováková",
    street: "Dlouhá 1",
    city: "Praha",
    zipCode: "11000",
    country: "CZ",
  },
  shipmentSet: { numberOfShipments: 1 },
};
const batch = (shipments: unknown[]) => ({
  labelSettings: {
    format: "Pdf",
    dpi: 300,
    completeLabelSettings: { isCompleteLabelRequested: false },
  },
  shipments,
});

test("PPL token, batch, origin-relative label and tracking lifecycle", async () => {
  const access = await token();
  const headers = { authorization: `Bearer ${access}` };
  const created = await jsonPost(
    "/ppl/shipment/batch",
    batch([
      shipment,
      {
        ...shipment,
        referenceId: "100002",
        productType: "PRID",
        cashOnDelivery: { codCurrency: "CZK", codPrice: 129, codVarSym: "100002" },
      },
    ]),
    access,
  );
  expect(created.status).toBe(201);
  expect(await created.text()).toBe("");
  const location = created.headers.get("location");
  expect(location).toMatch(/^\/shipment\/batch\/[a-f0-9-]{36}$/);
  const response = await app.request(`http://mocks:4010/ppl${location}`, { headers });
  const result = (await response.json()) as {
    items: {
      referenceId: string;
      shipmentNumber: string;
      labelUrl: string;
      importState: string;
      errorMessage: null;
    }[];
  };
  expect(result.items).toHaveLength(2);
  for (const item of result.items) {
    expect(item).toMatchObject({ importState: "Complete", errorMessage: null });
    expect(item.shipmentNumber).toMatch(/^400\d{8}$/);
    expect(item.labelUrl).toMatch(/^http:\/\/mocks:4010\/ppl\/shipment\/batch\//);
    const label = await app.request(item.labelUrl, { headers });
    expect(label.headers.get("content-type")).toBe("application/pdf");
    expect(await label.text()).toMatch(/^%PDF-1.4/);
  }
  const number = result.items[0]?.shipmentNumber;
  const trackingUrl = `/ppl/shipment?${result.items.map((item) => `ShipmentNumbers=${item.shipmentNumber}`).join("&")}`;
  expect(await (await app.request(trackingUrl, { headers })).json()).toMatchObject([
    { trackAndTrace: { phase: "Order" } },
    { trackAndTrace: { phase: "Order" } },
  ]);
  expect((await jsonPost(`/ppl/_shipments/${number}/phase`, { phase: "Delivered" })).status).toBe(
    200,
  );
  const tracking = await (await app.request(trackingUrl, { headers })).json();
  expect(tracking).toMatchObject([
    { trackAndTrace: { phase: "Delivered", events: [{ phase: "Order" }, { phase: "Delivered" }] } },
    { trackAndTrace: { phase: "Order" } },
  ]);
  expect((await jsonPost(`/ppl/_shipments/${number}/phase`, { phase: "wrong" })).status).toBe(400);
  expect((await jsonPost("/ppl/_shipments/missing/phase", { phase: "Delivered" })).status).toBe(
    404,
  );
  expect(
    (
      await app.request(`http://mocks:4010/ppl${location}/label?shipmentNumber=00000000000`, {
        headers,
      })
    ).status,
  ).toBe(404);
});

test("PPL authentication, expiration and validation faults", async () => {
  expect(
    (
      await app.request("/ppl/login/getAccessToken", {
        method: "POST",
        body: "client_id=x&client_secret=short",
      })
    ).status,
  ).toBe(401);
  expect(
    (
      await app.request("/ppl/login/getAccessToken", {
        method: "POST",
        body: "client_id=demo&client_secret=longsecret&grant_type=password",
      })
    ).status,
  ).toBe(400);
  for (const url of [
    "/ppl/shipment",
    "/ppl/shipment/batch/missing",
    "/ppl/shipment/batch/missing/label",
  ])
    expect((await app.request(url)).status).toBe(401);
  expect((await jsonPost("/ppl/shipment/batch", batch([shipment]))).status).toBe(401);
  const access = await token();
  for (const list of [
    [],
    Array.from({ length: 21 }, () => shipment),
    [{ ...shipment, referenceId: "" }],
    [{ ...shipment, productType: "wrong" }],
    [{ ...shipment, recipient: {} }],
    [{ ...shipment, recipient: { ...shipment.recipient, country: "CZE" } }],
    [{ ...shipment, productType: "PRID" }],
    [{ ...shipment, cashOnDelivery: {} }],
    [{ ...shipment, productType: "BUSD", cashOnDelivery: { codPrice: -1 } }],
  ]) {
    const response = await jsonPost("/ppl/shipment/batch", batch(list), access);
    expect(response.status).toBe(400);
    expect(await response.json()).toHaveProperty("errors");
  }
  const headers = { authorization: `Bearer ${access}` };
  expect((await app.request("/ppl/shipment/batch/missing", { headers })).status).toBe(404);
  for (const query of [
    "",
    "?ShipmentNumbers=bad",
    `?${Array.from({ length: 51 }, () => "ShipmentNumbers=40000000001").join("&")}`,
  ])
    expect((await app.request(`/ppl/shipment${query}`, { headers })).status).toBe(400);
  vi.useFakeTimers();
  vi.setSystemTime(Date.now() + 1_800_001);
  expect((await app.request("/ppl/shipment", { headers })).status).toBe(401);
});

test("ČNB business days include Czech fixed and movable holidays", () => {
  for (const [day, expected] of [
    ["2026-09-27", "2026-09-25"],
    ["2026-09-28", "2026-09-25"],
    ["2026-04-03", "2026-04-02"],
    ["2026-04-06", "2026-04-02"],
    ["2026-01-01", "2025-12-31"],
  ]) {
    const date = new Date(`${day}T00:00:00Z`);
    expect(isBusinessDay(date)).toBe(false);
    expect(fixingDate(date).toISOString().slice(0, 10)).toBe(expected);
  }
  expect(isBusinessDay(new Date("2026-09-25T00:00:00Z"))).toBe(true);
});

test("ČNB fixing text, fallback, invalid dates, future clamp and Prague today", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-09-29T12:00:00Z"));
  for (const date of ["25.09.2026", "27.09.2026", "28.09.2026"]) {
    const response = await app.request(`/cnb/denni_kurz.txt?date=${date}`);
    expect(response.headers.get("content-type")?.toLowerCase()).toBe("text/plain; charset=utf-8");
    const text = await response.text();
    expect(text).toMatch(/^25\.09\.2026 #186\nzemě\|měna\|množství\|kód\|kurz\n/);
    expect(text).toContain("EMU|euro|1|EUR|24,305");
    expect(text).toContain("Maďarsko|forint|100|HUF|6,321");
    expect(text.trim().split("\n")).toHaveLength(17);
  }
  for (const date of ["31.02.2026", "2026-09-25", "", "01.13.2026"])
    expect((await app.request(`/cnb/denni_kurz.txt?date=${date}`)).status).toBe(400);
  const today = await (await app.request("/cnb/denni_kurz.txt")).text();
  expect(await (await app.request("/cnb/denni_kurz.txt?date=01.01.2099")).text()).toBe(today);
  expect(today).toMatch(/^29\.09\.2026/);
  vi.setSystemTime(new Date("2026-09-28T22:30:00Z"));
  expect(await (await app.request("/cnb/denni_kurz.txt")).text()).toBe(today);
  expect(await (await app.request("/cnb/denni_kurz.txt?date=06.04.2026")).text()).toMatch(
    /^02\.04\.2026/,
  );
});

test("Packeta rejects field faults with details and supports every documented status", async () => {
  const invalid = await xml(
    "createPacket",
    `<packetAttributes>${attributes.replace("4101", "404")}</packetAttributes>`,
  );
  expect(invalid).toContain(
    "<detail><attributes><fault><name>addressId</name><fault>Unknown address id</fault></fault></attributes></detail>",
  );
  const created = await xml(
    "createPacket",
    `<packetAttributes>${attributes.replace("100001", "escaped&amp;number").replace("Jana", "Jana &lt;J&gt;")}</packetAttributes>`,
  );
  const id = /<id>(\d+)<\/id>/.exec(created)?.[1];
  expect(
    await (await app.request("/packeta/_packets?number=escaped%26number")).json(),
  ).toMatchObject([{ number: "escaped&number" }]);
  for (const code of [1, 2, 3, 4, 5, 6, 7, 9, 10, 11]) {
    expect((await jsonPost(`/packeta/_packets/${id}/status`, { statusCode: code })).status).toBe(
      200,
    );
    const status = await xml("packetStatus", `<packetId>${id}</packetId>`);
    expect(status).toContain(`<statusCode>${code}</statusCode>`);
    expect(status).toContain(`<isReturning>${code === 9 || code === 10}</isReturning>`);
  }
});

test("PPL rejects malformed JSON and invalid label settings without partial imports", async () => {
  const access = await token();
  const headers = { authorization: `Bearer ${access}`, "content-type": "application/json" };
  const invalid = await app.request("/ppl/shipment/batch", { method: "POST", headers, body: "{" });
  expect(invalid.status).toBe(400);
  expect((await jsonPost("/ppl/shipment/batch", { shipments: [shipment] }, access)).status).toBe(
    400,
  );
  expect(
    (
      await jsonPost(
        "/ppl/shipment/batch",
        batch([shipment, { ...shipment, referenceId: null }]),
        access,
      )
    ).status,
  ).toBe(400);
  const created = await jsonPost(
    "/ppl/shipment/batch",
    batch([
      { ...shipment, productType: "BUSS" },
      {
        ...shipment,
        productType: "BUSD",
        cashOnDelivery: { codCurrency: "CZK", codPrice: 10, codVarSym: "100001" },
      },
    ]),
    access,
  );
  expect(created.status).toBe(201);
  const result = (await (
    await app.request(`/ppl${created.headers.get("location")}`, { headers })
  ).json()) as { items: { shipmentNumber: string }[] };
  const number = result.items[0]?.shipmentNumber;
  for (const phase of [
    "InTransport",
    "Delivering",
    "PickupPoint",
    "Returning",
    "BackToSender",
    "Canceled",
  ]) {
    expect((await jsonPost(`/ppl/_shipments/${number}/phase`, { phase })).status).toBe(200);
    expect(
      await (await app.request(`/ppl/shipment?ShipmentNumbers=${number}`, { headers })).json(),
    ).toMatchObject([{ trackAndTrace: { phase } }]);
  }
});
