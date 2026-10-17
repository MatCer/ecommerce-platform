SET LOCAL lock_timeout = '5s';

CREATE TABLE platform.checkout_fences (
    tenant_id  uuid PRIMARY KEY REFERENCES platform.tenants (id),
    generation bigint NOT NULL CHECK (generation >= 1)
);

CREATE FUNCTION platform.checkout_fence_key(p_tenant uuid) RETURNS bigint
LANGUAGE sql IMMUTABLE
SET search_path = ''
AS $$ SELECT pg_catalog.hashtextextended('checkout_fence:' || p_tenant::text, 0) $$;

CREATE FUNCTION platform.enter_checkout_fence(p_tenant uuid, p_origin text)
RETURNS TABLE (generation bigint, mode text, origin_tenant uuid, origin_market uuid)
LANGUAGE plpgsql VOLATILE
SET search_path = ''
AS $$
BEGIN
    PERFORM pg_advisory_xact_lock_shared(platform.checkout_fence_key(p_tenant));
    -- A second statement takes a fresh snapshot: a switch that committed while we waited is seen.
    RETURN QUERY
    SELECT coalesce((SELECT f.generation FROM platform.checkout_fences f
                     WHERE f.tenant_id = p_tenant), 0),
           coalesce(o.mode, 'isolated'), o.tenant_id, o.market_id
    FROM (SELECT 1) AS one
    LEFT JOIN platform.checkout_origins o ON o.origin = p_origin;
END
$$;

CREATE FUNCTION platform.checkout_policy_changed() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = ''
AS $$
DECLARE
    t uuid := coalesce(NEW.tenant_id, OLD.tenant_id);
BEGIN
    PERFORM pg_advisory_xact_lock(platform.checkout_fence_key(t));
    INSERT INTO platform.checkout_fences AS f (tenant_id, generation) VALUES (t, 1)
    ON CONFLICT (tenant_id) DO UPDATE SET generation = f.generation + 1;
    RETURN NULL;
END
$$;
CREATE TRIGGER checkout_policy_mode_updated AFTER UPDATE OF mode ON platform.checkout_origins
    FOR EACH ROW WHEN (OLD.mode IS DISTINCT FROM NEW.mode)
    EXECUTE FUNCTION platform.checkout_policy_changed();
CREATE TRIGGER checkout_policy_same_origin_inserted AFTER INSERT ON platform.checkout_origins
    FOR EACH ROW WHEN (NEW.mode = 'same_origin')
    EXECUTE FUNCTION platform.checkout_policy_changed();
CREATE TRIGGER checkout_policy_same_origin_deleted AFTER DELETE ON platform.checkout_origins
    FOR EACH ROW WHEN (OLD.mode = 'same_origin')
    EXECUTE FUNCTION platform.checkout_policy_changed();

ALTER TABLE carts
    ADD COLUMN checkout_origin     text CHECK (length(checkout_origin) <= 300),
    ADD COLUMN checkout_generation bigint CHECK (checkout_generation >= 0),
    ADD CONSTRAINT carts_checkout_binding
        CHECK (num_nulls(checkout_origin, checkout_generation) IN (0, 2));
ALTER TABLE checkout_handoffs
    ADD COLUMN origin     text CHECK (length(origin) <= 300),
    ADD COLUMN generation bigint CHECK (generation >= 0),
    ADD CONSTRAINT checkout_handoffs_binding CHECK (num_nulls(origin, generation) IN (0, 2));

REVOKE INSERT, UPDATE, DELETE ON platform.checkout_fences FROM app_runtime;
REVOKE ALL ON FUNCTION platform.checkout_fence_key(uuid), platform.enter_checkout_fence(uuid, text),
    platform.checkout_policy_changed() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.checkout_fence_key(uuid), platform.enter_checkout_fence(uuid, text)
    TO app_runtime;
