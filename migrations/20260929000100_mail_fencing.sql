-- WP9 (A14): fenced send outcomes and reconciliation of stalled deliveries.
--
-- `send_token` is set with `sending`; only the attempt holding it may record the SMTP outcome,
-- so a worker that lost its lease cannot overwrite a newer attempt.
ALTER TABLE email_messages ADD COLUMN send_token uuid;
ALTER TABLE email_messages ADD CONSTRAINT email_messages_send_token
    CHECK ((status = 'sending') = (send_token IS NOT NULL));

CREATE INDEX email_messages_stalled ON email_messages (updated_at)
    WHERE status IN ('pending', 'sending', 'uncertain');

-- Messages whose delivery stalled (the job died, or the worker died mid-send), across tenants,
-- for the hourly reconciliation. Only ids leave the function; messages older than a week are
-- left alone (their content is stale by then).
CREATE POLICY reconcile_read ON email_messages FOR SELECT TO app_owner USING (true);

CREATE FUNCTION platform.stalled_email_messages(p_limit integer)
RETURNS TABLE (tenant_id uuid, id uuid, stalled_since timestamptz)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT m.tenant_id, m.id, m.updated_at
    FROM public.email_messages m
    WHERE (m.status IN ('pending', 'sending')
           OR (m.status = 'uncertain' AND m.stream = 'transactional' AND m.uncertain_count = 1))
      AND m.updated_at < now() - interval '30 minutes'
      AND m.created_at > now() - interval '7 days'
    ORDER BY m.updated_at
    LIMIT least(greatest(p_limit, 1), 1000)
$$;
REVOKE ALL ON FUNCTION platform.stalled_email_messages(integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.stalled_email_messages(integer) TO app_runtime;
