# WP4 Money, tax, pricing, promotions, inventory: implementation plan

> **For agentic workers:** execute task by task with TDD; commit after every task.

**Goal:** the pure commerce core (money, VAT liability, effective-price intervals, Omnibus
reference, the ordered cart pricing algorithm, cash rounding) plus the tables, services and
Admin API for tax profile, price lists, variant prices, sales, coupons and stock.

**Spec:** §7.2, §10.1, §10.2, D25 and amendments A3 (VAT liability), A12 (idempotency), A13
(stock movements), A15 (pricing algorithm), A16 (cash rounding), A18 (Omnibus).

## Global Constraints

- Integer minor units (`i64`) everywhere; intermediate products in `i128`; checked arithmetic,
  overflow is an error, never a wrap. No floats.
- Tax rates are integer hundredths of a percent (`TaxRate(2100)` = 21 %). `numeric(5,2)` maps
  onto it exactly, so no decimal crate is needed (deviation from D25's `rust_decimal`).
- Every new `public` table: `tenant_id`, `tenant_isolation` policy, ENABLE + FORCE RLS,
  composite `(tenant_id, …)` FKs; cross-tenant test per table family.
- Business logic in `crates/commerce` (`money`, `tax`, `pricing`, `promotions`, `inventory`);
  `api` parses, authorizes and wraps calls in `tenant_tx` (+ idempotency on creates).
- Every mutation writes `audit_log`; `price.changed` / `inventory.changed` carry before/after.
- Pure functions get unit + `proptest` property tests; DB services get `#[sqlx::test]` tests as
  the runtime role.

## Design decisions

- **Price intervals are the source of truth for the effective price.** `price_intervals`
  holds the full timeline per (price list, variant), non-overlapping (GiST exclusion). Any
  change (base price upsert/delete, sale create/update/delete, product category change)
  recomputes the timeline from `now` for the affected pairs: a pure `timeline()` builds the
  desired segments from the base price and live sales, a pure `diff()` turns them into
  close/delete/insert ops against the live intervals. Past intervals are never rewritten.
  Pricing writes take a per-tenant advisory lock.
- **Sales:** best (lowest) resulting price wins when sales overlap; fixed sales apply only to
  price lists in their currency; targets = all | products | categories (incl. descendants).
- **Scheduled transitions:** each future segment start enqueues `pricing.transition` (run at
  that time, idempotent per tenant+instant); the worker publishes `price.changed` for the
  intervals starting then. Immediate changes publish in the writing transaction.
- **Omnibus (A18):** a reduction is announced only while the current interval is a sale. The
  chain start is the first of the contiguous sale intervals; reference = min over intervals
  overlapping `[start − 30 d, start)` plus published coupons' per-unit effect in that window;
  young products use what exists; imported history < 30 days → no claim.
- **`price_cart`:** pure, A15 order: effective unit price × qty → coupon allocated by largest
  remainder → per-line VAT `round_half_up(g·r/(100+r))` → shipping/payment fee split across the
  goods' rates by largest remainder → cash rounding line last. Refund reversal is cumulative
  (`f(k) = round(component·k/qty)`), so partial refunds always sum to the original.
- **Cash rounding (A16):** CZK → 1 Kč, EUR in SK → €0.05, half up, only for tender = cash;
  outside the VAT base unless the tax profile says otherwise (accountant to confirm).
- **VAT liability (A3):** non-payer → no VAT; ship-to = establishment or `origin_threshold`
  → establishment rates; `destination` → destination rates; non-EU → blocked (M1).
- **Inventory (A13):** movement row first (`ON CONFLICT DO NOTHING` = idempotent replay), then a
  guarded `UPDATE … WHERE` on the level row (row lock serializes racing reservations) plus a
  CHECK constraint as backstop against overselling.
- **Coupons:** codes uppercase; limits enforced under the coupon row lock
  (`UPDATE … WHERE used_count < usage_limit`); redemption unique per order.

## Review Focus

- Arithmetic: overflow, rounding direction, allocation determinism, negative totals.
- Interval diff correctness at boundaries (now == valid_from, open-ended, deleted prices).
- Omnibus chain/young/imported rules. Concurrency of reservations and coupon limits.
- RLS + composite FKs on every table; fresh-auth on tax profile updates.

## Tasks

1. `money` (Currency, Money, formatting cs/sk/en, `allocate`, `div_round_half_up`) + proptests.
2. `tax` (TaxRate, TaxProfile validation, liability resolution, ship-to check, VAT extraction).
3. `pricing::cart` (`price_cart`, cash rounding, `reverse_line`) + proptests.
4. `pricing::intervals` (`timeline`, `diff`) and `pricing::omnibus` (pure) + tests.
5. Migration `20260926000000_pricing.sql`: tax_profiles, price_lists (+ markets FK),
   variant_prices, price_intervals, sales, coupons, coupon_redemptions, inventory_levels,
   stock_movements; RLS.
6. DB services: tax profile, price lists, variant prices + refresh, sales, coupons (+ redeem),
   inventory (+ race test), price history; worker `pricing.transition` handler.
7. Admin API (`admin_pricing.rs`, `admin_promotions.rs`, `admin_inventory.rs`) + API tests.
8. `make openapi`, sqlx prepare, smoke on the stack, docs.
