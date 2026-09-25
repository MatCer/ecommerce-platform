-- WP6: storefront runtime (spec §5.5, §7.3, §7.5, §8.2, §9.7, §10.3, A1, A4, A22, A30).
--
-- Tenant tables follow the WP1/WP3 rules (tenant_id, RLS + FORCE, composite FKs). Platform
-- tables (storefront tokens, theme artifacts, channels) have no RLS and are only reached
-- through explicit platform services.

-- ---------------------------------------------------------------------------------------
-- Storefront tokens (§5.5). Public by design: the edge injects the token on every storefront
-- call and `GET /internal/v1/resolve` returns it, so it is stored as issued. It only grants
-- public reads and cart operations of its tenant. Rotation keeps the previous token valid
-- until `expires_at` so edge caches can catch up.

CREATE TABLE platform.storefront_tokens (
    token      text PRIMARY KEY CHECK (token ~ '^sf_[0-9a-f]{64}$'),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- NULL: the current token. Set on rotation.
    expires_at timestamptz
);
CREATE INDEX storefront_tokens_tenant ON platform.storefront_tokens (tenant_id);
CREATE UNIQUE INDEX storefront_tokens_one_current ON platform.storefront_tokens (tenant_id)
    WHERE expires_at IS NULL;
GRANT SELECT, INSERT, UPDATE ON platform.storefront_tokens TO app_runtime;

-- Every existing tenant gets a token (new tenants get one in commerce::tenancy).
INSERT INTO platform.storefront_tokens (token, tenant_id)
SELECT 'sf_' || replace(gen_random_uuid()::text, '-', '') || replace(gen_random_uuid()::text, '-', ''), id
FROM platform.tenants;

-- ---------------------------------------------------------------------------------------
-- Redirects (§7.5, §9.5). Relative targets only: a redirect can never leave the shop.

CREATE TABLE redirects (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    from_path  text NOT NULL CHECK (from_path ~ '^/[^?#[:space:]]*$' AND length(from_path) <= 1000),
    -- A path on the same shop: no scheme, no `//host`, no backslashes (browsers read `/\` as `//`).
    to_path    text NOT NULL CHECK (to_path ~ '^/' AND to_path !~ '^/[/\\]' AND to_path !~ '[[:space:]\\]'
                                    AND length(to_path) <= 1000),
    code       smallint NOT NULL DEFAULT 301 CHECK (code IN (301, 302)),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT redirects_from_unique UNIQUE (tenant_id, from_path),
    CHECK (from_path <> to_path)
);

-- ---------------------------------------------------------------------------------------
-- Carts (§7.3, §10.3, A1, A4, A12). The capability tokens are stored as SHA-256 only. The
-- shop capability dies at the checkout handoff; the checkout capability is minted when the
-- handoff is redeemed. `version` changes with every mutation (place-order checks it, A12).

CREATE TABLE carts (
    id                  uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id           uuid NOT NULL REFERENCES platform.tenants (id),
    market_id           uuid NOT NULL,
    shop_token_hash     bytea CHECK (length(shop_token_hash) = 32),
    checkout_token_hash bytea CHECK (length(checkout_token_hash) = 32),
    -- Customers arrive in WP9; the FK is added there.
    customer_id         uuid,
    email               text CHECK (length(email) <= 320),
    locale              text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    currency            text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- Set by checkout (WP10); until then VAT uses the market's first country (A3).
    ship_to_country     text CHECK (ship_to_country ~ '^[A-Z]{2}$'),
    status              text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'converted', 'abandoned')),
    version             integer NOT NULL DEFAULT 1,
    last_activity_at    timestamptz NOT NULL DEFAULT now(),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id)
);
CREATE UNIQUE INDEX carts_shop_token ON carts (tenant_id, shop_token_hash);
CREATE UNIQUE INDEX carts_checkout_token ON carts (tenant_id, checkout_token_hash);
CREATE INDEX carts_activity ON carts (tenant_id, last_activity_at) WHERE status = 'open';

CREATE TABLE cart_lines (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL,
    cart_id    uuid NOT NULL,
    variant_id uuid NOT NULL,
    quantity   integer NOT NULL CHECK (quantity BETWEEN 1 AND 999),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, cart_id, variant_id),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, variant_id) REFERENCES variants (tenant_id, id) ON DELETE CASCADE
);

-- At most one coupon per cart (§10.2 stacking rule).
CREATE TABLE cart_coupons (
    tenant_id  uuid NOT NULL,
    cart_id    uuid NOT NULL,
    coupon_id  uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, cart_id),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, coupon_id) REFERENCES coupons (tenant_id, id) ON DELETE CASCADE
);

-- A1: single-use, 60 s, hashed at rest, bound to the market it was minted for.
CREATE TABLE checkout_handoffs (
    token_hash bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id  uuid NOT NULL,
    cart_id    uuid NOT NULL,
    market_id  uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX checkout_handoffs_expiry ON checkout_handoffs (tenant_id, expires_at);

-- ---------------------------------------------------------------------------------------
-- Theme artifacts and revisions (§7.5, §9.7, A22, A30). Artifacts are immutable and
-- content-addressed; their files live in the private bucket under `artifacts/<id>/`.
-- A channel names the artifact platform-wide (`default-theme`, `checkout`).

CREATE TABLE platform.theme_artifacts (
    id         text PRIMARY KEY CHECK (id ~ '^[0-9a-f]{32}$'),
    kind       text NOT NULL CHECK (kind IN ('theme', 'checkout')),
    -- Design tokens from the manifest (A6), exposed to themes via `GET /shop`.
    tokens     jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE platform.artifact_channels (
    name        text PRIMARY KEY CHECK (name IN ('default-theme', 'checkout')),
    artifact_id text NOT NULL REFERENCES platform.theme_artifacts (id),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
GRANT SELECT ON platform.theme_artifacts, platform.artifact_channels TO app_runtime;
GRANT INSERT, UPDATE ON platform.theme_artifacts, platform.artifact_channels TO app_runtime;

-- `origin = 'default'`: the revision follows the shared default artifact and is advanced when
-- a new default is published. M3 adds `custom` revisions built from tenant sources.
CREATE TABLE theme_revisions (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    number       integer NOT NULL CHECK (number > 0),
    parent_id    uuid,
    artifact_id  text NOT NULL REFERENCES platform.theme_artifacts (id),
    origin       text NOT NULL DEFAULT 'default' CHECK (origin IN ('default', 'custom')),
    status       text NOT NULL CHECK (status IN ('draft', 'building', 'checking', 'ready', 'failed',
                                                 'published', 'superseded')),
    checks       jsonb NOT NULL DEFAULT '{}',
    created_by   text NOT NULL,
    published_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, number),
    FOREIGN KEY (tenant_id, parent_id) REFERENCES theme_revisions (tenant_id, id)
);

CREATE TABLE theme_active (
    tenant_id   uuid PRIMARY KEY REFERENCES platform.tenants (id),
    revision_id uuid NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (tenant_id, revision_id) REFERENCES theme_revisions (tenant_id, id)
);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['redirects', 'carts', 'cart_lines', 'cart_coupons',
        'checkout_handoffs', 'theme_revisions', 'theme_active']
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
