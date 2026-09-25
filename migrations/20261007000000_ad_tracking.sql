-- WP20: ad-platform forwarders (spec §11.3, §14, A20, A21).

-- One row per tenant and platform. Credentials (access tokens, API secrets, OAuth client +
-- refresh token) are sealed with `platform::crypto::SecretBox` (AES-256-GCM, AAD
-- `adplatform:<tenant>:<platform>`) and never returned; `credentials_hint` tells them apart.
-- `settings` holds the non-secret ids (pixel, measurement, account, conversion action, SEM).
CREATE TABLE ad_platforms (
    tenant_id              uuid NOT NULL REFERENCES platform.tenants (id),
    platform               text NOT NULL CHECK (platform IN ('meta', 'ga4', 'google_ads', 'sklik')),
    enabled                boolean NOT NULL DEFAULT false,
    paused                 boolean NOT NULL DEFAULT false,
    test_mode              boolean NOT NULL DEFAULT false,
    market_ids             uuid[] NOT NULL DEFAULT '{}' CHECK (cardinality(market_ids) <= 50),
    settings               jsonb NOT NULL DEFAULT '{}',
    credentials_ciphertext bytea,
    credentials_hint       text CHECK (length(credentials_hint) <= 8),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, platform)
);

-- One forwarding of one event to one platform. Minimized on purpose: the consent subject
-- (pseudonymous, needed to re-check `ads` at send time), the customer (so a refusal on the
-- account also stops it), the order, allowlisted props (product/variant ids, quantity, page
-- path) and the user agent (Meta requires it for website events; cleared once finished).
-- Email and phone are read from the order and hashed only when sending: no raw PII here.
-- `event_id` is shared by the platforms of one event and sent as the vendor's dedupe key.
CREATE TABLE ad_deliveries (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL,
    platform      text NOT NULL,
    event_id      uuid NOT NULL,
    event_name    text NOT NULL CHECK (event_name IN ('page_view', 'view_item', 'add_to_cart',
                                                      'begin_checkout', 'purchase', 'refund')),
    market_id     uuid NOT NULL,
    subject       text NOT NULL CHECK (subject ~ '^[0-9a-f]{32}$'),
    customer_id   uuid,
    order_id      uuid,
    props         jsonb NOT NULL DEFAULT '{}',
    user_agent    text CHECK (length(user_agent) <= 512),
    occurred_at   timestamptz NOT NULL,
    status        text NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'retrying', 'paused', 'succeeded', 'dead',
                                    'cancelled', 'skipped')),
    attempts      integer NOT NULL DEFAULT 0,
    response_code integer,
    -- Our own short description (never the vendor's body, which may echo input).
    last_error    text CHECK (length(last_error) <= 300),
    finished_at   timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, platform, event_id),
    FOREIGN KEY (tenant_id, platform) REFERENCES ad_platforms (tenant_id, platform) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX ad_deliveries_log ON ad_deliveries (tenant_id, id DESC);
CREATE INDEX ad_deliveries_open_subject ON ad_deliveries (tenant_id, subject)
    WHERE status IN ('pending', 'retrying', 'paused');
CREATE INDEX ad_deliveries_open_customer ON ad_deliveries (tenant_id, customer_id)
    WHERE customer_id IS NOT NULL AND status IN ('pending', 'retrying', 'paused');
CREATE INDEX ad_deliveries_order ON ad_deliveries (tenant_id, order_id) WHERE order_id IS NOT NULL;
CREATE INDEX ad_deliveries_finished ON ad_deliveries (finished_at) WHERE finished_at IS NOT NULL;

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['ad_platforms', 'ad_deliveries']
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

-- Retention (hourly cleanup): finished deliveries are kept 90 days for the log; a finished
-- delivery's user agent is not needed any more. The owner may touch only finished rows
-- (same pattern as platform.purge_search_zero_results).
CREATE POLICY retention_read ON ad_deliveries FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_scrub ON ad_deliveries FOR UPDATE TO app_owner
    USING (finished_at IS NOT NULL) WITH CHECK (finished_at IS NOT NULL AND user_agent IS NULL);
CREATE POLICY retention_purge ON ad_deliveries FOR DELETE TO app_owner
    USING (finished_at < now() - interval '90 days');

CREATE FUNCTION platform.purge_ad_deliveries() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH ua AS (
        UPDATE public.ad_deliveries SET user_agent = NULL
        WHERE finished_at IS NOT NULL AND user_agent IS NOT NULL
        RETURNING 1
    ), d AS (
        DELETE FROM public.ad_deliveries WHERE finished_at < now() - interval '90 days'
        RETURNING 1
    )
    SELECT (SELECT count(*) FROM d)
$$;
REVOKE ALL ON FUNCTION platform.purge_ad_deliveries() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.purge_ad_deliveries() TO app_runtime;
