import type { Hono } from "hono";

/** ČNB daily fixing text endpoint. Mock publishes at 00:00 Prague; real ČNB publishes around 14:30. */
const DAY = 86_400_000;
function easter(year: number): Date {
  const a = year % 19,
    b = Math.floor(year / 100),
    c = year % 100;
  const d = Math.floor(b / 4),
    e = b % 4,
    f = Math.floor((b + 8) / 25);
  const g = Math.floor((b - f + 1) / 3),
    h = (19 * a + b - d - g + 15) % 30;
  const i = Math.floor(c / 4),
    k = c % 4,
    l = (32 + 2 * e + 2 * i - h - k) % 7;
  const m = Math.floor((a + 11 * h + 22 * l) / 451);
  const n = h + l - 7 * m + 114;
  return new Date(Date.UTC(year, Math.floor(n / 31) - 1, (n % 31) + 1));
}
const holidays = new Set([
  "1-1",
  "5-1",
  "5-8",
  "7-5",
  "7-6",
  "9-28",
  "10-28",
  "11-17",
  "12-24",
  "12-25",
  "12-26",
]);
/** Dates represent UTC calendar days, independent of the host timezone. */
export function isBusinessDay(date: Date): boolean {
  if (!Number.isFinite(date.getTime())) return false;
  const sunday = easter(date.getUTCFullYear()).getTime();
  const day = Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate());
  return (
    ![0, 6].includes(date.getUTCDay()) &&
    !holidays.has(`${date.getUTCMonth() + 1}-${date.getUTCDate()}`) &&
    day !== sunday - 2 * DAY &&
    day !== sunday + DAY
  );
}
export function fixingDate(date: Date): Date {
  if (!Number.isFinite(date.getTime())) throw new RangeError("Invalid date");
  const result = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()));
  while (!isBusinessDay(result)) result.setUTCDate(result.getUTCDate() - 1);
  return result;
}
const rates: [string, string, number, string, number][] = [
  ["Austrálie", "dolar", 1, "AUD", 15.012],
  ["Brazílie", "real", 1, "BRL", 4.123],
  ["Kanada", "dolar", 1, "CAD", 16.012],
  ["Švýcarsko", "frank", 1, "CHF", 25.912],
  ["Čína", "žen-min-pi", 1, "CNY", 3.012],
  ["Dánsko", "koruna", 1, "DKK", 3.258],
  ["EMU", "euro", 1, "EUR", 24.305],
  ["Velká Británie", "libra", 1, "GBP", 28.123],
  ["Maďarsko", "forint", 100, "HUF", 6.321],
  ["Japonsko", "jen", 100, "JPY", 14.512],
  ["Norsko", "koruna", 1, "NOK", 2.101],
  ["Polsko", "zlotý", 1, "PLN", 5.712],
  ["Rumunsko", "leu", 1, "RON", 4.881],
  ["Švédsko", "koruna", 1, "SEK", 2.201],
  ["USA", "dolar", 1, "USD", 21.512],
];
function parseDate(value: string): Date | null {
  const match = /^(\d{2})\.(\d{2})\.(\d{4})$/.exec(value);
  if (!match) return null;
  const day = Number(match[1]),
    month = Number(match[2]),
    year = Number(match[3]);
  if (year < 1900 || year > 9999) return null;
  const date = new Date(Date.UTC(year, month - 1, day));
  return date.getUTCFullYear() === year &&
    date.getUTCMonth() === month - 1 &&
    date.getUTCDate() === day
    ? date
    : null;
}
export function cnbRoutes(app: Hono) {
  app.get("/cnb/denni_kurz.txt", (c) => {
    c.header("content-type", "text/plain; charset=utf-8");
    const parts = new Intl.DateTimeFormat("en-GB", {
      timeZone: "Europe/Prague",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(new Date());
    const part = (type: string) => parts.find((part) => part.type === type)?.value ?? "";
    const today = new Date(`${part("year")}-${part("month")}-${part("day")}T00:00:00Z`);
    const raw = c.req.query("date");
    const requested = raw === undefined ? today : parseDate(raw);
    if (!requested) return c.text("Invalid date; expected DD.MM.YYYY", 400);
    const date = fixingDate(requested > today ? today : requested);
    const start = Date.UTC(date.getUTCFullYear(), 0, 1);
    let index = 0;
    for (let day = start; day <= date.getTime(); day += DAY)
      if (isBusinessDay(new Date(day))) index++;
    const dayOfYear = Math.floor((date.getTime() - start) / DAY) + 1;
    // September 25, 2026 (day 268) gives the documented sample base rates.
    const offset = ((dayOfYear - 268) % 17) / 1000;
    const headerDate = `${String(date.getUTCDate()).padStart(2, "0")}.${String(date.getUTCMonth() + 1).padStart(2, "0")}.${date.getUTCFullYear()}`;
    return c.text(
      `${headerDate} #${index}\nzemě|měna|množství|kód|kurz\n${rates.map(([country, currency, amount, code, rate]) => `${country}|${currency}|${amount}|${code}|${(rate + offset).toFixed(3).replace(".", ",")}`).join("\n")}\n`,
    );
  });
}
