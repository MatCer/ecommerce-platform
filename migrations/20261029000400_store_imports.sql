ALTER TABLE import_runs
    DROP CONSTRAINT import_runs_source_check,
    ADD CONSTRAINT import_runs_source_check CHECK (source IN ('heureka', 'google', 'store')),
    ADD CONSTRAINT import_runs_store_drafts CHECK (source <> 'store' OR NOT activate);

ALTER TABLE import_mappings
    DROP CONSTRAINT import_mappings_source_check,
    ADD CONSTRAINT import_mappings_source_check CHECK (source IN ('heureka', 'google', 'store'));

CREATE UNIQUE INDEX import_runs_one_store_import ON import_runs (tenant_id)
    WHERE source = 'store' AND status IN ('analyzing', 'applying');
