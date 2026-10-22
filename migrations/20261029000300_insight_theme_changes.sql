ALTER TABLE insight_signals ADD UNIQUE (tenant_id, id);

ALTER TABLE ai_theme_runs
    ADD COLUMN insight_id uuid,
    ADD FOREIGN KEY (tenant_id, insight_id) REFERENCES insight_signals (tenant_id, id)
        ON DELETE SET NULL (insight_id);
