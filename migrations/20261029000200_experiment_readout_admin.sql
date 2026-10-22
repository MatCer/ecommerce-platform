-- SHOP-99: why and when an experiment stopped, when it was promoted, two-arm experiments only,
-- and the promoted home hero copy.

ALTER TABLE experiments
    ADD COLUMN stop_reason text
        CHECK (stop_reason IN ('manual', 'promoted', 'srm', 'harm', 'max_duration')),
    ADD COLUMN promoted_at timestamptz;

-- Before the new freeze rules, and under a temporary policy: forced RLS applies to the owner.
CREATE POLICY shop99_backfill ON experiments TO app_owner USING (true) WITH CHECK (true);
UPDATE experiments SET stop_reason = 'manual' WHERE status = 'stopped';
DROP POLICY shop99_backfill ON experiments;

ALTER TABLE experiments
    ADD CONSTRAINT experiments_stop_reason CHECK ((status = 'stopped') = (stop_reason IS NOT NULL)),
    ADD CONSTRAINT experiments_promoted_stopped CHECK (promoted_at IS NULL OR status = 'stopped'),
    ADD CONSTRAINT experiments_promoted_at
        CHECK (stop_reason IS DISTINCT FROM 'promoted' OR promoted_at IS NOT NULL),
    ADD CONSTRAINT experiments_two_arms CHECK (jsonb_array_length(arms) = 2);

CREATE OR REPLACE FUNCTION platform.experiments_freeze() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF (NEW.id, NEW.tenant_id, NEW.name, NEW.version, NEW.arms, NEW.created_at,
        NEW.theme_arm, NEW.theme_revision_id)
       IS DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.name, OLD.version, OLD.arms, OLD.created_at,
                         OLD.theme_arm, OLD.theme_revision_id) THEN
        RAISE EXCEPTION 'experiment config is frozen; create a new version'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.stop_reason IS DISTINCT FROM OLD.stop_reason
       AND NOT (OLD.status = 'running' AND NEW.status = 'stopped') THEN
        RAISE EXCEPTION 'the stop reason is set once, when the experiment stops'
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.promoted_at IS NOT NULL THEN
        IF NEW.promoted_at IS DISTINCT FROM OLD.promoted_at THEN
            RAISE EXCEPTION 'the experiment is already promoted'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.promoted_at IS NOT NULL THEN
        NEW.promoted_at := now();
    END IF;
    NEW.started_at := OLD.started_at;
    NEW.stopped_at := OLD.stopped_at;
    IF NEW.status = OLD.status THEN
        RETURN NEW;
    ELSIF OLD.status = 'draft' AND NEW.status = 'running' THEN
        NEW.started_at := now();
    ELSIF OLD.status = 'running' AND NEW.status = 'stopped' THEN
        NEW.stopped_at := now();
    ELSE
        RAISE EXCEPTION 'experiment status cannot change from % to %', OLD.status, NEW.status
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;

CREATE TABLE home_settings (
    tenant_id uuid PRIMARY KEY REFERENCES platform.tenants (id),
    hero      jsonb NOT NULL CHECK (jsonb_typeof(hero) = 'object')
);
ALTER TABLE home_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE home_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON home_settings TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE ON home_settings TO app_runtime;
