-- SHOP-100: a whole-theme test. One arm of a two-arm experiment renders another revision of the
-- tenant's theme; the other arm is control (the active theme). With one running experiment per
-- tenant, a tenant serves at most two artifacts to its visitors: control plus one.

ALTER TABLE experiments
    ADD COLUMN theme_arm text,
    ADD COLUMN theme_revision_id uuid,
    ADD CONSTRAINT experiments_theme_revision
        FOREIGN KEY (tenant_id, theme_revision_id) REFERENCES theme_revisions (tenant_id, id),
    ADD CONSTRAINT experiments_theme_arm CHECK (
        (theme_arm IS NULL) = (theme_revision_id IS NULL)
        AND (theme_arm IS NULL
             OR (jsonb_array_length(arms) = 2
                 AND arms @> jsonb_build_array(jsonb_build_object('key', theme_arm)))));

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

CREATE FUNCTION platform.experiments_theme_arm_gated() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM public.theme_revisions
        WHERE tenant_id = NEW.tenant_id AND id = NEW.theme_revision_id
          AND artifact_id IS NOT NULL AND status IN ('ready', 'published', 'superseded')
    ) THEN
        RAISE EXCEPTION 'the theme arm''s revision has not passed its gates or has no artifact'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER experiments_theme_arm_gated BEFORE INSERT OR UPDATE OF status ON experiments
    FOR EACH ROW WHEN (NEW.status = 'running' AND NEW.theme_revision_id IS NOT NULL)
    EXECUTE FUNCTION platform.experiments_theme_arm_gated();
