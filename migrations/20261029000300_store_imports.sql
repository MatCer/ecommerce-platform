-- SHOP-43: imports from the merchant's current shop reuse the feed import runs and mappings
-- with source 'store'. A store import only creates drafts, and a tenant runs one at a time.

ALTER TABLE import_runs
    DROP CONSTRAINT import_runs_source_check,
    ADD CONSTRAINT import_runs_source_check CHECK (source IN ('heureka', 'google', 'store')),
    ADD CONSTRAINT import_runs_store_drafts CHECK (source <> 'store' OR NOT activate);

ALTER TABLE import_mappings
    DROP CONSTRAINT import_mappings_source_check,
    ADD CONSTRAINT import_mappings_source_check CHECK (source IN ('heureka', 'google', 'store'));

CREATE UNIQUE INDEX import_runs_one_store_import ON import_runs (tenant_id)
    WHERE source = 'store' AND status IN ('analyzing', 'applying');
