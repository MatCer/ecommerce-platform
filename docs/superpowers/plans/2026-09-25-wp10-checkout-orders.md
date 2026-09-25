# WP10 Checkout + order placement: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** a one-page checkout on the checkout origin that turns a handed-off cart into an
order exactly once (A12), reserves stock (A13), redeems the coupon, persists the pricing
allocations (A15), creates a payment attempt and pays it through the fake gateway (A10),
shows the order on `/o/<token>` (A4), emails a confirmation, expires unpaid orders, and links
guest orders to verified accounts (A5). Shipping and payment methods are configured per
market in the admin.

**Architecture:** business logic in `commerce::{shipping, payments, orders, checkout}`
(new) on top of `cart`, `pricing::price_cart`, `promotions::coupons`, `inventory`,
`orders::status` (WP9 machines) and `notifications`. HTTP in `api::storefront::checkout`
(cart capability, via the edge), `api::storefront::orders` (order capability), `api::admin_orders`
(staff JWT) and `api::webhooks` (fake provider events). The edge maps checkout-origin
`/_p/checkout/*`, `/_p/orders/*`, `/_p/fake-pay/*` and extends the CHECKOUT binding. UI in
`apps/checkout` (Astro SSR + one Solid island) and `apps/admin` (three screens). The Packeta
pickup-point widget is loaded on interaction from `PACKETA_WIDGET_URL`; `apps/mocks` serves a
compatible local widget.

## Global constraints

- Every new tenant table: `tenant_id`, RLS + `FORCE`, composite FKs, a cross-tenant test.
- Order capability tokens: 256-bit, SHA-256 at rest (`order_tokens`, 90 days). The token is
  never stored in plaintext (idempotent replays mint a fresh token for the same order; the
  confirmation email is `sensitive`, so its body is dropped once delivered).
- `place-order` is one transaction: idempotency key → cart row lock + version check →
  validation (A3 ship-to allowlist, method availability, legal checkboxes) → re-price with
  shipping/payment fee/cash rounding → order + lines + charges + addresses → stock
  reservations (variant order, so concurrent placements cannot deadlock) → coupon
  redemption → payment attempt → consent records → outbox `order.created` + confirmation
  email → cart `converted`. Provider init runs after the commit (A10) and is idempotent.
- Status changes only through `orders::status` transitions; each writes `order_events`.
- The client never sends prices; it sends the cart `version` and the total it saw, and gets
  `409 cart_changed` / `409 price_changed` when they no longer hold.
- `PAYMENTS_FAKE=1` is refused when `APP_ENV=prod`.
- No `unwrap()` outside tests, sqlx macros with `.sqlx/` data, TS strict without `any`.

## Review focus

- A12 races: double submit (same key), two tabs (two keys), last unit, coupon limit.
- A10: late success after expiry → `paid` + order `exception`, no restock; retries create
  new attempts on the same order within the order's payment window.
- A13: reservation on placement, release on expiry; COD `confirmed` on placement (A16).
- A15: persisted per-line discount/VAT and per-charge VAT portions equal the priced cart.
- A3: checkout refuses ship-to countries outside the market allowlist / tax profile.
- A20: optional consents unchecked by default, recorded server-side only when checked.
- Capability handling: cart capability for checkout mutations, order token for the order
  page and payment retries, session for the account list; no credential reaches the worker.

## Tasks

1. **Migration** `20260930000000_checkout_orders.sql`: `shipping_methods`,
   `payment_methods`, checkout columns on `carts` (phone, addresses, shipping method, pickup
   point, payment method), `order_numbers`, `orders` (unique `cart_id`), `order_tokens`,
   `order_lines`, `order_charges`, `order_addresses`, `order_events`, `payment_attempts`;
   `platform.due_payment_expiries()` (SECURITY DEFINER, cross-tenant scan for the job).
2. **`commerce::shipping`**: CRUD + validation (localized names, flat price, free-over,
   weight tiers, COD allowed + fee), `quote(method, goods_after_coupon, weight)`,
   `lowest_free_threshold(market)`; `/shop.free_shipping_threshold` and
   `cart.free_shipping_remaining` filled from it.
3. **`commerce::payments`**: per-market method config (`stripe|bank_transfer|cod|fake`,
   enabled, name, timeout), the `Gateway` trait (`available`, `init`) with `Fake`, `Cod`
   and placeholder `BankTransfer`/`Stripe` (unavailable until WP11); attempts (`create`,
   `init`, `apply_outcome` via the WP9 payment machine incl. late payments); fake provider
   events signed with HMAC-SHA256 and verified before processing.
4. **`commerce::checkout`**: checkout view (cart + methods with live rates + totals +
   selections), `set_contact`, `set_addresses` (A3), `set_shipping` (pickup snapshot),
   `set_payment`, `place_order` (A12 sequence above), order view by token, customer order
   list, guest linking, `expire_due` (A10 timeouts).
5. **Emails**: `order_confirmation` template (lines, totals, VAT recap, shipping/pickup,
   bank-transfer placeholder) cs/sk/en, enqueued in the placement transaction.
6. **API**: storefront `GET /checkout`, `PUT /checkout/{contact,addresses,shipping,payment}`,
   `POST /checkout/place-order` (Idempotency-Key required), `GET /orders/{token}`,
   `GET /orders/{token}/payment`, `POST /orders/{token}/payment-attempts`,
   `POST /orders/{token}/payment-attempts/{id}/init`, `GET|POST /checkout/fake-pay/{attempt}`,
   `GET /customer/orders`, `GET /customer/orders/{id}`; `POST /webhooks/fake`; admin
   shipping methods CRUD, payment methods per market, orders list + detail. OpenAPI + clients.
7. **Worker**: `orders.link_guest` on `customer.email_verified`, `payments.expire` every
   minute (cron).
8. **Integration tests** (commerce, real Postgres): placement happy path + allocations,
   idempotent replay, the four races, ship-to refusal, pickup required, COD confirmed,
   expiry releases stock + coupon, late success flags the exception, guest linking, RLS.
9. **Edge**: checkout-origin proxies (`/_p/checkout/*` incl. Idempotency-Key, `/_p/orders/*`,
   `/_p/fake-pay/*` page + form post), account orders ops, CHECKOUT binding ops, CSP with the
   widget origin on checkout only. Vitest.
10. **Mocks + compose**: Packeta widget mock (`library.js` compatible with
    `Packeta.Widget.pick`, accessible point list page), `mocks.localhost` in Caddy,
    `PACKETA_WIDGET_URL`, `PACKETA_API_KEY`, `PAYMENTS_FAKE` wiring.
11. **Checkout UI**: one-page checkout island (contact, addresses with prefill, shipping
    radios with rates + pickup point, payment radios, summary with legal checkboxes, place
    order → redirect), `/o/<token>` confirmation with status polling + retry, account orders.
12. **Admin UI**: shipping methods, payment methods, orders list + read-only detail.
13. **Seed**: CZ/SK shipping methods, payment methods (fake, COD).
14. **E2E** `e2e/checkout/orders.spec.ts`: guest CZ Packeta + fake success + Mailpit; SK home
    delivery; failed payment → retry → success; coupon + sale totals match the cart;
    logged-in customer sees the order; guest order linked after magic-link verification.
15. **Verification**: `make lint test`, e2e, `make perf`, `scripts/smoke-images.sh`; Astra
    review; PR.
