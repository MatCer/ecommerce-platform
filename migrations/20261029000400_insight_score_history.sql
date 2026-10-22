ALTER TABLE insight_signals
    DROP CONSTRAINT insight_signals_kind_check,
    DROP CONSTRAINT insight_signals_check4,
    ADD COLUMN dismissed_score double precision
        CHECK (dismissed_score > 0 AND dismissed_score < 'Infinity'),
    ADD COLUMN reopened_on date,
    ADD COLUMN hypothesis_score double precision
        CHECK (hypothesis_score > 0 AND hypothesis_score < 'Infinity');

CREATE TABLE insight_scores (
    tenant_id uuid NOT NULL,
    signal_id uuid NOT NULL,
    day       date NOT NULL,
    score     double precision NOT NULL CHECK (score > 0 AND score < 'Infinity'),
    PRIMARY KEY (tenant_id, signal_id, day),
    FOREIGN KEY (tenant_id, signal_id) REFERENCES insight_signals (tenant_id, id) ON DELETE CASCADE
);

CREATE POLICY shop110_backfill ON insight_signals TO app_owner USING (true) WITH CHECK (true);
UPDATE insight_signals SET dismissed_score = score WHERE status = 'dismissed';
UPDATE insight_signals SET hypothesis_score = score WHERE hypothesis_at IS NOT NULL;
INSERT INTO insight_scores (tenant_id, signal_id, day, score)
    SELECT tenant_id, id, last_detected_on, score FROM insight_signals;
DROP POLICY shop110_backfill ON insight_signals;

ALTER TABLE insight_signals
    ADD CHECK (kind = detection->>'kind'),
    ADD CHECK (num_nonnulls(dismissed_at, dismissed_by, dismissed_score) IN (0, 3)),
    ADD CHECK (CASE WHEN status = 'dismissed'
                    THEN dismissed_at IS NOT NULL AND reopened_on IS NULL
                    ELSE (dismissed_at IS NULL) = (reopened_on IS NULL) END),
    ADD CHECK ((hypothesis_at IS NULL) = (hypothesis_score IS NULL));

ALTER TABLE insight_scores ENABLE ROW LEVEL SECURITY;
ALTER TABLE insight_scores FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON insight_scores TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);

GRANT SELECT, INSERT, UPDATE ON insight_scores TO app_runtime;
