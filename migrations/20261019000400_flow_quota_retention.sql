-- Expired mail budgets no longer affect delivery and should not retain recipient addresses.
CREATE POLICY flow_watch_mail_quota_retention_read ON flow_watch_mail_quotas
    FOR SELECT TO app_owner USING (window_started_at < now() - interval '2 days');
CREATE POLICY flow_watch_mail_quota_retention_delete ON flow_watch_mail_quotas
    FOR DELETE TO app_owner USING (window_started_at < now() - interval '2 days');

CREATE FUNCTION platform.purge_flow_watch_mail_quotas() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
    WITH deleted AS (
        DELETE FROM public.flow_watch_mail_quotas
        WHERE window_started_at < now() - interval '2 days' RETURNING 1
    ) SELECT count(*) FROM deleted
$$;
REVOKE ALL ON FUNCTION platform.purge_flow_watch_mail_quotas() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.purge_flow_watch_mail_quotas() TO app_runtime;
