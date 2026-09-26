-- Media encoding is serial and CPU-heavy. Waiting for its semaphore must not hold all
-- default worker slots, starving mail, imports and invoicing. Route at the queue boundary
-- so API processes from the previous release also use the isolated queue during rollout.
CREATE OR REPLACE FUNCTION queue.enqueue(
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
    VALUES (p_tenant_id,
            CASE WHEN p_kind = 'media.process' AND p_queue = 'default' THEN 'media' ELSE p_queue END,
            p_kind, p_payload, coalesce(p_run_at, now()), p_max_attempts, p_idempotency_key)
    ON CONFLICT (idempotency_key) DO NOTHING
    RETURNING id INTO v_id;
    IF v_id IS NULL THEN
        SELECT j.id INTO v_id FROM queue.jobs j WHERE j.idempotency_key = p_idempotency_key;
    END IF;
    RETURN v_id;
END
$$;

-- Preserve leases, fencing tokens, due dates, attempt counts and idempotency keys. Existing
-- handlers can finish normally; new workers reclaim expired leases on the media queue.
UPDATE queue.jobs SET queue = 'media'
WHERE kind = 'media.process' AND queue = 'default' AND status IN ('queued', 'running', 'dead');
