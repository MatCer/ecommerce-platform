import type { Hono } from "hono";

/**
 * Fio banka API stand-in (WP11): the worker polls
 * `GET /fio/v1/rest/periods/{token}/{from}/{to}/transactions.json` (the real API's path under
 * `https://fioapi.fio.cz`) and gets the account statement JSON with numbered columns.
 * Tests and demos add incoming payments with `POST /fio/_transactions`. In-memory.
 */
interface Tx {
  id: number;
  date: string;
  amount: number;
  currency: string;
  vs: string | null;
  counterparty: string | null;
  name: string | null;
  message: string | null;
}

const accounts = new Map<string, Tx[]>();
const MAX_TOKENS = 1_000;
const MAX_TX = 10_000;
let nextId = 20_000_000_001;

const TOKEN_RE = /^[A-Za-z0-9]{8,200}$/;
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

function text(v: unknown, max: number): string | null {
  return typeof v === "string" && v.length > 0 && v.length <= max ? v : null;
}

function column(id: number, name: string, value: string | number | null) {
  return value === null ? null : { value, name, id };
}

export function fioRoutes(app: Hono) {
  app.post("/fio/_transactions", async (c) => {
    const body: unknown = await c.req.json().catch(() => null);
    const b = typeof body === "object" && body !== null ? (body as Record<string, unknown>) : {};
    const token = typeof b.token === "string" && TOKEN_RE.test(b.token) ? b.token : null;
    const list = Array.isArray(b.transactions) ? b.transactions : null;
    if (!token || !list || list.length > 100)
      return c.json({ error: "expected { token, transactions: [...] }" }, 400);
    if (!accounts.has(token) && accounts.size >= MAX_TOKENS) return c.json({ error: "full" }, 507);
    const txs = accounts.get(token) ?? [];
    for (const raw of list) {
      const t = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {};
      const amount = typeof t.amount === "number" && Number.isFinite(t.amount) ? t.amount : null;
      const date = typeof t.date === "string" && DATE_RE.test(t.date) ? t.date : null;
      if (amount === null || date === null)
        return c.json({ error: "amount and date required" }, 400);
      if (txs.length >= MAX_TX) return c.json({ error: "full" }, 507);
      txs.push({
        id: nextId++,
        date,
        amount,
        currency: text(t.currency, 3) ?? "CZK",
        vs: text(t.vs, 10),
        counterparty: text(t.counterparty, 40),
        name: text(t.name, 200),
        message: text(t.message, 200),
      });
    }
    accounts.set(token, txs);
    return c.json({ token, count: txs.length });
  });

  app.get("/fio/v1/rest/periods/:token/:from/:to/transactions.json", (c) => {
    const { token, from, to } = c.req.param();
    if (!TOKEN_RE.test(token) || !DATE_RE.test(from) || !DATE_RE.test(to))
      return c.text("Bad request", 400);
    const txs = (accounts.get(token) ?? []).filter((t) => t.date >= from && t.date <= to);
    return c.json({
      accountStatement: {
        info: {
          accountId: "2000000000",
          bankId: "2010",
          currency: "CZK",
          iban: null,
          bic: "FIOBCZPPXXX",
          dateStart: `${from}+0200`,
          dateEnd: `${to}+0200`,
        },
        transactionList: {
          transaction: txs.map((t) => ({
            column22: column(22, "ID pohybu", t.id),
            column0: column(0, "Datum", `${t.date}+0200`),
            column1: column(1, "Objem", t.amount),
            column14: column(14, "Měna", t.currency),
            column2: column(2, "Protiúčet", t.counterparty),
            column10: column(10, "Název protiúčtu", t.name),
            column5: column(5, "VS", t.vs),
            column16: column(16, "Zpráva pro příjemce", t.message),
          })),
        },
      },
    });
  });
}
