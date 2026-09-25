-- WP23: tenant theme sources and the builder pipeline (spec §7.5, §12.3, A6, A21, A22, A30).
--
-- A custom revision owns a source archive (`theme-sources/<tenant>/<revision>.tar.gz` in the
-- private bucket, validated per A6). The builder turns it into an artifact: `artifact_id` is
-- empty while the revision is a draft or building, and stays empty when the build failed.

ALTER TABLE platform.theme_artifacts
    -- The source archive a default-theme artifact was built from (`make theme-build`); forks
    -- and resets start from it.
    ADD COLUMN source_key text;

ALTER TABLE theme_revisions
    ALTER COLUMN artifact_id DROP NOT NULL,
    ADD COLUMN source_key text,
    -- How the revision came about: `default` (follows the shared artifact, A30), `fork` (a copy
    -- of the default source), `tokens` (token edit, no code change: fast-path gates),
    -- `upload` (archive from a power user), `reset` (latest default + the tenant's tokens),
    -- `ai` (WP24).
    ADD COLUMN change text NOT NULL DEFAULT 'default'
        CHECK (change IN ('default', 'fork', 'tokens', 'upload', 'reset', 'ai')),
    ADD COLUMN prompt text CHECK (length(prompt) <= 4000),
    -- When the status last moved (stuck builds are expired from it).
    ADD COLUMN status_changed_at timestamptz NOT NULL DEFAULT now(),
    ADD CONSTRAINT theme_revisions_artifact_present
        CHECK (artifact_id IS NOT NULL OR status IN ('draft', 'building', 'failed')),
    ADD CONSTRAINT theme_revisions_custom_source
        CHECK (origin = 'default' OR source_key IS NOT NULL),
    ADD CONSTRAINT theme_revisions_change_origin
        CHECK ((origin = 'default') = (change = 'default'));

-- The admin list and the builder's lookups go by tenant, newest first.
CREATE INDEX theme_revisions_status ON theme_revisions (tenant_id, status, status_changed_at);

-- Artifact GC (worker): unreferenced theme artifacts are deleted; the foreign keys from
-- `theme_revisions` (checked regardless of RLS) keep every referenced one.
GRANT DELETE ON platform.theme_artifacts TO app_runtime;
