-- WP22: AI gateway metering, quotas, glossary, proposals, AI Act markers and bulk plans
-- (spec §7.7, §12, A9, A12).
--
-- Tenant tables follow the WP1 rules: tenant_id, RLS + FORCE, composite FKs.

-- Superadmin override of the plan's monthly token quota (`api admin set-ai-quota`);
-- NULL = the plan default from AI_PLAN_QUOTAS.
ALTER TABLE platform.tenants
    ADD COLUMN ai_monthly_tokens bigint CHECK (ai_monthly_tokens >= 0);

-- One row per completed model call (append-only for the application).
CREATE TABLE ai_usage (
    id                 uuid NOT NULL DEFAULT platform.uuid_v7() PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES platform.tenants (id),
    feature            text NOT NULL CHECK (feature ~ '^[a-z_]{1,40}$'),
    model              text NOT NULL CHECK (length(model) BETWEEN 1 AND 100),
    input_tokens       bigint NOT NULL CHECK (input_tokens >= 0),
    output_tokens      bigint NOT NULL CHECK (output_tokens >= 0),
    cache_read_tokens  bigint NOT NULL DEFAULT 0 CHECK (cache_read_tokens >= 0),
    cache_write_tokens bigint NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0),
    -- USD micros from the configured price table at the time of the call.
    cost_micros        bigint NOT NULL CHECK (cost_micros >= 0),
    actor              text NOT NULL,
    at                 timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ai_usage_tenant_at ON ai_usage (tenant_id, at);

-- Terms kept as-is (brand names) or translated one fixed way: [{term, translations{locale}}].
CREATE TABLE ai_glossaries (
    tenant_id  uuid PRIMARY KEY REFERENCES platform.tenants (id),
    entries    jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(entries) = 'array'),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- A generated proposal (description, SEO, translation) waiting for the staff's decision.
-- `changes` holds the proposed field values; nothing is written to the entity until the
-- staff accepts fields.
CREATE TABLE ai_proposals (
    id          uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    kind        text NOT NULL CHECK (kind IN ('product_description', 'seo',
                                              'category_description', 'translate')),
    entity_type text NOT NULL CHECK (entity_type IN ('product', 'category', 'page', 'menu')),
    -- A uuid, or the handle of a menu.
    entity_id   text NOT NULL CHECK (length(entity_id) BETWEEN 1 AND 64),
    params      jsonb NOT NULL CHECK (jsonb_typeof(params) = 'object'),
    status      text NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'ready', 'failed', 'accepted', 'discarded')),
    progress    jsonb NOT NULL DEFAULT '{}',
    changes     jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(changes) = 'array'),
    warnings    jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(warnings) = 'array'),
    error       text CHECK (length(error) <= 500),
    model       text,
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);
CREATE INDEX ai_proposals_entity ON ai_proposals (tenant_id, entity_type, entity_id, id DESC);

-- AI Act transparency: a field written from an accepted AI proposal. The label shows while the
-- stored value still hashes to `value_sha256` (a human rewrite of the field drops it).
CREATE TABLE ai_marks (
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    entity_type     text NOT NULL CHECK (entity_type IN ('product', 'category', 'page', 'menu')),
    entity_id       text NOT NULL,
    locale          text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    field           text NOT NULL CHECK (field ~ '^[a-z_]{1,40}$'),
    value_sha256    text NOT NULL CHECK (value_sha256 ~ '^[0-9a-f]{64}$'),
    feature         text NOT NULL,
    model           text NOT NULL,
    -- The proposal or bulk plan the text came from.
    proposal_id     uuid,
    accepted_by     text NOT NULL,
    ai_generated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, entity_type, entity_id, locale, field)
);

-- Bulk edit by prompt (§12.2). The plan is the validated model output; the targets are
-- resolved by the API (one item per product) and frozen when the plan is previewed.
CREATE TABLE ai_bulk_plans (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    prompt       text NOT NULL CHECK (length(prompt) BETWEEN 1 AND 2000),
    status       text NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'ready', 'rejected', 'applying', 'applied',
                                   'failed')),
    plan         jsonb,
    errors       jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(errors) = 'array'),
    target_count integer NOT NULL DEFAULT 0 CHECK (target_count >= 0),
    sample       jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(sample) = 'array'),
    progress     jsonb NOT NULL DEFAULT '{}',
    -- What the plan's codes/slugs/keys resolved to at preview time (applied as previewed).
    refs         jsonb NOT NULL DEFAULT '{}',
    model        text,
    created_by   text NOT NULL,
    applied_by   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    applied_at   timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);
CREATE INDEX ai_bulk_plans_recent ON ai_bulk_plans (tenant_id, id DESC);

CREATE TABLE ai_bulk_items (
    tenant_id  uuid NOT NULL,
    plan_id    uuid NOT NULL,
    -- No FK to products: a product deleted before the apply is skipped.
    product_id uuid NOT NULL,
    status     text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'done', 'skipped')),
    PRIMARY KEY (tenant_id, plan_id, product_id),
    FOREIGN KEY (tenant_id, plan_id) REFERENCES ai_bulk_plans (tenant_id, id) ON DELETE CASCADE
);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['ai_usage', 'ai_glossaries', 'ai_proposals', 'ai_marks',
        'ai_bulk_plans', 'ai_bulk_items']
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

GRANT SELECT, INSERT, UPDATE, DELETE
    ON ai_glossaries, ai_proposals, ai_marks, ai_bulk_plans, ai_bulk_items TO app_runtime;
-- Metering is append-only for the application.
GRANT SELECT, INSERT ON ai_usage TO app_runtime;
