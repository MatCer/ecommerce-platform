-- WP9: customers (spec §5.4, §7.3, A4, A5), consent (A20), mail core (§11.4, §13, A14, A29).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Credentials
-- (sessions, magic links) are stored only as SHA-256 hashes of 256-bit tokens.

-- ---------------------------------------------------------------------------------------
-- IP hashing (§14: "IPs hashed with a rotating salt"). One random salt per UTC day; salts
-- older than two days are deleted, after which stored hashes can no longer be linked back to
-- an address. Platform table: it holds no tenant data and only hashes leave it.

CREATE TABLE platform.ip_salts (
    day  date PRIMARY KEY,
    salt bytea NOT NULL CHECK (length(salt) = 32)
);
GRANT SELECT, INSERT ON platform.ip_salts TO app_runtime;

-- ---------------------------------------------------------------------------------------
-- Customers (§5.4, §7.3). Emails are trimmed and lower-cased by commerce::customers (the
-- CHECK keeps it that way), so a plain unique constraint gives case-insensitive uniqueness.

-- B2B-ready (§7.3), unused in the v1 UI.
CREATE TABLE customer_groups (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    name       text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, name)
);

CREATE TABLE customers (
    id                  uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id           uuid NOT NULL REFERENCES platform.tenants (id),
    email               text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    name                text CHECK (length(name) <= 200),
    phone               text CHECK (length(phone) <= 40),
    -- argon2id PHC string (A5); NULL until the customer sets a password.
    password_hash       text CHECK (password_hash LIKE '$argon2id$%'),
    password_changed_at timestamptz,
    email_verified_at   timestamptz,
    locale              text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    group_id            uuid,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT customers_email_unique UNIQUE (tenant_id, email),
    FOREIGN KEY (tenant_id, group_id) REFERENCES customer_groups (tenant_id, id)
);

CREATE TABLE customer_addresses (
    id          uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id   uuid NOT NULL,
    customer_id uuid NOT NULL,
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    company     text CHECK (length(company) <= 200),
    street      text NOT NULL CHECK (length(street) BETWEEN 1 AND 200),
    city        text NOT NULL CHECK (length(city) BETWEEN 1 AND 100),
    postal_code text NOT NULL CHECK (length(postal_code) BETWEEN 1 AND 20),
    country     text NOT NULL CHECK (country ~ '^[A-Z]{2}$'),
    phone       text CHECK (length(phone) <= 40),
    is_default  boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX customer_addresses_customer ON customer_addresses (tenant_id, customer_id);
CREATE UNIQUE INDEX customer_addresses_one_default ON customer_addresses (tenant_id, customer_id)
    WHERE is_default;

-- `sid` sessions (§5.4): 30-day sliding expiry. `email_verified_at` is when this session last
-- proved control of the email address (a magic-link sign-in); A5 lets a password be set
-- without the current one only within 10 minutes of it.
CREATE TABLE customer_sessions (
    token_hash        bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id         uuid NOT NULL,
    customer_id       uuid NOT NULL,
    email_verified_at timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    last_seen_at      timestamptz NOT NULL DEFAULT now(),
    expires_at        timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX customer_sessions_customer ON customer_sessions (tenant_id, customer_id);
CREATE INDEX customer_sessions_expiry ON customer_sessions (expires_at);

-- Magic links (§5.4, A5): single use, 15 minutes, consumed atomically, bound to the market
-- (checkout host) they were requested on. The customer row is created on consumption, so
-- requests for unknown addresses leave no customer behind.
CREATE TABLE customer_magic_links (
    token_hash bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    market_id  uuid NOT NULL,
    email      text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) <= 254),
    locale     text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    -- A5: a relative path on the checkout origin.
    redirect   text NOT NULL CHECK (redirect ~ '^/' AND redirect !~ '^/[/\\]'
                                    AND redirect !~ '[[:space:][:cntrl:]\\]' AND length(redirect) <= 500),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id)
);
CREATE INDEX customer_magic_links_expiry ON customer_magic_links (expires_at);

-- Rate-limit ledger (§5.4): magic-link requests and failed password logins per email and per
-- hashed IP. Rows older than a day are purged.
CREATE TABLE customer_auth_attempts (
    id        bigint GENERATED ALWAYS AS IDENTITY,
    tenant_id uuid NOT NULL REFERENCES platform.tenants (id),
    kind      text NOT NULL CHECK (kind IN ('magic_link', 'login_failed')),
    email     text NOT NULL CHECK (length(email) <= 254),
    ip_hash   bytea CHECK (length(ip_hash) = 32),
    at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX customer_auth_attempts_email ON customer_auth_attempts (tenant_id, kind, email, at);
CREATE INDEX customer_auth_attempts_ip ON customer_auth_attempts (tenant_id, kind, ip_hash, at);
CREATE INDEX customer_auth_attempts_at ON customer_auth_attempts (at);

-- Carts gain their customer (A4: attached on login) and the `merged` end state (lines moved
-- into the cart the customer is checking out with).
ALTER TABLE carts
    ADD CONSTRAINT carts_customer_fk FOREIGN KEY (tenant_id, customer_id)
        REFERENCES customers (tenant_id, id) ON DELETE SET NULL (customer_id),
    DROP CONSTRAINT carts_status_check,
    ADD CONSTRAINT carts_status_check CHECK (status IN ('open', 'converted', 'abandoned', 'merged'));
CREATE INDEX carts_customer ON carts (tenant_id, customer_id) WHERE status = 'open';

-- ---------------------------------------------------------------------------------------
-- Consent (A20). Append-only: the latest record per (subject, purpose) wins. Subjects:
-- `anon` (the random id in the first-party consent cookie), `customer` (customer id),
-- `email` (normalized address, newsletter/checkout consents without an account).

CREATE TABLE consent_records (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    subject_type text NOT NULL CHECK (subject_type IN ('anon', 'customer', 'email')),
    subject_id   text NOT NULL CHECK (length(subject_id) BETWEEN 1 AND 254),
    purpose      text NOT NULL CHECK (purpose IN ('analytics', 'ads', 'personalization',
                                                  'email_marketing', 'review_invites')),
    granted      boolean NOT NULL,
    text_version text NOT NULL CHECK (text_version ~ '^[A-Za-z0-9_-]{1,32}$'),
    source       text NOT NULL CHECK (source ~ '^[a-z_]{1,32}$'),
    ip_hash      bytea CHECK (length(ip_hash) = 32),
    at           timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);
CREATE INDEX consent_records_latest
    ON consent_records (tenant_id, subject_type, subject_id, purpose, at DESC, id DESC);

-- ---------------------------------------------------------------------------------------
-- Email (§11.4, §13, A14, A29). One row per message; the `mail.send` job moves it through
-- pending → sending → accepted | uncertain | failed. `accepted` only after the SMTP 250.

CREATE TABLE email_messages (
    id              uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    stream          text NOT NULL CHECK (stream IN ('transactional', 'marketing')),
    template        text NOT NULL CHECK (template ~ '^[a-z_]{1,64}$'),
    -- What makes a send unique (`magic_link:<hash>`): enqueueing it twice is a no-op.
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    to_email        text NOT NULL CHECK (length(to_email) BETWEEN 3 AND 254),
    locale          text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    subject         text NOT NULL CHECK (length(subject) <= 500),
    -- Cleared once a `sensitive` message (sign-in links) reaches a final state.
    html            text,
    body_text       text,
    sensitive       boolean NOT NULL DEFAULT false,
    status          text NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'sending', 'accepted', 'uncertain', 'failed')),
    -- SMTP attempts started, and how often a send ended without knowing the outcome.
    attempts        integer NOT NULL DEFAULT 0,
    uncertain_count smallint NOT NULL DEFAULT 0,
    last_error      text CHECK (length(last_error) <= 1000),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    accepted_at     timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT email_messages_idempotency UNIQUE (tenant_id, idempotency_key)
);
CREATE INDEX email_messages_status ON email_messages (tenant_id, status, created_at);

-- Checked before every send (§11.4, A29). `complaint` suppresses marketing only; `bounce`
-- and `manual` suppress every stream.
CREATE TABLE email_suppressions (
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    email      text NOT NULL CHECK (email = lower(btrim(email)) AND length(email) <= 254),
    reason     text NOT NULL CHECK (reason IN ('bounce', 'complaint', 'manual')),
    note       text CHECK (length(note) <= 500),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, email)
);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['customer_groups', 'customers', 'customer_addresses',
        'customer_sessions', 'customer_magic_links', 'customer_auth_attempts', 'consent_records',
        'email_messages', 'email_suppressions']
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

GRANT SELECT, INSERT, UPDATE, DELETE ON customer_groups, customers, customer_addresses,
    customer_sessions, customer_magic_links, customer_auth_attempts, email_messages,
    email_suppressions TO app_runtime;
-- Consent records are evidence: the application may only add them (A20).
GRANT SELECT, INSERT ON consent_records TO app_runtime;

-- Retention across tenants (hourly `maintenance.cleanup`): expired sessions and magic links,
-- day-old rate-limit rows, IP salts older than two days (§14: sessions 30 days).
CREATE POLICY retention_read ON customer_sessions FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON customer_sessions FOR DELETE TO app_owner
    USING (expires_at < now());
CREATE POLICY retention_read ON customer_magic_links FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON customer_magic_links FOR DELETE TO app_owner
    USING (expires_at < now() - interval '1 day');
CREATE POLICY retention_read ON customer_auth_attempts FOR SELECT TO app_owner USING (true);
CREATE POLICY retention_purge ON customer_auth_attempts FOR DELETE TO app_owner
    USING (at < now() - interval '1 day');

CREATE FUNCTION platform.purge_customer_auth() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH sessions AS (
        DELETE FROM public.customer_sessions WHERE expires_at < now() RETURNING 1
    ), links AS (
        DELETE FROM public.customer_magic_links WHERE expires_at < now() - interval '1 day'
        RETURNING 1
    ), attempts AS (
        DELETE FROM public.customer_auth_attempts WHERE at < now() - interval '1 day' RETURNING 1
    ), salts AS (
        DELETE FROM platform.ip_salts WHERE day < (now() AT TIME ZONE 'utc')::date - 1 RETURNING 1
    )
    SELECT (SELECT count(*) FROM sessions) + (SELECT count(*) FROM links)
         + (SELECT count(*) FROM attempts) + (SELECT count(*) FROM salts)
$$;
REVOKE ALL ON FUNCTION platform.purge_customer_auth() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.purge_customer_auth() TO app_runtime;
