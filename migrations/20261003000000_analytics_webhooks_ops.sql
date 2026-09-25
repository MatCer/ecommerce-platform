-- WP14: analytics (spec §7.6, §11.3, A20), outbound webhooks (§8.5, A21), operations (§13, §15).

-- ---------------------------------------------------------------------------------------
-- Analytics.

-- A20: before consent only minimized server counters exist: page requests per market, route
-- template and UTC day, counted by the edge. No identifiers, no device storage.
CREATE TABLE analytics_counters (
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    market_id uuid NOT NULL,
    day       date NOT NULL,
    template  text NOT NULL CHECK (template IN ('home', 'category', 'product', 'search', 'page',
                                                'blog', 'checkout', 'other')),
    requests  bigint NOT NULL CHECK (requests >= 0),
    PRIMARY KEY (tenant_id, market_id, day, template),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

-- Search queries per day, counted by the edge (every request, edge-cached pages included) and
-- minimized like `search_zero_results` (product-like text only, folded, 90 days).
CREATE TABLE search_query_counts (
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    day       date NOT NULL,
    locale    text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    query     text NOT NULL CHECK (length(query) BETWEEN 1 AND 200),
    count     integer NOT NULL CHECK (count > 0),
    PRIMARY KEY (tenant_id, day, locale, query)
);

-- Consented browser events and authoritative server events (purchase, refund). Monthly
-- partitions (`events_YYYY_MM`), created ahead and dropped after 13 months by the nightly
-- maintenance job. RLS and grants live on the parent only: app_runtime cannot reach a
-- partition directly. `anon_id` is derived from the consent subject (never the subject
-- itself); `session_id` groups one anonymous visitor's events with gaps under 30 minutes.
CREATE TABLE events (
    id               uuid NOT NULL,
    tenant_id        uuid NOT NULL REFERENCES platform.tenants (id),
    at               timestamptz NOT NULL,
    type             text NOT NULL CHECK (type IN ('page_view', 'view_item', 'add_to_cart',
                                                   'begin_checkout', 'web_vitals', 'purchase',
                                                   'refund')),
    anon_id          text CHECK (anon_id ~ '^[0-9a-f]{32}$'),
    customer_id      uuid,
    session_id       uuid,
    market_id        uuid,
    props            jsonb NOT NULL DEFAULT '{}',
    consent_purposes text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (tenant_id, at, id)
) PARTITION BY RANGE (at);
CREATE INDEX events_anon ON events (tenant_id, anon_id, at DESC) WHERE anon_id IS NOT NULL;
CREATE INDEX events_type ON events (tenant_id, type, at);

-- Rollups (hourly for today and yesterday). `dims` narrows a metric (template, product, ...).
CREATE TABLE daily_metrics (
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    date      date NOT NULL,
    market_id uuid NOT NULL,
    metric    text NOT NULL CHECK (length(metric) <= 64),
    dims      jsonb NOT NULL DEFAULT '{}',
    value     double precision NOT NULL,
    PRIMARY KEY (tenant_id, date, market_id, metric, dims),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

-- Creates the monthly partitions from last month to `p_months_ahead` months ahead (UTC).
-- Each partition also gets forced RLS and the isolation policy (defense in depth: app_runtime
-- has no grants on partitions and reaches rows only through the parent).
CREATE FUNCTION platform.ensure_event_partitions(p_months_ahead integer DEFAULT 2) RETURNS integer
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
DECLARE
    m       timestamp;
    part    text;
    created integer := 0;
BEGIN
    FOR i IN -1..p_months_ahead LOOP
        m := date_trunc('month', now() AT TIME ZONE 'UTC') + make_interval(months => i);
        part := format('events_%s', to_char(m, 'YYYY_MM'));
        IF to_regclass('public.' || part) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE public.%I PARTITION OF public.events FOR VALUES FROM (%L) TO (%L)',
                part, m AT TIME ZONE 'UTC', (m + interval '1 month') AT TIME ZONE 'UTC');
            EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', part);
            EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', part);
            EXECUTE format(
                'CREATE POLICY tenant_isolation ON public.%I TO app_runtime
                     USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                     WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', part);
            created := created + 1;
        END IF;
    END LOOP;
    RETURN created;
END
$$;

-- Retention (§14): drops the partitions whose whole month is older than `p_keep_months`.
CREATE FUNCTION platform.drop_event_partitions(p_keep_months integer DEFAULT 13) RETURNS integer
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
DECLARE
    r       record;
    dropped integer := 0;
BEGIN
    FOR r IN
        SELECT c.relname
        FROM pg_catalog.pg_inherits i
        JOIN pg_catalog.pg_class c ON c.oid = i.inhrelid
        WHERE i.inhparent = 'public.events'::regclass AND c.relname ~ '^events_\d{4}_\d{2}$'
    LOOP
        IF (to_date(substr(r.relname, 8), 'YYYY_MM') + interval '1 month')
           <= (now() AT TIME ZONE 'UTC') - make_interval(months => p_keep_months) THEN
            EXECUTE format('DROP TABLE public.%I', r.relname);
            dropped := dropped + 1;
        END IF;
    END LOOP;
    RETURN dropped;
END
$$;

SELECT platform.ensure_event_partitions();

-- 90-day retention of the search query log, like the zero-result log (A20).
CREATE POLICY retention_read ON search_query_counts FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON search_query_counts FOR DELETE TO app_owner
    USING (day < current_date - 90);
CREATE FUNCTION platform.purge_search_query_counts() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH gone AS (
        DELETE FROM public.search_query_counts WHERE day < current_date - 90 RETURNING 1
    )
    SELECT count(*) FROM gone
$$;

-- ---------------------------------------------------------------------------------------
-- Outbound webhooks (§8.5, A21). The signing secret is encrypted at rest
-- (`platform::crypto::SecretBox`, AES-256-GCM, nonce || ciphertext) and returned only when
-- created or rotated.

CREATE TABLE webhook_subscriptions (
    id                uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id         uuid NOT NULL REFERENCES platform.tenants (id),
    url               text NOT NULL CHECK (length(url) BETWEEN 10 AND 2048),
    events            text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 20),
    description       text NOT NULL DEFAULT '' CHECK (length(description) <= 200),
    secret_ciphertext bytea NOT NULL,
    -- The last characters of the secret, so staff can tell secrets apart.
    secret_hint       text NOT NULL CHECK (length(secret_hint) <= 8),
    active            boolean NOT NULL DEFAULT true,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);

-- One delivery per subscription and outbox event. `window_started_at` starts the 24 h retry
-- window (reset by a redelivery); `attempts` doubles as the fencing counter: an attempt is
-- recorded only when the stored count is the one before it.
CREATE TABLE webhook_deliveries (
    id                uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id         uuid NOT NULL,
    subscription_id   uuid NOT NULL,
    event_id          bigint NOT NULL,
    event_type        text NOT NULL,
    payload           jsonb NOT NULL,
    status            text NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending', 'retrying', 'succeeded', 'dead')),
    attempts          integer NOT NULL DEFAULT 0,
    response_code     integer,
    last_error        text CHECK (length(last_error) <= 500),
    next_at           timestamptz,
    window_started_at timestamptz NOT NULL DEFAULT now(),
    delivered_at      timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, subscription_id, event_id),
    FOREIGN KEY (tenant_id, subscription_id) REFERENCES webhook_subscriptions (tenant_id, id)
        ON DELETE CASCADE
);
CREATE INDEX webhook_deliveries_log ON webhook_deliveries (tenant_id, created_at DESC, id DESC);

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['analytics_counters', 'search_query_counts', 'events',
                             'daily_metrics', 'webhook_subscriptions', 'webhook_deliveries']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I TO app_runtime
                 USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                 WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_runtime', t);
    END LOOP;
END
$$;

-- ---------------------------------------------------------------------------------------
-- Operations.

-- Abandoned-upload sweeper: the `uploads/` object of a completed asset is removed once its
-- presigned URL expired (a reused URL could otherwise leave garbage behind).
ALTER TABLE assets ADD COLUMN upload_purged_at timestamptz;

-- Queue depth and lag for `/metrics` (finished jobs are not counted).
CREATE FUNCTION queue.stats()
RETURNS TABLE (kind text, status text, jobs bigint, oldest_due_seconds double precision)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT j.kind, j.status, count(*),
           extract(epoch FROM now() - min(j.run_at) FILTER (
               WHERE j.status = 'queued' AND j.run_at <= now()))::double precision
    FROM queue.jobs j
    WHERE j.status <> 'done'
    GROUP BY j.kind, j.status
$$;

-- Age of the oldest undispatched outbox event (0 when none).
CREATE FUNCTION queue.outbox_lag_seconds() RETURNS double precision
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT coalesce(extract(epoch FROM now() - min(created_at)), 0)::double precision
    FROM queue.outbox WHERE dispatched_at IS NULL
$$;

-- Superadmin job list (newest first), optionally by status and kind.
CREATE FUNCTION queue.list_jobs(p_status text, p_kind text, p_before bigint, p_limit integer)
RETURNS SETOF queue.jobs
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT * FROM queue.jobs j
    WHERE (p_status IS NULL OR j.status = p_status)
      AND (p_kind IS NULL OR j.kind = p_kind)
      AND (p_before IS NULL OR j.id < p_before)
    ORDER BY j.id DESC
    LIMIT least(greatest(p_limit, 1), 200)
$$;

-- Puts a dead job back in the queue with fresh attempts; false when it is not dead.
CREATE FUNCTION queue.requeue(p_id bigint) RETURNS boolean
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH hit AS (
        UPDATE queue.jobs
        SET status = 'queued', attempts = 0, run_at = now(), finished_at = NULL
        WHERE id = p_id AND status = 'dead'
        RETURNING 1
    )
    SELECT EXISTS (SELECT 1 FROM hit)
$$;

-- Retention: finished jobs and dispatched events after p_older_than; dead jobs after 30 days
-- (kept that long for inspection and requeue).
CREATE OR REPLACE FUNCTION queue.purge(p_older_than interval) RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH j AS (
        DELETE FROM queue.jobs WHERE status = 'done' AND finished_at < now() - p_older_than
        RETURNING 1
    ), d AS (
        DELETE FROM queue.jobs WHERE status = 'dead' AND finished_at < now() - interval '30 days'
        RETURNING 1
    ), o AS (
        DELETE FROM queue.outbox WHERE dispatched_at < now() - p_older_than
        RETURNING 1
    )
    SELECT (SELECT count(*) FROM j) + (SELECT count(*) FROM d) + (SELECT count(*) FROM o)
$$;

REVOKE ALL ON FUNCTION queue.stats(), queue.outbox_lag_seconds(),
    queue.list_jobs(text, text, bigint, integer), queue.requeue(bigint),
    platform.ensure_event_partitions(integer), platform.drop_event_partitions(integer),
    platform.purge_search_query_counts() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION queue.stats(), queue.outbox_lag_seconds(),
    queue.list_jobs(text, text, bigint, integer), queue.requeue(bigint),
    platform.ensure_event_partitions(integer), platform.drop_event_partitions(integer),
    platform.purge_search_query_counts() TO app_runtime;
