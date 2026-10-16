CREATE FUNCTION experiment_arms_valid(arms jsonb) RETURNS boolean
LANGUAGE sql IMMUTABLE SET search_path = '' AS $$
    SELECT CASE WHEN jsonb_typeof(arms) IS DISTINCT FROM 'array' THEN false
                WHEN jsonb_array_length(arms) NOT BETWEEN 2 AND 8 THEN false
                ELSE coalesce((
                    SELECT bool_and(CASE
                        WHEN jsonb_typeof(a) <> 'object' THEN false
                        WHEN (SELECT count(*) FROM jsonb_object_keys(a)) <> 3 THEN false
                        WHEN jsonb_typeof(a -> 'key') IS DISTINCT FROM 'string'
                             OR (a ->> 'key') !~ '^[a-z0-9_-]{1,32}$' THEN false
                        WHEN jsonb_typeof(a -> 'config') IS DISTINCT FROM 'object' THEN false
                        WHEN jsonb_typeof(a -> 'weight') IS DISTINCT FROM 'number'
                             OR (a ->> 'weight') !~ '^[1-9][0-9]{0,4}$' THEN false
                        ELSE (a ->> 'weight')::integer <= 10000
                    END)
                    FROM jsonb_array_elements(arms) a), false)
                    AND (SELECT count(DISTINCT a ->> 'key') FROM jsonb_array_elements(arms) a)
                        = jsonb_array_length(arms)
           END
$$;

CREATE TABLE experiments (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    name       text NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
    version    integer NOT NULL CHECK (version > 0),
    arms       jsonb NOT NULL CHECK (experiment_arms_valid(arms)),
    status     text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'running', 'stopped')),
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    stopped_at timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, name, version),
    UNIQUE (tenant_id, id),
    CHECK (CASE status
               WHEN 'draft' THEN started_at IS NULL AND stopped_at IS NULL
               WHEN 'running' THEN started_at IS NOT NULL AND stopped_at IS NULL
               ELSE started_at IS NOT NULL AND stopped_at IS NOT NULL
           END)
);

CREATE UNIQUE INDEX experiments_one_running ON experiments (tenant_id) WHERE status = 'running';

CREATE FUNCTION platform.experiments_freeze() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF (NEW.id, NEW.tenant_id, NEW.name, NEW.version, NEW.arms, NEW.created_at)
       IS DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.name, OLD.version, OLD.arms, OLD.created_at) THEN
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

CREATE TRIGGER experiments_freeze BEFORE UPDATE ON experiments
    FOR EACH ROW EXECUTE FUNCTION platform.experiments_freeze();

CREATE TABLE experiment_assignments (
    tenant_id     uuid NOT NULL REFERENCES platform.tenants (id),
    experiment_id uuid NOT NULL,
    anon_id       text NOT NULL CHECK (anon_id ~ '^[0-9a-f]{32}$'),
    arm           text NOT NULL CHECK (arm ~ '^[a-z0-9_-]{1,32}$'),
    assigned_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, experiment_id, anon_id),
    FOREIGN KEY (tenant_id, experiment_id) REFERENCES experiments (tenant_id, id)
);

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['experiments', 'experiment_assignments']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I TO app_runtime
                 USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                 WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
    END LOOP;
END
$$;

GRANT SELECT, INSERT, UPDATE ON experiments TO app_runtime;
GRANT SELECT, INSERT ON experiment_assignments TO app_runtime;

CREATE POLICY experiment_assignments_retention_read ON experiment_assignments
    FOR SELECT TO app_owner USING (assigned_at < now() - interval '13 months');
CREATE POLICY experiment_assignments_retention_delete ON experiment_assignments
    FOR DELETE TO app_owner USING (assigned_at < now() - interval '13 months');

CREATE FUNCTION platform.purge_experiment_assignments() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
    WITH deleted AS (
        DELETE FROM public.experiment_assignments
        WHERE assigned_at < now() - interval '13 months' RETURNING 1
    ) SELECT count(*) FROM deleted
$$;
REVOKE ALL ON FUNCTION platform.purge_experiment_assignments() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.purge_experiment_assignments() TO app_runtime;
