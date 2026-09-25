-- WP7: search bookkeeping (spec §11.1, A23, A27). The documents themselves live in
-- Meilisearch; these tables track index lifecycle, per-product indexing versions and the
-- zero-result query log.

-- One row per tenant + locale index (`t_<tenant uuid>_<locale>`).
CREATE TABLE search_indexes (
    tenant_id        uuid NOT NULL REFERENCES platform.tenants (id),
    locale           text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    -- Settings version applied to the live index (commerce::search::SETTINGS_VERSION).
    settings_version integer NOT NULL,
    -- Index being filled by a running rebuild; incremental jobs write to it as well.
    building_uid     text,
    rebuilt_at       timestamptz,
    documents        bigint,
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, locale)
);

-- Per-product indexing version: the id of the last job that indexed the product. The row
-- lock serializes indexing of one product; older jobs are dropped as stale (A27).
-- No FK to products: a deleted product still needs its documents removed.
CREATE TABLE search_product_state (
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    product_id      uuid NOT NULL,
    indexed_version bigint NOT NULL DEFAULT 0,
    indexed_at      timestamptz,
    PRIMARY KEY (tenant_id, product_id)
);

-- Queries without results, for the analytics dashboard (WP14). Normalized query text only,
-- aggregated per day: no customer, session or IP (A20).
CREATE TABLE search_zero_results (
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    day       date NOT NULL,
    locale    text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    query     text NOT NULL CHECK (length(query) BETWEEN 1 AND 200),
    count     integer NOT NULL DEFAULT 1 CHECK (count > 0),
    PRIMARY KEY (tenant_id, day, locale, query)
);

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['search_indexes', 'search_product_state', 'search_zero_results']
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
