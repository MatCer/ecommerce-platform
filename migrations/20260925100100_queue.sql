-- WP1: transactional outbox and leased job queue (spec §7.7, §13, A8, A14).
--
-- Schema `queue` has no RLS and no table grants: app_runtime reaches it only through the
-- SECURITY DEFINER functions below. Handlers do their tenant work inside tenant_tx.
-- ponytail: one global queue, no per-tenant fairness (A30); add a per-tenant running cap in
-- queue.claim if one tenant starves the others.

CREATE SCHEMA queue;
REVOKE ALL ON SCHEMA queue FROM PUBLIC;
GRANT USAGE ON SCHEMA queue TO app_runtime;

CREATE TABLE queue.outbox (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id     uuid,
    type          text NOT NULL,
    payload       jsonb NOT NULL DEFAULT '{}',
    created_at    timestamptz NOT NULL DEFAULT now(),
    dispatched_at timestamptz
);
CREATE INDEX outbox_pending ON queue.outbox (id) WHERE dispatched_at IS NULL;

CREATE TABLE queue.jobs (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id       uuid,
    queue           text NOT NULL DEFAULT 'default',
    kind            text NOT NULL,
    payload         jsonb NOT NULL DEFAULT '{}',
    run_at          timestamptz NOT NULL DEFAULT now(),
    attempts        integer NOT NULL DEFAULT 0,
    max_attempts    integer NOT NULL DEFAULT 10 CHECK (max_attempts > 0),
    status          text NOT NULL DEFAULT 'queued'
                    CHECK (status IN ('queued', 'running', 'done', 'dead')),
    lease_owner     text,
    lease_token     uuid,
    locked_until    timestamptz,
    last_error      text,
    idempotency_key text UNIQUE,
    created_at      timestamptz NOT NULL DEFAULT now(),
    finished_at     timestamptz,
    CHECK ((status = 'running') = (lease_token IS NOT NULL AND locked_until IS NOT NULL))
);
CREATE INDEX jobs_ready ON queue.jobs (queue, run_at, id) WHERE status = 'queued';
CREATE INDEX jobs_leased ON queue.jobs (locked_until) WHERE status = 'running';
CREATE INDEX jobs_finished ON queue.jobs (finished_at) WHERE status = 'done';

-- Records an event in the caller's transaction. The tenant comes from the transaction's
-- tenant context (NULL for platform events), so callers cannot publish for another tenant.
CREATE FUNCTION queue.publish(p_type text, p_payload jsonb) RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    INSERT INTO queue.outbox (tenant_id, type, payload)
    VALUES (nullif(current_setting('app.tenant_id', true), '')::uuid, p_type, p_payload)
    RETURNING id
$$;

-- Adds a job; with an idempotency key an existing job is returned instead of a duplicate.
CREATE FUNCTION queue.enqueue(
    p_kind text,
    p_payload jsonb,
    p_tenant_id uuid DEFAULT NULL,
    p_queue text DEFAULT 'default',
    p_run_at timestamptz DEFAULT NULL,
    p_max_attempts integer DEFAULT 10,
    p_idempotency_key text DEFAULT NULL
) RETURNS bigint
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
DECLARE
    v_id bigint;
BEGIN
    INSERT INTO queue.jobs (tenant_id, queue, kind, payload, run_at, max_attempts, idempotency_key)
    VALUES (p_tenant_id, p_queue, p_kind, p_payload, coalesce(p_run_at, now()), p_max_attempts,
            p_idempotency_key)
    ON CONFLICT (idempotency_key) DO NOTHING
    RETURNING id INTO v_id;
    IF v_id IS NULL THEN
        SELECT j.id INTO v_id FROM queue.jobs j WHERE j.idempotency_key = p_idempotency_key;
    END IF;
    RETURN v_id;
END
$$;

-- Leases up to p_limit ready jobs: queued ones that are due, plus running ones whose lease
-- expired (the worker crashed or hung). Each claim gets a fresh lease_token; only the holder
-- of the current token can heartbeat, complete or fail the job (fencing). A job whose lease
-- expired on its final attempt is dead instead of being retried.
CREATE FUNCTION queue.claim(
    p_owner text,
    p_queues text[],
    p_limit integer,
    p_lease_seconds integer DEFAULT 60
) RETURNS SETOF queue.jobs
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
BEGIN
    -- SKIP LOCKED: a row another claimer holds must not stall this claim.
    UPDATE queue.jobs j
    SET status = 'dead', last_error = 'lease expired on the final attempt',
        lease_owner = NULL, lease_token = NULL, locked_until = NULL, finished_at = now()
    FROM (
        SELECT d.id FROM queue.jobs d
        WHERE d.status = 'running' AND d.locked_until < now() AND d.attempts >= d.max_attempts
          AND d.queue = ANY (p_queues)
        LIMIT 100
        FOR UPDATE SKIP LOCKED
    ) expired
    WHERE j.id = expired.id;

    RETURN QUERY
    WITH picked AS (
        SELECT j.id
        FROM queue.jobs j
        WHERE j.queue = ANY (p_queues)
          AND ((j.status = 'queued' AND j.run_at <= now())
               OR (j.status = 'running' AND j.locked_until < now() AND j.attempts < j.max_attempts))
        ORDER BY j.run_at, j.id
        LIMIT p_limit
        FOR UPDATE SKIP LOCKED
    )
    UPDATE queue.jobs j
    SET status = 'running', attempts = j.attempts + 1, lease_owner = p_owner,
        lease_token = gen_random_uuid(), locked_until = now() + make_interval(secs => p_lease_seconds)
    FROM picked
    WHERE j.id = picked.id
    RETURNING j.*;
END
$$;

CREATE FUNCTION queue.heartbeat(p_id bigint, p_lease_token uuid, p_lease_seconds integer DEFAULT 60)
RETURNS boolean
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH hit AS (
        UPDATE queue.jobs
        SET locked_until = now() + make_interval(secs => p_lease_seconds)
        WHERE id = p_id AND lease_token = p_lease_token AND status = 'running'
        RETURNING 1
    )
    SELECT EXISTS (SELECT 1 FROM hit)
$$;

CREATE FUNCTION queue.complete(p_id bigint, p_lease_token uuid) RETURNS boolean
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH hit AS (
        UPDATE queue.jobs
        SET status = 'done', lease_owner = NULL, lease_token = NULL, locked_until = NULL,
            last_error = NULL, finished_at = now()
        WHERE id = p_id AND lease_token = p_lease_token AND status = 'running'
        RETURNING 1
    )
    SELECT EXISTS (SELECT 1 FROM hit)
$$;

-- Records a failed attempt. Retries after p_retry_in_ms unless the attempts are used up or
-- p_retry_in_ms is NULL (permanent failure), in which case the job is dead.
-- Returns the new status, or NULL when the caller no longer holds the lease.
CREATE FUNCTION queue.fail(p_id bigint, p_lease_token uuid, p_error text, p_retry_in_ms bigint)
RETURNS text
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    UPDATE queue.jobs
    SET status = CASE WHEN p_retry_in_ms IS NULL OR attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
        run_at = CASE WHEN p_retry_in_ms IS NULL OR attempts >= max_attempts THEN run_at
                      ELSE now() + p_retry_in_ms * interval '1 millisecond' END,
        finished_at = CASE WHEN p_retry_in_ms IS NULL OR attempts >= max_attempts THEN now() END,
        last_error = left(p_error, 2000),
        lease_owner = NULL, lease_token = NULL, locked_until = NULL
    WHERE id = p_id AND lease_token = p_lease_token AND status = 'running'
    RETURNING status
$$;

-- Outbox dispatch: lock a batch of undispatched events (rows stay locked until the caller's
-- transaction ends), enqueue their fan-out jobs, then mark them, all in one transaction.
CREATE FUNCTION queue.claim_outbox(p_limit integer) RETURNS SETOF queue.outbox
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT * FROM queue.outbox
    WHERE dispatched_at IS NULL
    ORDER BY id
    LIMIT p_limit
    FOR UPDATE SKIP LOCKED
$$;

CREATE FUNCTION queue.mark_dispatched(p_ids bigint[]) RETURNS void
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    UPDATE queue.outbox SET dispatched_at = now() WHERE id = ANY (p_ids) AND dispatched_at IS NULL
$$;

-- Retention: finished jobs and dispatched events older than p_older_than. Dead jobs stay for
-- inspection.
CREATE FUNCTION queue.purge(p_older_than interval) RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH j AS (
        DELETE FROM queue.jobs WHERE status = 'done' AND finished_at < now() - p_older_than
        RETURNING 1
    ), o AS (
        DELETE FROM queue.outbox WHERE dispatched_at < now() - p_older_than
        RETURNING 1
    )
    SELECT (SELECT count(*) FROM j) + (SELECT count(*) FROM o)
$$;

REVOKE ALL ON ALL FUNCTIONS IN SCHEMA queue FROM PUBLIC;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA queue TO app_runtime;
