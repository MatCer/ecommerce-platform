SET LOCAL lock_timeout = '5s';

ALTER TABLE carts
    ADD COLUMN source_cart_id uuid,
    ADD CONSTRAINT carts_source_fk FOREIGN KEY (tenant_id, source_cart_id)
        REFERENCES carts (tenant_id, id) ON DELETE SET NULL (source_cart_id),
    ADD CONSTRAINT carts_snapshot_without_shop_capability
        CHECK (source_cart_id IS NULL OR shop_token_hash IS NULL);
CREATE INDEX carts_source ON carts (tenant_id, source_cart_id) WHERE source_cart_id IS NOT NULL;
