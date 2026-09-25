-- WP19: tenant-configured lifecycle flows, durable executions and watchdog capabilities.
CREATE TABLE flow_definitions (
    id uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    kind text NOT NULL CHECK (kind IN ('abandoned_cart', 'watchdog', 'review_invite')),
    enabled boolean NOT NULL DEFAULT true,
    config jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(config) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(id), UNIQUE(tenant_id, id), UNIQUE(tenant_id, kind)
);

CREATE TABLE flow_runs (
    id uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    definition_id uuid NOT NULL,
    source_kind text NOT NULL CHECK (source_kind IN ('cart', 'order', 'watch')),
    source_id uuid NOT NULL,
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'completed', 'cancelled', 'failed')),
    next_step integer NOT NULL DEFAULT 0 CHECK (next_step BETWEEN 0 AND 16),
    due_at timestamptz NOT NULL,
    exit_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(id), UNIQUE(tenant_id, id), UNIQUE(tenant_id, definition_id, source_id),
    FOREIGN KEY(tenant_id, definition_id) REFERENCES flow_definitions(tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX flow_runs_due ON flow_runs(tenant_id, due_at, id) WHERE status = 'active';

CREATE TABLE flow_steps (
    id uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    run_id uuid NOT NULL,
    step_number integer NOT NULL CHECK (step_number BETWEEN 0 AND 15),
    status text NOT NULL CHECK (status IN ('sent', 'skipped', 'failed')),
    message_id uuid,
    reason text,
    executed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(id), UNIQUE(tenant_id, id), UNIQUE(tenant_id, run_id, step_number),
    FOREIGN KEY(tenant_id, run_id) REFERENCES flow_runs(tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY(tenant_id, message_id) REFERENCES email_messages(tenant_id, id) ON DELETE SET NULL(message_id)
);

CREATE TABLE flow_restore_tokens (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    token_hash bytea NOT NULL CHECK (length(token_hash) = 32),
    cart_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(tenant_id, token_hash),
    FOREIGN KEY(tenant_id, cart_id) REFERENCES carts(tenant_id, id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX flow_restore_token_hash ON flow_restore_tokens(token_hash);

CREATE TABLE flow_watches (
    id uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    market_id uuid NOT NULL,
    variant_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('back_in_stock', 'price_drop')),
    target_minor bigint CHECK (target_minor > 0),
    email text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    locale text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'fired', 'unsubscribed')),
    confirm_hash bytea CHECK (length(confirm_hash) = 32),
    unsubscribe_hash bytea NOT NULL CHECK (length(unsubscribe_hash) = 32),
    confirm_expires_at timestamptz,
    confirmed_at timestamptz,
    fired_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(id), UNIQUE(tenant_id, id),
    UNIQUE(tenant_id, market_id, variant_id, kind, email),
    FOREIGN KEY(tenant_id, market_id) REFERENCES markets(tenant_id, id),
    FOREIGN KEY(tenant_id, variant_id) REFERENCES variants(tenant_id, id)
);
CREATE UNIQUE INDEX flow_watches_confirm_hash ON flow_watches(confirm_hash) WHERE confirm_hash IS NOT NULL;
CREATE UNIQUE INDEX flow_watches_unsubscribe_hash ON flow_watches(unsubscribe_hash);
CREATE INDEX flow_watches_trigger ON flow_watches(tenant_id, variant_id, kind) WHERE status = 'confirmed';

-- Clock offsets are never consulted when APP_ENV=prod. This table exists to make integration
-- tests deterministic without changing wall time or queue scheduling for other tenants.
CREATE TABLE flow_test_clocks (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants(id),
    offset_seconds bigint NOT NULL DEFAULT 0 CHECK (offset_seconds BETWEEN 0 AND 31536000),
    updated_at timestamptz NOT NULL DEFAULT now()
);

DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['flow_definitions', 'flow_runs', 'flow_steps',
                              'flow_restore_tokens', 'flow_watches', 'flow_test_clocks'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format('CREATE POLICY tenant_isolation ON %I TO app_runtime
            USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
            WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_runtime', t);
    END LOOP;
END $$;
