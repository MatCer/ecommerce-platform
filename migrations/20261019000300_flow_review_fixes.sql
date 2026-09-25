-- Keep each run's schedule stable while staff edit the live definition.
ALTER TABLE flow_runs ADD COLUMN config_snapshot jsonb NOT NULL DEFAULT '{}';
UPDATE flow_runs r SET config_snapshot = d.config FROM flow_definitions d
WHERE r.definition_id = d.id;
ALTER TABLE flow_runs ADD COLUMN source_generation integer NOT NULL DEFAULT 1
    CHECK (source_generation > 0);
ALTER TABLE flow_runs ADD COLUMN coupon_code text;
ALTER TABLE flow_runs DROP CONSTRAINT flow_runs_tenant_id_definition_id_source_id_key;
ALTER TABLE flow_runs ADD CONSTRAINT flow_runs_source_generation_key
    UNIQUE (tenant_id, definition_id, source_id, source_generation);

ALTER TABLE flow_watches ADD COLUMN generation integer NOT NULL DEFAULT 1
    CHECK (generation > 0);

-- A serialized, durable daily budget across variants. The IP identifier is a salted daily
-- hash; recipient identifiers are removed during erasure. This bookkeeping is never exported.
CREATE TABLE flow_watch_mail_quotas (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    scope text NOT NULL CHECK (scope IN ('recipient', 'ip')),
    identifier text NOT NULL,
    window_started_at timestamptz NOT NULL DEFAULT now(),
    sent_count integer NOT NULL DEFAULT 1 CHECK (sent_count > 0),
    PRIMARY KEY (tenant_id, scope, identifier)
);
ALTER TABLE flow_watch_mail_quotas ENABLE ROW LEVEL SECURITY;
ALTER TABLE flow_watch_mail_quotas FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON flow_watch_mail_quotas TO app_runtime
    USING (tenant_id=current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id=current_setting('app.tenant_id')::uuid);
GRANT SELECT,INSERT,UPDATE,DELETE ON flow_watch_mail_quotas TO app_runtime;
