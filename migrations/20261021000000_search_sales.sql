CREATE TABLE product_search_sales (
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    product_id uuid NOT NULL,
    market_id  uuid NOT NULL,
    sales      double precision NOT NULL CHECK (sales > 0),
    PRIMARY KEY (tenant_id, product_id, market_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

ALTER TABLE product_search_sales ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_search_sales FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON product_search_sales TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE, DELETE ON product_search_sales TO app_runtime;

ALTER TABLE search_indexes ADD COLUMN rebuilt_settings_version integer;
