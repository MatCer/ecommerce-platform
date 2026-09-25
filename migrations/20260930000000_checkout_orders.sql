-- WP10: shipping and payment methods, checkout state, orders, payment attempts (spec §7.3,
-- §7.4, §10.3-10.5, A4, A10, A12, A13, A15, A16).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Order
-- capability tokens are stored only as SHA-256 hashes.

-- ---------------------------------------------------------------------------------------
-- Shipping methods (§7.4, §10.5): per market; flat price, optional free-over threshold,
-- optional weight tiers (`[{"up_to_g": 2000, "price_minor": 7900}, ...]`, ascending; heavier
-- carts cannot use the method), cash on delivery with its fee.

CREATE TABLE shipping_methods (
    id              uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    market_id       uuid NOT NULL,
    carrier         text NOT NULL CHECK (carrier IN ('packeta_pickup', 'packeta_home', 'ppl', 'personal_pickup')),
    name_i18n       jsonb NOT NULL CHECK (jsonb_typeof(name_i18n) = 'object'),
    description_i18n jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(description_i18n) = 'object'),
    price_minor     bigint NOT NULL CHECK (price_minor BETWEEN 0 AND 100000000),
    free_over_minor bigint CHECK (free_over_minor BETWEEN 1 AND 100000000000),
    weight_tiers    jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(weight_tiers) = 'array'),
    cod_allowed     boolean NOT NULL DEFAULT false,
    cod_fee_minor   bigint NOT NULL DEFAULT 0 CHECK (cod_fee_minor BETWEEN 0 AND 100000000),
    active          boolean NOT NULL DEFAULT true,
    position        integer NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX shipping_methods_market ON shipping_methods (tenant_id, market_id, position);

-- Payment methods per market (§10.4, A10). `timeout_minutes`: how long an unpaid order holds
-- its stock (NULL = no timeout, cash on delivery).
CREATE TABLE payment_methods (
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    market_id       uuid NOT NULL,
    kind            text NOT NULL CHECK (kind IN ('stripe', 'bank_transfer', 'cod', 'fake')),
    enabled         boolean NOT NULL DEFAULT false,
    name_i18n       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(name_i18n) = 'object'),
    timeout_minutes integer CHECK (timeout_minutes BETWEEN 5 AND 43200),
    position        integer NOT NULL DEFAULT 0,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, market_id, kind),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

-- ---------------------------------------------------------------------------------------
-- Checkout state lives on the cart until the order is placed. Addresses are validated JSON
-- snapshots (`commerce::checkout`); the pickup point is the widget's selection.

ALTER TABLE carts
    ADD COLUMN phone              text CHECK (length(phone) <= 40),
    ADD COLUMN billing_address    jsonb CHECK (jsonb_typeof(billing_address) = 'object'),
    ADD COLUMN shipping_address   jsonb CHECK (jsonb_typeof(shipping_address) = 'object'),
    ADD COLUMN shipping_method_id uuid,
    ADD COLUMN pickup_point       jsonb CHECK (jsonb_typeof(pickup_point) = 'object'),
    ADD COLUMN payment_method     text CHECK (payment_method IN ('stripe', 'bank_transfer', 'cod', 'fake')),
    ADD CONSTRAINT carts_shipping_method_fk FOREIGN KEY (tenant_id, shipping_method_id)
        REFERENCES shipping_methods (tenant_id, id) ON DELETE SET NULL (shipping_method_id);

-- ---------------------------------------------------------------------------------------
-- Orders (§7.3, A12, A13, A15). Numbers are per tenant, numeric and at most 10 digits (they
-- are the bank-transfer variable symbol, A25).

CREATE TABLE order_numbers (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants (id),
    last      bigint NOT NULL CHECK (last BETWEEN 0 AND 9999999999)
);

CREATE TABLE orders (
    id                 uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id          uuid NOT NULL REFERENCES platform.tenants (id),
    number             bigint NOT NULL CHECK (number BETWEEN 1 AND 9999999999),
    market_id          uuid NOT NULL,
    -- A12: one order per cart.
    cart_id            uuid NOT NULL,
    customer_id        uuid,
    email              text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    phone              text CHECK (length(phone) <= 40),
    locale             text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    currency           text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    status             text NOT NULL CHECK (status IN ('pending', 'confirmed', 'processing', 'shipped',
                                                       'delivered', 'cancelled', 'returned')),
    payment_status     text NOT NULL CHECK (payment_status IN ('unpaid', 'authorized', 'paid',
                                             'partially_refunded', 'refunded', 'failed', 'expired')),
    fulfillment_status text NOT NULL CHECK (fulfillment_status IN ('unfulfilled', 'label_created',
                                             'shipped', 'delivered', 'returned')),
    -- A10: money arrived for an expired or cancelled order; needs a refund, never a restock.
    exception          text CHECK (exception IN ('late_payment')),
    ship_to_country    text NOT NULL CHECK (ship_to_country ~ '^[A-Z]{2}$'),
    vat_payer          boolean NOT NULL,
    subtotal_minor     bigint NOT NULL,
    discount_minor     bigint NOT NULL,
    shipping_minor     bigint NOT NULL,
    payment_fee_minor  bigint NOT NULL,
    tax_minor          bigint NOT NULL,
    rounding_minor     bigint NOT NULL,
    total_minor        bigint NOT NULL CHECK (total_minor >= 0),
    vat_recap          jsonb NOT NULL,
    coupon_id          uuid,
    coupon_code        text,
    shipping_method_id uuid,
    shipping_method_snapshot jsonb NOT NULL,
    payment_method     text NOT NULL CHECK (payment_method IN ('stripe', 'bank_transfer', 'cod', 'fake')),
    pickup_point       jsonb,
    notes              text CHECK (length(notes) <= 1000),
    -- Unpaid prepaid orders are cancelled after this (A10); NULL for cash on delivery.
    payment_expires_at timestamptz,
    placed_at          timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT orders_number_unique UNIQUE (tenant_id, number),
    CONSTRAINT orders_cart_unique UNIQUE (tenant_id, cart_id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE SET NULL (customer_id),
    FOREIGN KEY (tenant_id, coupon_id) REFERENCES coupons (tenant_id, id) ON DELETE SET NULL (coupon_id),
    FOREIGN KEY (tenant_id, shipping_method_id) REFERENCES shipping_methods (tenant_id, id)
        ON DELETE SET NULL (shipping_method_id)
);
CREATE INDEX orders_placed ON orders (tenant_id, placed_at DESC, id DESC);
CREATE INDEX orders_customer ON orders (tenant_id, customer_id, placed_at DESC) WHERE customer_id IS NOT NULL;
CREATE INDEX orders_guest_email ON orders (tenant_id, email) WHERE customer_id IS NULL;
CREATE INDEX orders_payment_expiry ON orders (payment_expires_at)
    WHERE status = 'pending' AND payment_expires_at IS NOT NULL;

-- A4: read-only capability for `/o/<token>` (90 days). Several per order: an idempotent
-- replay of place-order mints a fresh one instead of storing the first in plaintext.
CREATE TABLE order_tokens (
    token_hash bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id  uuid NOT NULL,
    order_id   uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX order_tokens_order ON order_tokens (tenant_id, order_id);

-- A15: the allocations are persisted as priced; historical orders are never re-priced.
CREATE TABLE order_lines (
    id               uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id        uuid NOT NULL,
    order_id         uuid NOT NULL,
    position         integer NOT NULL,
    variant_id       uuid,
    product_id       uuid,
    sku              text NOT NULL,
    name             text NOT NULL,
    options_label    text NOT NULL DEFAULT '',
    quantity         integer NOT NULL CHECK (quantity > 0),
    unit_gross_minor bigint NOT NULL,
    base_minor       bigint NOT NULL,
    discount_minor   bigint NOT NULL,
    total_minor      bigint NOT NULL,
    tax_rate         text NOT NULL,
    tax_minor        bigint NOT NULL,
    net_minor        bigint NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, order_id, position),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE SET NULL (variant_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE SET NULL (product_id)
);

CREATE TABLE order_charges (
    tenant_id      uuid NOT NULL,
    order_id       uuid NOT NULL,
    kind           text NOT NULL CHECK (kind IN ('shipping', 'payment_fee', 'rounding')),
    base_minor     bigint NOT NULL,
    discount_minor bigint NOT NULL,
    total_minor    bigint NOT NULL,
    tax_minor      bigint NOT NULL,
    net_minor      bigint NOT NULL,
    -- The gross split across VAT rates: [{"tax_rate", "gross_minor", "vat_minor", "net_minor"}].
    portions       jsonb NOT NULL,
    PRIMARY KEY (tenant_id, order_id, kind),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE order_addresses (
    tenant_id   uuid NOT NULL,
    order_id    uuid NOT NULL,
    kind        text NOT NULL CHECK (kind IN ('billing', 'shipping')),
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    company     text CHECK (length(company) <= 200),
    street      text NOT NULL CHECK (length(street) BETWEEN 1 AND 200),
    city        text NOT NULL CHECK (length(city) BETWEEN 1 AND 100),
    postal_code text NOT NULL CHECK (length(postal_code) BETWEEN 1 AND 20),
    country     text NOT NULL CHECK (country ~ '^[A-Z]{2}$'),
    phone       text CHECK (length(phone) <= 40),
    PRIMARY KEY (tenant_id, order_id, kind),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);

-- The timeline (§10.6): state transitions, payments, consents given at checkout.
CREATE TABLE order_events (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL,
    order_id   uuid NOT NULL,
    kind       text NOT NULL CHECK (kind ~ '^[a-z_]{1,64}$'),
    data       jsonb NOT NULL DEFAULT '{}',
    actor      text NOT NULL,
    at         timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX order_events_order ON order_events (tenant_id, order_id, at, id);

-- A10: one row per try to pay. Provider init happens after the placement commit with the
-- attempt id as the provider's idempotency key. At most one open attempt per order.
CREATE TABLE payment_attempts (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL,
    order_id     uuid NOT NULL,
    method       text NOT NULL CHECK (method IN ('stripe', 'bank_transfer', 'cod', 'fake')),
    status       text NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'succeeded', 'failed', 'expired')),
    amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
    currency     text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    provider_ref text CHECK (length(provider_ref) <= 255),
    expires_at   timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX payment_attempts_order ON payment_attempts (tenant_id, order_id, created_at);
CREATE UNIQUE INDEX payment_attempts_one_open ON payment_attempts (tenant_id, order_id)
    WHERE status = 'pending';

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['shipping_methods', 'payment_methods', 'order_numbers', 'orders',
        'order_tokens', 'order_lines', 'order_charges', 'order_addresses', 'order_events',
        'payment_attempts']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I TO app_runtime
                 USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                 WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
    END LOOP;
END
$$;

GRANT SELECT, INSERT, UPDATE, DELETE ON shipping_methods, payment_methods, order_tokens
    TO app_runtime;
GRANT SELECT, INSERT, UPDATE ON order_numbers, orders, order_lines, order_charges,
    order_addresses, payment_attempts TO app_runtime;
-- The timeline is append-only.
GRANT SELECT, INSERT ON order_events TO app_runtime;

-- Payment timeouts (A10) scan every tenant: only the ids of pending orders whose payment
-- window closed leave this function; the expiry itself runs per tenant in `tenant_tx`.
CREATE POLICY expiry_scan ON orders FOR SELECT TO app_owner
    USING (status = 'pending' AND payment_expires_at IS NOT NULL);

CREATE FUNCTION platform.due_payment_expiries(max_rows integer)
RETURNS TABLE (tenant_id uuid, order_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT o.tenant_id, o.id FROM public.orders o
    WHERE o.status = 'pending' AND o.payment_expires_at IS NOT NULL
      AND o.payment_expires_at < now()
    ORDER BY o.payment_expires_at
    LIMIT least(greatest(max_rows, 1), 1000)
$$;
REVOKE ALL ON FUNCTION platform.due_payment_expiries(integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.due_payment_expiries(integer) TO app_runtime;
