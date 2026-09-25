-- WP5: indexed substring search for the admin product list (`q`: name in any locale or SKU).
--
-- pg_trgm is a trusted extension: app_owner owns the database and can install it.
-- `ILIKE` is not leakproof, so under the `tenant_isolation` RLS policy Postgres must check the
-- policy before evaluating it and cannot use a trigram index for app_runtime queries. The
-- search therefore runs in a SECURITY DEFINER function owned by app_owner: the narrow
-- `search_read` policies below give app_owner plain reads, and the function applies the tenant
-- filter itself from the same transaction-local `app.tenant_id` that RLS uses (so it fails
-- closed without tenant context, like every tenant query).
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX product_translations_name_trgm ON product_translations USING gin (name gin_trgm_ops);
CREATE INDEX variants_sku_trgm ON variants USING gin (sku gin_trgm_ops);

CREATE POLICY search_read ON product_translations FOR SELECT TO app_owner USING (true);
CREATE POLICY search_read ON variants FOR SELECT TO app_owner USING (true);

-- Ids of the current tenant's products whose name (any locale) or SKU matches the ILIKE
-- `p_pattern` (the caller escapes wildcards).
CREATE FUNCTION platform.search_product_ids(p_pattern text) RETURNS SETOF uuid
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT t.product_id FROM public.product_translations t
    WHERE t.name ILIKE p_pattern AND t.tenant_id = current_setting('app.tenant_id')::uuid
    UNION
    SELECT v.product_id FROM public.variants v
    WHERE v.sku ILIKE p_pattern AND v.tenant_id = current_setting('app.tenant_id')::uuid
$$;

REVOKE ALL ON FUNCTION platform.search_product_ids(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.search_product_ids(text) TO app_runtime;
