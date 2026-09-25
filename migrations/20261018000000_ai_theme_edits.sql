-- WP24: AI theme editing (spec §12.3, A6). A run is one merchant prompt: an agent loop in the
-- worker edits a copy of the base revision's source through contract-restricted file tools and
-- checks it through the WP23 builder (each check is a `change = 'ai'` revision). The run keeps
-- the transcript, the diff and the last check report; the staff accepts (the final revision may
-- then be published) or discards it.

CREATE TABLE ai_theme_runs (
    id               uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id        uuid NOT NULL REFERENCES platform.tenants (id),
    base_revision_id uuid NOT NULL,
    prompt           text NOT NULL CHECK (length(prompt) BETWEEN 1 AND 4000),
    status           text NOT NULL DEFAULT 'queued'
                     CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled',
                                       'accepted', 'discarded')),
    cancel_requested boolean NOT NULL DEFAULT false,
    model            text,
    -- Hard limits are enforced from these counters (model calls, check runs, usage).
    turns            integer NOT NULL DEFAULT 0 CHECK (turns >= 0),
    checks_run       integer NOT NULL DEFAULT 0 CHECK (checks_run >= 0),
    tokens           bigint NOT NULL DEFAULT 0 CHECK (tokens >= 0),
    cost_micros      bigint NOT NULL DEFAULT 0 CHECK (cost_micros >= 0),
    -- The Messages API history (append-only; model output is untrusted data).
    transcript       jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(transcript) = 'array'),
    -- What the admin shows as progress: one entry per tool call.
    steps            jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(steps) = 'array'),
    -- The latest checked revision (the one accept makes publishable).
    revision_id      uuid,
    diff             text CHECK (length(diff) <= 1048576),
    report           jsonb,
    summary          text CHECK (length(summary) <= 4000),
    error            text CHECK (length(error) <= 1000),
    created_by       text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    finished_at      timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, base_revision_id) REFERENCES theme_revisions (tenant_id, id),
    FOREIGN KEY (tenant_id, revision_id) REFERENCES theme_revisions (tenant_id, id)
);
CREATE INDEX ai_theme_runs_tenant ON ai_theme_runs (tenant_id, id DESC);
-- One active run per tenant (abuse control for the shared builder and the AI budget).
CREATE UNIQUE INDEX ai_theme_runs_one_active ON ai_theme_runs (tenant_id)
    WHERE status IN ('queued', 'running');

ALTER TABLE theme_revisions
    ADD COLUMN ai_run_id uuid,
    ADD FOREIGN KEY (tenant_id, ai_run_id) REFERENCES ai_theme_runs (tenant_id, id),
    ADD CONSTRAINT theme_revisions_ai_run CHECK ((change = 'ai') = (ai_run_id IS NOT NULL));

ALTER TABLE ai_theme_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE ai_theme_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON ai_theme_runs TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE ON ai_theme_runs TO app_runtime;
