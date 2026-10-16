SET LOCAL lock_timeout = '5s';

CREATE TABLE platform.hostname_claims (
    hostname        text PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    platform_issued boolean NOT NULL DEFAULT false,
    claimed_at      timestamptz DEFAULT now()
);

CREATE TABLE platform.account_domains (
    domain     text PRIMARY KEY
               CHECK (domain ~ '^[a-z0-9-]+(\.[a-z0-9-]+)+$' AND length(domain) <= 253),
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE platform.domains
    ADD CONSTRAINT domains_hostname_tenant_market UNIQUE (hostname, tenant_id, market_id);

CREATE TABLE platform.checkout_origins (
    id                  uuid PRIMARY KEY DEFAULT platform.uuid_v7(),
    scheme              text NOT NULL CHECK (scheme IN ('https', 'http')),
    hostname            text NOT NULL,
    port                integer NOT NULL CHECK (port BETWEEN 1 AND 65535),
    -- Serialized as browsers do (default port omitted): the edge matches the request on it.
    origin              text NOT NULL GENERATED ALWAYS AS (
                            scheme || '://' || hostname ||
                            CASE WHEN (scheme, port) IN (('https', 443), ('http', 80)) THEN ''
                                 ELSE ':' || port::text END
                        ) STORED UNIQUE,
    tenant_id           uuid NOT NULL,
    market_id           uuid NOT NULL,
    mode                text NOT NULL DEFAULT 'isolated' CHECK (mode IN ('isolated', 'same_origin')),
    mode_generation     bigint NOT NULL DEFAULT 1 CHECK (mode_generation >= 1),
    history             text NOT NULL DEFAULT 'unknown_or_used'
                        CHECK (history IN ('unknown_or_used', 'platform_never_reassigned',
                                           'merchant_attested_never_served')),
    attested_by         text CHECK (length(attested_by) BETWEEN 1 AND 255),
    attested_at         timestamptz,
    attestation_version integer CHECK (attestation_version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (hostname, tenant_id, market_id)
        REFERENCES platform.domains (hostname, tenant_id, market_id),
    CONSTRAINT checkout_origins_https_outside_localhost
        CHECK (scheme = 'https' OR hostname LIKE '%.localhost'),
    CONSTRAINT checkout_origins_same_origin_needs_known_history
        CHECK (mode = 'isolated' OR history <> 'unknown_or_used'),
    CONSTRAINT checkout_origins_attestation_complete
        CHECK (num_nulls(attested_by, attested_at, attestation_version)
               = CASE WHEN history = 'merchant_attested_never_served' THEN 0 ELSE 3 END)
);
CREATE INDEX checkout_origins_domain ON platform.checkout_origins (hostname, tenant_id, market_id);

INSERT INTO platform.hostname_claims (hostname, tenant_id, claimed_at)
SELECT hostname, tenant_id, NULL FROM platform.domains;
INSERT INTO platform.checkout_origins (scheme, hostname, port, tenant_id, market_id)
SELECT 'https', hostname, 443, tenant_id, market_id FROM platform.domains;

CREATE FUNCTION platform.domain_inserted() RETURNS trigger
LANGUAGE plpgsql
SET search_path = ''
AS $$
BEGIN
    PERFORM pg_advisory_xact_lock('platform.account_domains'::regclass::oid::bigint);
    IF EXISTS (SELECT 1 FROM platform.account_domains a
               WHERE NEW.hostname = a.domain OR NEW.hostname LIKE '%.' || a.domain) THEN
        RAISE EXCEPTION 'hostname % is under an account domain', NEW.hostname
            USING ERRCODE = 'check_violation', CONSTRAINT = 'account_domain';
    END IF;
    INSERT INTO platform.hostname_claims (hostname, tenant_id) VALUES (NEW.hostname, NEW.tenant_id)
    ON CONFLICT (hostname) DO NOTHING;
    IF (SELECT tenant_id FROM platform.hostname_claims WHERE hostname = NEW.hostname)
       <> NEW.tenant_id THEN
        RAISE EXCEPTION 'hostname % belonged to another tenant', NEW.hostname
            USING ERRCODE = 'unique_violation', CONSTRAINT = 'hostname_claims_pkey';
    END IF;
    INSERT INTO platform.checkout_origins (scheme, hostname, port, tenant_id, market_id)
    VALUES ('https', NEW.hostname, 443, NEW.tenant_id, NEW.market_id);
    RETURN NULL;
END
$$;
CREATE TRIGGER domain_inserted AFTER INSERT ON platform.domains
    FOR EACH ROW EXECUTE FUNCTION platform.domain_inserted();

CREATE FUNCTION platform.account_domain_inserted() RETURNS trigger
LANGUAGE plpgsql
SET search_path = ''
AS $$
BEGIN
    PERFORM pg_advisory_xact_lock('platform.account_domains'::regclass::oid::bigint);
    IF EXISTS (SELECT 1 FROM platform.hostname_claims c
               WHERE c.hostname = NEW.domain OR c.hostname LIKE '%.' || NEW.domain) THEN
        RAISE EXCEPTION 'account domain % covers a shop hostname', NEW.domain
            USING ERRCODE = 'check_violation', CONSTRAINT = 'account_domain';
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER account_domain_inserted AFTER INSERT ON platform.account_domains
    FOR EACH ROW EXECUTE FUNCTION platform.account_domain_inserted();

CREATE FUNCTION platform.require_complete_platform_claim() RETURNS trigger
LANGUAGE plpgsql
SET search_path = ''
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM platform.hostname_claims c
        WHERE c.hostname = NEW.hostname AND c.tenant_id = NEW.tenant_id
          AND c.platform_issued AND c.claimed_at IS NOT NULL
    ) THEN
        RAISE EXCEPTION 'hostname % has no complete platform history', NEW.hostname
            USING ERRCODE = 'check_violation', CONSTRAINT = 'checkout_origins_platform_history';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER platform_history_requires_complete_claim
    BEFORE INSERT OR UPDATE ON platform.checkout_origins
    FOR EACH ROW WHEN (NEW.history = 'platform_never_reassigned')
    EXECUTE FUNCTION platform.require_complete_platform_claim();

CREATE FUNCTION platform.checkout_origin_mode_changed() RETURNS trigger
LANGUAGE plpgsql
SET search_path = ''
AS $$
BEGIN
    NEW.mode_generation := OLD.mode_generation + 1;
    RETURN NEW;
END
$$;
CREATE TRIGGER checkout_origin_mode_changed BEFORE UPDATE OF mode ON platform.checkout_origins
    FOR EACH ROW WHEN (OLD.mode IS DISTINCT FROM NEW.mode)
    EXECUTE FUNCTION platform.checkout_origin_mode_changed();

REVOKE UPDATE, DELETE ON platform.hostname_claims FROM app_runtime;
REVOKE UPDATE, DELETE ON platform.account_domains FROM app_runtime;
REVOKE INSERT, UPDATE, DELETE ON platform.checkout_origins FROM app_runtime;
GRANT INSERT (scheme, hostname, port, tenant_id, market_id, mode, history,
              attested_by, attested_at, attestation_version)
    ON platform.checkout_origins TO app_runtime;
GRANT UPDATE (mode, history, attested_by, attested_at, attestation_version)
    ON platform.checkout_origins TO app_runtime;
