-- WP18: email marketing (spec §7.6, §11.4, §11.5, A12, A14, A20): subscribers with double
-- opt-in evidence, segments, campaigns and their per-recipient sends; marketing mail headers;
-- tenant email branding (logo, subject/intro texts).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Capability
-- tokens (confirmation, unsubscribe/click) are stored only as SHA-256 hashes.

-- ---------------------------------------------------------------------------------------
-- Subscribers. One row per address and tenant; the status is the newsletter state, the
-- consent records (A20) are the evidence and are what sending checks.
CREATE TABLE subscribers (
    id                 uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id          uuid NOT NULL REFERENCES platform.tenants (id),
    email              text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    status             text NOT NULL DEFAULT 'pending'
                       CHECK (status IN ('pending', 'subscribed', 'unsubscribed', 'bounced', 'complained')),
    locale             text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    market_id          uuid NOT NULL,
    customer_id        uuid,
    -- Double opt-in: the pending confirmation (single use, expires).
    confirm_token_hash bytea CHECK (length(confirm_token_hash) = 32),
    confirm_expires_at timestamptz,
    -- Evidence (A20): when and from where (salted IP hash) it was requested and confirmed,
    -- and the consent text version shown.
    requested_at       timestamptz NOT NULL DEFAULT now(),
    request_ip_hash    bytea CHECK (length(request_ip_hash) = 32),
    confirmed_at       timestamptz,
    confirm_ip_hash    bytea CHECK (length(confirm_ip_hash) = 32),
    text_version       text NOT NULL CHECK (text_version ~ '^[A-Za-z0-9_-]{1,32}$'),
    source             text NOT NULL CHECK (source ~ '^[a-z_]{1,32}$'),
    unsubscribed_at    timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT subscribers_email_unique UNIQUE (tenant_id, email),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id)
        ON DELETE SET NULL (customer_id),
    CHECK ((confirm_token_hash IS NULL) = (confirm_expires_at IS NULL))
);
CREATE UNIQUE INDEX subscribers_confirm_token ON subscribers (tenant_id, confirm_token_hash)
    WHERE confirm_token_hash IS NOT NULL;
CREATE INDEX subscribers_status ON subscribers (tenant_id, status, id);
CREATE INDEX subscribers_customer ON subscribers (tenant_id, customer_id) WHERE customer_id IS NOT NULL;

-- Newsletter sign-ups join the customer rate-limit ledger (per hashed IP and hour, every
-- request counts, purged daily with the rest of it).
ALTER TABLE customer_auth_attempts
    DROP CONSTRAINT customer_auth_attempts_kind_check,
    ADD CONSTRAINT customer_auth_attempts_kind_check
        CHECK (kind IN ('magic_link', 'login_failed', 'newsletter'));

-- Segments (§11.5): an allowlisted rule set (validated by commerce::marketing::segments,
-- compiled to parameterized SQL at use; never stored as SQL).
CREATE TABLE segments (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    name       text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    rules      jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT segments_name_unique UNIQUE (tenant_id, name)
);

-- Campaigns. `content` = {"<locale>": {subject, preheader, blocks}}; `link_key` signs the
-- click-tracking links of this campaign (HMAC-SHA256), so a redirect is never open.
CREATE TABLE campaigns (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    segment_id   uuid,
    content      jsonb NOT NULL,
    status       text NOT NULL DEFAULT 'draft'
                 CHECK (status IN ('draft', 'scheduled', 'sending', 'sent', 'cancelled')),
    scheduled_at timestamptz,
    started_at   timestamptz,
    finished_at  timestamptz,
    link_key     bytea NOT NULL CHECK (length(link_key) = 32),
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, segment_id) REFERENCES segments (tenant_id, id),
    CHECK (status = 'draft' OR scheduled_at IS NOT NULL)
);

-- Email messages learn their subscriber (marketing re-checks at send time, A20) and the
-- one-click unsubscribe URL of marketing mail (RFC 8058).
ALTER TABLE email_messages
    ADD COLUMN subscriber_id uuid,
    ADD COLUMN list_unsubscribe text
        CHECK (list_unsubscribe ~ '^https?://[!-~]+$' AND length(list_unsubscribe) <= 2000),
    ADD CONSTRAINT email_messages_subscriber_fk FOREIGN KEY (tenant_id, subscriber_id)
        REFERENCES subscribers (tenant_id, id) ON DELETE SET NULL (subscriber_id);
CREATE INDEX email_messages_list ON email_messages (tenant_id, id DESC);

-- One row per (campaign, subscriber): the idempotency anchor of a send (A12). `sent` means
-- the message was enqueued (its delivery state lives in email_messages); `skipped` records
-- why a segment member got nothing (consent withdrawn, suppressed, no longer subscribed).
-- `token_hash` is the recipient's capability for the unsubscribe and click links.
CREATE TABLE campaign_sends (
    id              uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    campaign_id     uuid NOT NULL,
    subscriber_id   uuid NOT NULL,
    status          text NOT NULL CHECK (status IN ('sent', 'skipped')),
    skip_reason     text CHECK (skip_reason ~ '^[a-z_]{1,32}$'),
    message_id      uuid,
    token_hash      bytea CHECK (length(token_hash) = 32),
    -- Rendered with the customer's personal signals (A20: re-checked before SMTP).
    personalized    boolean NOT NULL DEFAULT false,
    clicked_at      timestamptz,
    click_count     integer NOT NULL DEFAULT 0,
    unsubscribed_at timestamptz,
    bounced_at      timestamptz,
    complained_at   timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT campaign_sends_once UNIQUE (tenant_id, campaign_id, subscriber_id),
    FOREIGN KEY (tenant_id, campaign_id) REFERENCES campaigns (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, subscriber_id) REFERENCES subscribers (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, message_id) REFERENCES email_messages (tenant_id, id) ON DELETE SET NULL (message_id),
    CHECK ((status = 'sent') = (token_hash IS NOT NULL))
);
CREATE UNIQUE INDEX campaign_sends_token ON campaign_sends (tenant_id, token_hash)
    WHERE token_hash IS NOT NULL;
CREATE INDEX campaign_sends_message ON campaign_sends (tenant_id, message_id);
CREATE INDEX campaign_sends_clicks ON campaign_sends (tenant_id, subscriber_id, clicked_at)
    WHERE clicked_at IS NOT NULL;

-- Tenant email settings: the logo in every email layout (§11.4) and the marketing send window
-- (per-tenant throttle: at most N marketing messages per minute).
CREATE TABLE email_settings (
    tenant_id             uuid PRIMARY KEY REFERENCES platform.tenants (id),
    logo_asset_id         uuid,
    throttle_window_start timestamptz,
    throttle_window_count integer NOT NULL DEFAULT 0 CHECK (throttle_window_count >= 0),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, logo_asset_id) REFERENCES assets (tenant_id, id)
        ON DELETE SET NULL (logo_asset_id)
);

-- Tenant-editable subject/intro of transactional templates per locale (§11.4); the layout and
-- everything else stay platform-owned. NULL = the platform default.
CREATE TABLE email_template_texts (
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    template   text NOT NULL CHECK (template ~ '^[a-z_]{1,64}$'),
    locale     text NOT NULL CHECK (locale ~ '^[a-z]{2}$'),
    subject    text CHECK (length(subject) BETWEEN 1 AND 200),
    intro      text CHECK (length(intro) BETWEEN 1 AND 1000),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, template, locale)
);

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['subscribers', 'segments', 'campaigns', 'campaign_sends',
        'email_settings', 'email_template_texts']
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

-- Bounce/complaint notifications (SES via SNS) arrive for the whole platform; the message id
-- in them (our Message-ID) names the tenant. Only the tenant id leaves the function.
CREATE FUNCTION platform.email_message_tenant(p_id uuid) RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT m.tenant_id FROM public.email_messages m WHERE m.id = p_id
$$;
REVOKE ALL ON FUNCTION platform.email_message_tenant(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.email_message_tenant(uuid) TO app_runtime;
