-- Each abandoned-cart message gets an independent one-click withdrawal capability. A click
-- records email_marketing=false in the consent ledger and cancels all active cart runs.
CREATE TABLE flow_unsubscribe_tokens (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id),
    token_hash bytea NOT NULL CHECK (length(token_hash)=32),
    email text NOT NULL,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(tenant_id,token_hash)
);
CREATE UNIQUE INDEX flow_unsubscribe_token_hash ON flow_unsubscribe_tokens(token_hash);
ALTER TABLE flow_unsubscribe_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE flow_unsubscribe_tokens FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON flow_unsubscribe_tokens TO app_runtime
    USING (tenant_id=current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id=current_setting('app.tenant_id')::uuid);
GRANT SELECT,INSERT,UPDATE,DELETE ON flow_unsubscribe_tokens TO app_runtime;
