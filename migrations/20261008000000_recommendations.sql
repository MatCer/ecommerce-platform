-- WP17: recommendations (spec §7.6, §11.2, A20). Days are UTC. Everything is per tenant:
-- no statistic, pair or affinity is ever computed across tenants.

-- Per product, market and day: consented browser events (views, adds) and placed,
-- non-cancelled orders (authoritative purchases). `purchases` counts units; `revenue_minor`
-- is the sum of line totals in the market's currency.
CREATE TABLE product_stats_daily (
    tenant_id     uuid NOT NULL REFERENCES platform.tenants (id),
    date          date NOT NULL,
    market_id     uuid NOT NULL,
    product_id    uuid NOT NULL,
    views         integer NOT NULL DEFAULT 0 CHECK (views >= 0),
    add_to_carts  integer NOT NULL DEFAULT 0 CHECK (add_to_carts >= 0),
    purchases     integer NOT NULL DEFAULT 0 CHECK (purchases >= 0),
    revenue_minor bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, market_id, date, product_id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);

-- Products bought together in the last 90 days: distinct orders containing both. Symmetric
-- (both directions are stored) and only pairs with a support of at least 3.
CREATE TABLE co_purchases (
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    product_a uuid NOT NULL,
    product_b uuid NOT NULL,
    count_90d integer NOT NULL CHECK (count_90d >= 3),
    PRIMARY KEY (tenant_id, product_a, product_b),
    CHECK (product_a <> product_b),
    FOREIGN KEY (tenant_id, product_a) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, product_b) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);

-- Time-decayed scores per market over the last 90 days (half-life 14 days): `sales_score`
-- ranks bestsellers (units), `popularity` blends purchases, adds and views.
CREATE TABLE product_scores (
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    market_id   uuid NOT NULL,
    product_id  uuid NOT NULL,
    sales_score double precision NOT NULL CHECK (sales_score >= 0),
    popularity  double precision NOT NULL CHECK (popularity >= 0),
    PRIMARY KEY (tenant_id, market_id, product_id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX product_scores_sales ON product_scores (tenant_id, market_id, sales_score DESC);

-- The popularity currently in the search documents (all markets together). Rewritten only
-- when it moves noticeably, so the hourly rollup does not reindex the whole catalog.
-- `reindex`: changed, reindex job not enqueued yet. Set in the rollup's transaction and cleared
-- in the one that enqueues the jobs, so a crash in between only delays the reindex.
CREATE TABLE product_popularity (
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    product_id uuid NOT NULL,
    popularity integer NOT NULL CHECK (popularity >= 0),
    reindex    boolean NOT NULL DEFAULT true,
    PRIMARY KEY (tenant_id, product_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX product_popularity_reindex ON product_popularity (tenant_id) WHERE reindex;

-- Merchant-curated product lists. `seasonal` ones have a schedule window and feed the home
-- page while it is open; `manual` ones are always available (optionally scheduled) and are
-- rendered where a theme asks for them (`context=collection:<id>`).
CREATE TABLE collections (
    id          uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    -- Shopper-facing heading per locale (`{"cs": "..."}`); the name when absent.
    title_i18n  jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(title_i18n) = 'object'),
    kind        text NOT NULL CHECK (kind IN ('manual', 'seasonal')),
    starts_at   timestamptz,
    ends_at     timestamptz,
    product_ids uuid[] NOT NULL DEFAULT '{}' CHECK (cardinality(product_ids) <= 200),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK (kind = 'manual' OR (starts_at IS NOT NULL AND ends_at IS NOT NULL)),
    CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at > starts_at)
);

-- Category/brand affinity of customers whose current `personalization` consent is granted
-- (A20), from their orders. Rebuilt by the rollup; withdrawn consent drops the rows.
CREATE TABLE customer_affinity (
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    customer_id uuid NOT NULL,
    dim         text NOT NULL CHECK (dim IN ('category', 'brand')),
    key         text NOT NULL CHECK (length(key) BETWEEN 1 AND 200),
    score       double precision NOT NULL CHECK (score > 0),
    PRIMARY KEY (tenant_id, customer_id, dim, key),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE CASCADE
);

-- Per-tenant switches; no row = everything enabled, nothing excluded.
CREATE TABLE recommendation_settings (
    tenant_id            uuid PRIMARY KEY REFERENCES platform.tenants (id),
    bestsellers          boolean NOT NULL DEFAULT true,
    bought_together      boolean NOT NULL DEFAULT true,
    seasonal             boolean NOT NULL DEFAULT true,
    recently_viewed      boolean NOT NULL DEFAULT true,
    personalized         boolean NOT NULL DEFAULT true,
    excluded_product_ids uuid[] NOT NULL DEFAULT '{}' CHECK (cardinality(excluded_product_ids) <= 500),
    updated_at           timestamptz NOT NULL DEFAULT now()
);

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['product_stats_daily', 'co_purchases', 'product_scores',
                             'product_popularity', 'collections', 'customer_affinity',
                             'recommendation_settings']
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
