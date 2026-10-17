SET LOCAL lock_timeout = '5s';

ALTER TABLE carts
    ADD COLUMN replaced_token_hash bytea CHECK (length(replaced_token_hash) = 32);
CREATE UNIQUE INDEX carts_replaced_token ON carts (tenant_id, replaced_token_hash)
    WHERE replaced_token_hash IS NOT NULL;
