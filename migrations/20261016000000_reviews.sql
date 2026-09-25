-- WP16: product reviews (spec §7.6, §11.6, §11.7, §14 Omnibus review verification).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Review tokens
-- are capabilities (256-bit, only the SHA-256 is stored), one per delivered order line, single
-- use and expiring: only a buyer can submit, and a submission is "verified" because it is
-- linked to a delivered order line.

CREATE TABLE review_tokens (
    tenant_id     uuid NOT NULL REFERENCES platform.tenants (id),
    order_line_id uuid NOT NULL,
    order_id      uuid NOT NULL,
    product_id    uuid NOT NULL,
    token_hash    bytea NOT NULL CHECK (length(token_hash) = 32),
    expires_at    timestamptz NOT NULL,
    used_at       timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, order_line_id),
    FOREIGN KEY (tenant_id, order_line_id) REFERENCES order_lines (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);
-- Global: a token hash names exactly one row, whatever the tenant.
CREATE UNIQUE INDEX review_tokens_hash ON review_tokens (token_hash);

CREATE TABLE reviews (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL REFERENCES platform.tenants (id),
    product_id    uuid NOT NULL,
    -- The delivered order line behind a verified review (NULL once the order is erased).
    order_line_id uuid,
    customer_name text NOT NULL CHECK (length(customer_name) BETWEEN 1 AND 60),
    rating        smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    title         text NOT NULL DEFAULT '' CHECK (length(title) <= 120),
    body          text NOT NULL CHECK (length(body) BETWEEN 1 AND 4000),
    status        text NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'published', 'rejected', 'hidden')),
    -- Linked to a delivered order line when written (Omnibus disclosure).
    verified      boolean NOT NULL,
    locale        text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    reply         text CHECK (length(reply) BETWEEN 1 AND 2000),
    replied_at    timestamptz,
    moderated_at  timestamptz,
    moderated_by  text,
    ip_hash       bytea CHECK (length(ip_hash) = 32),
    created_at    timestamptz NOT NULL DEFAULT now(),
    published_at  timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK (status <> 'published' OR published_at IS NOT NULL),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, order_line_id) REFERENCES order_lines (tenant_id, id)
        ON DELETE SET NULL (order_line_id)
);
-- One review per order line.
CREATE UNIQUE INDEX reviews_order_line ON reviews (tenant_id, order_line_id)
    WHERE order_line_id IS NOT NULL;
CREATE INDEX reviews_published ON reviews (tenant_id, product_id, published_at DESC)
    WHERE status = 'published';
CREATE INDEX reviews_queue ON reviews (tenant_id, status, id DESC);

-- Per-IP submission cap (the rate-limit ledger of customer sign-ins, purged daily).
ALTER TABLE customer_auth_attempts
    DROP CONSTRAINT customer_auth_attempts_kind_check,
    ADD CONSTRAINT customer_auth_attempts_kind_check
        CHECK (kind IN ('magic_link', 'login_failed', 'newsletter', 'review'));

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['review_tokens', 'reviews']
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
