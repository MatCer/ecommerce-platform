-- WP1: tenants, domains, platform admins, and the first tenant tables (spec §5, A8, A12).
--
-- Role model (docker/postgres/init-roles.sh):
--   app_owner   owns every table and function here and runs migrations.
--   app_runtime owns nothing, has no BYPASSRLS; api, worker and CLI connect as it.
--
-- Tenant tables live in `public`. Each one has `tenant_id`, a `tenant_isolation` policy for
-- app_runtime keyed on the transaction-local `app.tenant_id` setting, and FORCE RLS so not
-- even the owner reads across tenants by accident. `current_setting('app.tenant_id')` raises
-- when the setting is missing and the uuid cast raises when it is empty, so a query without
-- tenant context fails instead of returning data (platform::db::tenant_tx sets it).
-- Cross-tenant platform reads go through SECURITY DEFINER functions owned by app_owner,
-- backed by narrow `TO app_owner` policies.

-- UUIDv7 (spec D4) for defaults in SQL. ponytail: replace with the built-in uuidv7() on PG 18.
CREATE FUNCTION platform.uuid_v7() RETURNS uuid
LANGUAGE sql VOLATILE PARALLEL SAFE
SET search_path = ''
AS $$
    -- 48-bit unix millis, then random bits with the version nibble set to 7 (bits 52, 53).
    SELECT encode(
        set_bit(set_bit(overlay(
            uuid_send(gen_random_uuid())
            PLACING substring(int8send((extract(epoch FROM clock_timestamp()) * 1000)::bigint) FROM 3)
            FROM 1 FOR 6
        ), 52, 1), 53, 1),
        'hex')::uuid
$$;

-- ---------------------------------------------------------------------------------------
-- Platform tables (no RLS; reached only through explicit platform services).

CREATE TABLE platform.tenants (
    id                  uuid PRIMARY KEY DEFAULT platform.uuid_v7(),
    slug                text NOT NULL UNIQUE
                        CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
    name                text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    status              text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    plan                text NOT NULL DEFAULT 'standard',
    application_fee_bps integer NOT NULL DEFAULT 0 CHECK (application_fee_bps BETWEEN 0 AND 10000),
    legal_entity        jsonb NOT NULL DEFAULT '{}',
    settings            jsonb NOT NULL DEFAULT '{}',
    created_at          timestamptz NOT NULL DEFAULT now()
);

-- Better Auth user ids are opaque strings.
CREATE TABLE platform.platform_admins (
    user_id    text PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- Tenant tables.

CREATE TABLE markets (
    id             uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id      uuid NOT NULL REFERENCES platform.tenants (id),
    code           text NOT NULL CHECK (code ~ '^[a-z0-9][a-z0-9-]{0,31}$'),
    name           text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    country_codes  text[] NOT NULL CHECK (cardinality(country_codes) > 0),
    currency       text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    default_locale text NOT NULL,
    locales        text[] NOT NULL CHECK (default_locale = ANY (locales)),
    -- Price lists arrive in WP4; the FK is added there.
    price_list_id  uuid,
    tax_mode       text NOT NULL DEFAULT 'gross' CHECK (tax_mode IN ('gross', 'net')),
    is_default     boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, code)
);
CREATE UNIQUE INDEX markets_one_default ON markets (tenant_id) WHERE is_default;

-- Hostname -> (tenant, market). Unverified domains never resolve (A29 stub: TXT record
-- `_commerce-verification.<hostname>` must contain `commerce-verification=<token>`).
CREATE TABLE platform.domains (
    hostname           text PRIMARY KEY CHECK (hostname = lower(hostname) AND length(hostname) <= 253),
    tenant_id          uuid NOT NULL REFERENCES platform.tenants (id),
    market_id          uuid NOT NULL,
    is_primary         boolean NOT NULL DEFAULT false,
    verification_token text NOT NULL DEFAULT replace(gen_random_uuid()::text, '-', ''),
    verified_at        timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id)
);
CREATE INDEX domains_tenant ON platform.domains (tenant_id);
CREATE UNIQUE INDEX domains_one_primary_per_market ON platform.domains (market_id) WHERE is_primary;

CREATE TABLE staff_members (
    id         uuid NOT NULL DEFAULT platform.uuid_v7() PRIMARY KEY,
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    user_id    text NOT NULL,
    email      text NOT NULL,
    role       text NOT NULL CHECK (role IN ('owner', 'admin', 'staff')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, user_id)
);
CREATE INDEX staff_members_user ON staff_members (user_id);

CREATE TABLE audit_log (
    id        uuid NOT NULL DEFAULT platform.uuid_v7() PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    actor     text NOT NULL,
    action    text NOT NULL,
    entity    text NOT NULL,
    entity_id text,
    diff      jsonb NOT NULL DEFAULT '{}',
    at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_tenant_id ON audit_log (tenant_id, id DESC);

-- A12. Retention 24 h (purged by the worker's maintenance job).
CREATE TABLE idempotency_keys (
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    operation    text NOT NULL,
    key          text NOT NULL CHECK (length(key) BETWEEN 1 AND 255),
    request_hash text NOT NULL,
    status       text NOT NULL DEFAULT 'completed' CHECK (status IN ('in_progress', 'completed')),
    response     jsonb,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, operation, key)
);
CREATE INDEX idempotency_keys_created ON idempotency_keys (created_at);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

ALTER TABLE markets ENABLE ROW LEVEL SECURITY;
ALTER TABLE markets FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON markets TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);

ALTER TABLE staff_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE staff_members FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON staff_members TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
-- Membership bootstrap (A8) runs before any tenant context exists.
CREATE POLICY membership_lookup ON staff_members FOR SELECT TO app_owner USING (true);

ALTER TABLE audit_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_log FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON audit_log TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);

ALTER TABLE idempotency_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE idempotency_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON idempotency_keys TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
-- Retention purge across tenants (platform.purge_idempotency_keys).
CREATE POLICY retention_read ON idempotency_keys FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON idempotency_keys FOR DELETE TO app_owner
    USING (created_at < now() - interval '24 hours');

GRANT SELECT, INSERT, UPDATE, DELETE ON markets, staff_members, idempotency_keys TO app_runtime;
-- The audit log is append-only for the application.
GRANT SELECT, INSERT ON audit_log TO app_runtime;

-- ---------------------------------------------------------------------------------------
-- SECURITY DEFINER functions. `search_path = ''` and schema-qualified names keep callers
-- from redirecting them; EXECUTE is revoked from PUBLIC and granted to app_runtime only.

-- A8: the role of `user_id` in an active tenant, or NULL. No business access before this.
CREATE FUNCTION platform.staff_membership(p_user_id text, p_tenant_id uuid) RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT s.role
    FROM public.staff_members s
    JOIN platform.tenants t ON t.id = s.tenant_id
    WHERE s.user_id = p_user_id AND s.tenant_id = p_tenant_id AND t.status = 'active'
$$;

-- The tenants a user can act in (tenant switcher, /admin/v1/me).
CREATE FUNCTION platform.staff_tenants(p_user_id text)
RETURNS TABLE (tenant_id uuid, slug text, name text, role text)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT t.id, t.slug, t.name, s.role
    FROM public.staff_members s
    JOIN platform.tenants t ON t.id = s.tenant_id
    WHERE s.user_id = p_user_id AND t.status = 'active'
    ORDER BY t.name, t.id
$$;

CREATE FUNCTION platform.purge_idempotency_keys() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH gone AS (
        DELETE FROM public.idempotency_keys WHERE created_at < now() - interval '24 hours'
        RETURNING 1
    )
    SELECT count(*) FROM gone
$$;

REVOKE ALL ON FUNCTION platform.staff_membership(text, uuid), platform.staff_tenants(text),
    platform.purge_idempotency_keys() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.staff_membership(text, uuid), platform.staff_tenants(text),
    platform.purge_idempotency_keys() TO app_runtime;
