-- WP4: tax profile, price lists, variant prices, effective-price intervals, sales, coupons,
-- stock levels and movements (spec §7.2, §10.1, §10.2, A3, A13, A15, A16, A18).
--
-- Tenant tables follow the WP1/WP3 rules: tenant_id, RLS + FORCE with `tenant_isolation`,
-- composite (tenant_id, ...) foreign keys. Amounts are integer minor units.

-- Needed for the non-overlap exclusion constraint on price_intervals (uuid equality in GiST).
-- A trusted extension: the database owner (app_owner) may create it.
CREATE EXTENSION IF NOT EXISTS btree_gist;

-- ---------------------------------------------------------------------------------------
-- Tax profile (A3). One per tenant; no row = no VAT setup, and tax resolution fails closed.

CREATE TABLE tax_profiles (
    tenant_id                     uuid PRIMARY KEY REFERENCES platform.tenants (id),
    establishment_country         text NOT NULL CHECK (establishment_country ~ '^[A-Z]{2}$'),
    vat_payer                     boolean NOT NULL,
    -- DIČ (CZ: CZ + 8-10 digits; SK: 10 digits). Validated by commerce::tax.
    vat_id                        text CHECK (length(vat_id) BETWEEN 4 AND 20),
    -- SK IČ DPH (SK + 10 digits), separate from the DIČ (A3, A17).
    sk_ic_dph                     text CHECK (sk_ic_dph ~ '^SK[0-9]{10}$'),
    distance_sales_mode           text NOT NULL
                                  CHECK (distance_sales_mode IN ('origin_threshold', 'destination')),
    -- When the merchant confirmed eligibility for the EU EUR 10 000 threshold exception.
    origin_threshold_confirmed_at timestamptz,
    -- A16: whether the cash rounding line is part of the VAT base. Default: outside it.
    cash_rounding_in_vat_base     boolean NOT NULL DEFAULT false,
    created_at                    timestamptz NOT NULL DEFAULT now(),
    updated_at                    timestamptz NOT NULL DEFAULT now(),
    CHECK (distance_sales_mode <> 'origin_threshold' OR origin_threshold_confirmed_at IS NOT NULL)
);

-- ---------------------------------------------------------------------------------------
-- Price lists and variant prices (§7.2). Prices are gross (VAT-inclusive, B2C).

CREATE TABLE price_lists (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    code       text NOT NULL CHECK (code ~ '^[a-z0-9][a-z0-9-]{0,31}$'),
    name       text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    currency   text NOT NULL CHECK (currency IN ('BGN', 'CZK', 'DKK', 'EUR', 'HUF', 'PLN', 'RON', 'SEK')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, id, currency),
    UNIQUE (tenant_id, code)
);

-- A market sells from one price list in its own currency (the FK includes the currency).
ALTER TABLE markets
    ADD CONSTRAINT markets_price_list_fk FOREIGN KEY (tenant_id, price_list_id, currency)
        REFERENCES price_lists (tenant_id, id, currency);

CREATE TABLE variant_prices (
    tenant_id        uuid NOT NULL,
    price_list_id    uuid NOT NULL,
    variant_id       uuid NOT NULL,
    amount_minor     bigint NOT NULL CHECK (amount_minor BETWEEN 0 AND 1000000000000),
    -- A recommended/"was" price for display only; never a reduction basis (A18).
    compare_at_minor bigint CHECK (compare_at_minor > amount_minor AND compare_at_minor <= 1000000000000),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, price_list_id, variant_id),
    FOREIGN KEY (tenant_id, price_list_id) REFERENCES price_lists (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX variant_prices_variant ON variant_prices (tenant_id, variant_id);

-- ---------------------------------------------------------------------------------------
-- Sales (automatic discounts, §10.2). `value`: basis points for percent (1500 = 15 %),
-- minor units of `currency` for fixed. Targets: {"all": bool, "product_ids": [], "category_ids": []}.

CREATE TABLE sales (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    name       text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    kind       text NOT NULL CHECK (kind IN ('percent', 'fixed')),
    value      bigint NOT NULL CHECK (value > 0),
    currency   text CHECK (currency IN ('BGN', 'CZK', 'DKK', 'EUR', 'HUF', 'PLN', 'RON', 'SEK')),
    starts_at  timestamptz NOT NULL,
    ends_at    timestamptz,
    targets    jsonb NOT NULL CHECK (jsonb_typeof(targets) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK (ends_at > starts_at),
    CHECK ((kind = 'percent' AND value <= 10000 AND currency IS NULL)
        OR (kind = 'fixed' AND currency IS NOT NULL))
);
CREATE INDEX sales_live ON sales (tenant_id, ends_at);

-- ---------------------------------------------------------------------------------------
-- Effective-price intervals (A18). The materialized timeline per (price list, variant),
-- including future scheduled sale starts/ends. Rewritten only from "now" onwards by
-- commerce::pricing::intervals; the past is history and never changes.

CREATE TABLE price_intervals (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL,
    price_list_id uuid NOT NULL,
    variant_id    uuid NOT NULL,
    amount_minor  bigint NOT NULL CHECK (amount_minor >= 0),
    valid_from    timestamptz NOT NULL,
    valid_to      timestamptz CHECK (valid_to > valid_from),
    cause         text NOT NULL CHECK (cause IN ('base', 'sale', 'tax')),
    sale_id       uuid,
    -- The price arrived by import: its history before valid_from is unknown (A18).
    imported      boolean NOT NULL DEFAULT false,
    PRIMARY KEY (id),
    CHECK (sale_id IS NULL OR cause = 'sale'),
    FOREIGN KEY (tenant_id, price_list_id) REFERENCES price_lists (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE CASCADE,
    -- A deleted sale's past intervals stay (cause 'sale'), only the link goes.
    FOREIGN KEY (tenant_id, sale_id) REFERENCES sales (tenant_id, id) ON DELETE SET NULL (sale_id),
    EXCLUDE USING gist (
        tenant_id WITH =, price_list_id WITH =, variant_id WITH =,
        tstzrange(valid_from, valid_to) WITH &&
    )
);
CREATE INDEX price_intervals_pair ON price_intervals (tenant_id, price_list_id, variant_id, valid_from);
CREATE INDEX price_intervals_live ON price_intervals (tenant_id, valid_to);
CREATE INDEX price_intervals_start ON price_intervals (tenant_id, valid_from);

-- ---------------------------------------------------------------------------------------
-- Coupons (§7.2, §10.2). Codes are stored uppercase. `value`: basis points (percent), minor
-- units of `currency` (fixed), NULL (free_shipping). `published` = available to everyone,
-- which makes it a price reduction for the Omnibus reference (A18).

CREATE TABLE coupons (
    id                 uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id          uuid NOT NULL REFERENCES platform.tenants (id),
    code               text NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{2,31}$'),
    kind               text NOT NULL CHECK (kind IN ('percent', 'fixed', 'free_shipping')),
    value              bigint,
    currency           text CHECK (currency IN ('BGN', 'CZK', 'DKK', 'EUR', 'HUF', 'PLN', 'RON', 'SEK')),
    min_subtotal_minor bigint CHECK (min_subtotal_minor > 0),
    starts_at          timestamptz,
    ends_at            timestamptz,
    usage_limit        integer CHECK (usage_limit > 0),
    per_customer_limit integer CHECK (per_customer_limit > 0),
    used_count         integer NOT NULL DEFAULT 0 CHECK (used_count >= 0),
    published          boolean NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT coupons_code_unique UNIQUE (tenant_id, code),
    CHECK (ends_at > starts_at),
    CONSTRAINT coupons_usage_within_limit CHECK (usage_limit IS NULL OR used_count <= usage_limit),
    CHECK ((kind = 'percent' AND value BETWEEN 1 AND 10000)
        OR (kind = 'fixed' AND value > 0 AND currency IS NOT NULL)
        OR (kind = 'free_shipping' AND value IS NULL)),
    CHECK (min_subtotal_minor IS NULL OR currency IS NOT NULL)
);

-- One redemption per coupon and order. `customer_key`: the customer id, or the normalized
-- e-mail for guests (customers arrive in WP9).
CREATE TABLE coupon_redemptions (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL,
    coupon_id    uuid NOT NULL,
    customer_key text NOT NULL CHECK (length(customer_key) BETWEEN 1 AND 320),
    order_ref    text NOT NULL CHECK (length(order_ref) BETWEEN 1 AND 255),
    redeemed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, coupon_id, order_ref),
    FOREIGN KEY (tenant_id, coupon_id) REFERENCES coupons (tenant_id, id)
);
CREATE INDEX coupon_redemptions_customer ON coupon_redemptions (tenant_id, coupon_id, customer_key);

-- ---------------------------------------------------------------------------------------
-- Inventory (§7.2, A13). Levels change only together with a movement, in one transaction.

CREATE TABLE inventory_levels (
    tenant_id       uuid NOT NULL,
    variant_id      uuid NOT NULL,
    on_hand         integer NOT NULL DEFAULT 0,
    reserved        integer NOT NULL DEFAULT 0 CHECK (reserved >= 0),
    track           boolean NOT NULL DEFAULT true,
    allow_backorder boolean NOT NULL DEFAULT false,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, variant_id),
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE CASCADE,
    -- Backstop against overselling: tracked stock without backorders never goes below zero
    -- and never has more reserved than on hand.
    CONSTRAINT inventory_levels_no_oversell
        CHECK (NOT track OR allow_backorder OR (on_hand >= 0 AND reserved <= on_hand))
);

-- `quantity`: positive units for reserve/release/commit/restock, a signed delta for adjust.
-- The unique identity makes every movement idempotent (A13).
CREATE TABLE stock_movements (
    id             uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id      uuid NOT NULL,
    variant_id     uuid NOT NULL,
    kind           text NOT NULL CHECK (kind IN ('reserve', 'release', 'commit', 'restock', 'adjust')),
    quantity       integer NOT NULL CHECK (quantity <> 0 AND (kind = 'adjust' OR quantity > 0)),
    ref_type       text NOT NULL CHECK (ref_type ~ '^[a-z][a-z_]{0,31}$'),
    ref_id         text NOT NULL CHECK (length(ref_id) BETWEEN 1 AND 255),
    on_hand_after  integer NOT NULL,
    reserved_after integer NOT NULL,
    actor          text NOT NULL,
    note           text CHECK (length(note) <= 500),
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    CONSTRAINT stock_movements_identity UNIQUE (tenant_id, kind, ref_type, ref_id, variant_id),
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX stock_movements_variant ON stock_movements (tenant_id, variant_id, id DESC);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['tax_profiles', 'price_lists', 'variant_prices', 'sales',
        'price_intervals', 'coupons', 'coupon_redemptions', 'inventory_levels', 'stock_movements']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I TO app_runtime
                 USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                 WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_runtime', t);
    END LOOP;
END
$$;

-- Movements are an append-only ledger for the application.
REVOKE UPDATE, DELETE ON stock_movements FROM app_runtime;
