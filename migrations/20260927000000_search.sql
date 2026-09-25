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
    -- The last completed rebuild: when it started reading the catalog, and when it finished.
    rebuild_started_at timestamptz,
    rebuilt_at       timestamptz,
    documents        bigint,
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, locale)
);

-- Per-product indexing state. The row lock serializes indexing of one product; `read_at` is
-- when the last successful run read the catalog (database clock), so jobs dispatched before
-- it are stale and dropped (A27). No FK to products: a deleted product still needs its
-- documents removed.
CREATE TABLE search_product_state (
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    product_id uuid NOT NULL,
    read_at    timestamptz,
    indexed_at timestamptz,
    PRIMARY KEY (tenant_id, product_id)
);

-- Queries without results, for the analytics dashboard (WP14). Normalized query text only,
-- aggregated per day: no customer, session or IP (A20). Queries that look like contact data
-- (e-mail, phone numbers) are never stored; rows expire after 90 days.
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

-- Retention across tenants for the hourly maintenance job (same pattern as
-- platform.purge_idempotency_keys): the owner may read and delete only expired rows.
CREATE POLICY retention_read ON search_zero_results FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON search_zero_results FOR DELETE TO app_owner
    USING (day < current_date - 90);

CREATE FUNCTION platform.purge_search_zero_results() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH gone AS (
        DELETE FROM public.search_zero_results WHERE day < current_date - 90 RETURNING 1
    )
    SELECT count(*) FROM gone
$$;
REVOKE ALL ON FUNCTION platform.purge_search_zero_results() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.purge_search_zero_results() TO app_runtime;
