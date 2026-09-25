-- WP13b: CSV imports of customers, historical orders and newsletter subscribers, tenant data
-- exports, GDPR access/erasure support (spec §10.8, §11.5, §14, A20, A21, A28, A29).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs).

-- ---------------------------------------------------------------------------------------
-- CSV import runs. The CSV sits in the private bucket (`object_key`) from the analysis until
-- the run is applied; `mapping` maps our field names to the file's column headers.
CREATE TABLE data_imports (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    kind       text NOT NULL CHECK (kind IN ('customers', 'orders', 'subscribers')),
    -- Default locale for customers, the market of new subscribers.
    market_id  uuid NOT NULL,
    mapping    jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(mapping) = 'object'),
    object_key text NOT NULL CHECK (length(object_key) <= 300),
    status     text NOT NULL DEFAULT 'pending'
               CHECK (status IN ('pending', 'analyzing', 'analyzed', 'applying', 'applied',
                                 'failed')),
    report     jsonb,
    progress   jsonb NOT NULL DEFAULT '{}',
    error      text CHECK (length(error) <= 2000),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    applied_at timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id)
);
CREATE INDEX data_imports_recent ON data_imports (tenant_id, id DESC);

-- Historical orders from another shop (A28): an archive, deliberately outside `orders`, so
-- no order workflow (payments, stock, invoices, emails, webhooks, analytics, flows) can ever
-- act on them. Re-importing the same number updates the row.
CREATE TABLE archived_orders (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    number       text NOT NULL CHECK (length(number) BETWEEN 1 AND 64),
    placed_at    timestamptz NOT NULL,
    email        text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    customer_id  uuid,
    name         text CHECK (length(name) <= 200),
    phone        text CHECK (length(phone) <= 40),
    currency     text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    total_minor  bigint NOT NULL CHECK (total_minor >= 0),
    -- The old shop's status as given (`delivered`, `Vyřízeno`, ...), display only.
    status_label text CHECK (length(status_label) <= 64),
    -- {name, company, street, city, postal_code, country}
    address      jsonb CHECK (jsonb_typeof(address) = 'object'),
    -- [{sku, name, quantity, unit_price_minor}]
    lines        jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(lines) = 'array'),
    import_id    uuid,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT archived_orders_number_unique UNIQUE (tenant_id, number),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id)
        ON DELETE SET NULL (customer_id),
    FOREIGN KEY (tenant_id, import_id) REFERENCES data_imports (tenant_id, id)
        ON DELETE SET NULL (import_id)
);
CREATE INDEX archived_orders_placed ON archived_orders (tenant_id, placed_at DESC, id DESC);
CREATE INDEX archived_orders_email ON archived_orders (tenant_id, email);
CREATE INDEX archived_orders_customer ON archived_orders (tenant_id, customer_id)
    WHERE customer_id IS NOT NULL;

-- Full tenant exports (§10.8): a zip in the private bucket, downloaded through 5-minute
-- presigned URLs (A21).
CREATE TABLE data_exports (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    status       text NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'running', 'ready', 'failed')),
    object_key   text NOT NULL CHECK (length(object_key) <= 300),
    size_bytes   bigint,
    error        text CHECK (length(error) <= 2000),
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);
CREATE INDEX data_exports_recent ON data_exports (tenant_id, id DESC);

-- Consent evidence of imported subscribers, as the old shop recorded it (A20):
-- {source, at, ip, text_version, import_id}. The consent record carries the decision.
ALTER TABLE subscribers ADD COLUMN consent_evidence jsonb
    CHECK (jsonb_typeof(consent_evidence) = 'object');

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['data_imports', 'archived_orders', 'data_exports']
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

-- ---------------------------------------------------------------------------------------
-- GDPR erasure (§14, A29) of consent evidence. The application may only add consent records;
-- erasing a data subject replaces its id with a random one (the decisions stay as
-- anonymous evidence). Only the current tenant's rows (`app.tenant_id`, set by tenant_tx).
-- `missing_ok`: outside a tenant transaction (migrations, tooling) the owner sees nothing.
CREATE POLICY erasure_read ON consent_records FOR SELECT TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_update ON consent_records FOR UPDATE TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

CREATE FUNCTION platform.erase_consent_subject(p_type text, p_id text) RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH erased AS (
        UPDATE public.consent_records
        SET subject_id = 'erased-' || replace(gen_random_uuid()::text, '-', '')
        WHERE tenant_id = current_setting('app.tenant_id')::uuid
          AND subject_type = p_type AND subject_id = p_id
        RETURNING 1
    )
    SELECT count(*) FROM erased
$$;
REVOKE ALL ON FUNCTION platform.erase_consent_subject(text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.erase_consent_subject(text, text) TO app_runtime;

-- The order timeline and withdrawal declarations are immutable for the application; erasure
-- is the one exception, through this function: for the current tenant's `p_order_ids` it
-- clears the data of timeline events that carry personal data (staff notes, address changes),
-- anonymizes withdrawals (address, IBAN, note, declaration) and drops withdrawal links.
CREATE POLICY erasure_read ON order_events FOR SELECT TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_update ON order_events FOR UPDATE TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_read ON withdrawals FOR SELECT TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_update ON withdrawals FOR UPDATE TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid)
    WITH CHECK (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_read ON withdrawal_tokens FOR SELECT TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);
CREATE POLICY erasure_delete ON withdrawal_tokens FOR DELETE TO app_owner
    USING (tenant_id = nullif(current_setting('app.tenant_id', true), '')::uuid);

CREATE FUNCTION platform.erase_order_records(p_order_ids uuid[], p_email text, p_marker text)
RETURNS void
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
DECLARE
    v_tenant uuid := current_setting('app.tenant_id')::uuid;
BEGIN
    UPDATE public.order_events SET data = '{}'
    WHERE tenant_id = v_tenant AND order_id = ANY (p_order_ids)
      AND kind IN ('note', 'address_changed');
    UPDATE public.withdrawals
    SET email = p_email, iban = NULL, note = NULL, declaration = p_marker
    WHERE tenant_id = v_tenant AND order_id = ANY (p_order_ids);
    DELETE FROM public.withdrawal_tokens
    WHERE tenant_id = v_tenant AND order_id = ANY (p_order_ids);
END
$$;
REVOKE ALL ON FUNCTION platform.erase_order_records(uuid[], text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.erase_order_records(uuid[], text, text) TO app_runtime;
