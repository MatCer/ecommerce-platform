-- WP3: catalog, media assets and per-country tax categories (spec §7.1, §7.5, A3, A21).
--
-- Every table below is a tenant table (RLS + FORCE, `tenant_isolation` for app_runtime) and
-- every reference between them is a composite `(tenant_id, ...)` foreign key, so a row can
-- never point at another tenant's row even if application code passes a foreign id.
-- The only exception is `platform.tax_categories`: statutory rates are platform reference
-- data, identical for every tenant and read-only for the application.

-- ---------------------------------------------------------------------------------------
-- Tax categories (A3). Rates are law, maintained by migrations, never by tenants.
-- The rate of (country, code) at date D is the row with the latest valid_from <= D.

CREATE TABLE platform.tax_categories (
    country    text NOT NULL CHECK (country ~ '^[A-Z]{2}$'),
    code       text NOT NULL CHECK (code IN ('standard', 'reduced', 'second_reduced', 'super_reduced')),
    rate       numeric(5, 2) NOT NULL CHECK (rate >= 0 AND rate < 100),
    valid_from date NOT NULL,
    PRIMARY KEY (country, code, valid_from)
);
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON platform.tax_categories FROM app_runtime;
GRANT SELECT ON platform.tax_categories TO app_runtime;

-- Seed: EU-27 standard and reduced rates.
-- Source: European Parliamentary Research Service, "Highs and lows: VAT rate-setting in the
-- European Union", PE 782.613 (January 2026), Table 3 "VAT rate overview, standard and reduced
-- rates, EU, 1 July 2025" (data: European Commission Taxes in Europe database, updated
-- 1 July 2025); super-reduced rates from the Commission's "VAT rates applied in the Member
-- States" list. Changes after 1 July 2025, from the national legislation as reported by the
-- Commission/tax authorities:
--   RO 2025-08-01: standard 19 -> 21, the 5 % and 9 % rates replaced by a single 11 %
--                  (Law 141/2025; transitional 9 % for housing not modelled)
--   LT 2026-01-01: reduced 9 -> 12 (5 % kept) (2026 budget law)
--   FI 2026-01-01: reduced 14 -> 13.5 (Government proposal HE 95/2025, vero.fi)
-- Convention: `reduced` is a member state's higher reduced rate, `second_reduced` the lower
-- one, `super_reduced` a rate below 5 %. valid_from 2025-07-01 means "in force on that date"
-- (the date of the source table), not the date the rate was introduced.
-- Merchants must have their category mapping confirmed by an accountant (A3).
INSERT INTO platform.tax_categories (country, code, rate, valid_from) VALUES
    ('AT', 'standard', 20, '2025-07-01'), ('AT', 'reduced', 13, '2025-07-01'), ('AT', 'second_reduced', 10, '2025-07-01'),
    ('BE', 'standard', 21, '2025-07-01'), ('BE', 'reduced', 12, '2025-07-01'), ('BE', 'second_reduced', 6, '2025-07-01'),
    ('BG', 'standard', 20, '2025-07-01'), ('BG', 'reduced', 9, '2025-07-01'),
    ('CY', 'standard', 19, '2025-07-01'), ('CY', 'reduced', 9, '2025-07-01'), ('CY', 'second_reduced', 5, '2025-07-01'),
    ('CZ', 'standard', 21, '2025-07-01'), ('CZ', 'reduced', 12, '2025-07-01'),
    ('DE', 'standard', 19, '2025-07-01'), ('DE', 'reduced', 7, '2025-07-01'),
    ('DK', 'standard', 25, '2025-07-01'),
    ('EE', 'standard', 24, '2025-07-01'), ('EE', 'reduced', 13, '2025-07-01'), ('EE', 'second_reduced', 9, '2025-07-01'),
    ('ES', 'standard', 21, '2025-07-01'), ('ES', 'reduced', 10, '2025-07-01'), ('ES', 'super_reduced', 4, '2025-07-01'),
    ('FI', 'standard', 25.5, '2025-07-01'), ('FI', 'reduced', 14, '2025-07-01'), ('FI', 'second_reduced', 10, '2025-07-01'),
    ('FI', 'reduced', 13.5, '2026-01-01'),
    ('FR', 'standard', 20, '2025-07-01'), ('FR', 'reduced', 10, '2025-07-01'), ('FR', 'second_reduced', 5.5, '2025-07-01'),
    ('FR', 'super_reduced', 2.1, '2025-07-01'),
    ('GR', 'standard', 24, '2025-07-01'), ('GR', 'reduced', 13, '2025-07-01'), ('GR', 'second_reduced', 6, '2025-07-01'),
    ('HR', 'standard', 25, '2025-07-01'), ('HR', 'reduced', 13, '2025-07-01'), ('HR', 'second_reduced', 5, '2025-07-01'),
    ('HU', 'standard', 27, '2025-07-01'), ('HU', 'reduced', 18, '2025-07-01'), ('HU', 'second_reduced', 5, '2025-07-01'),
    ('IE', 'standard', 23, '2025-07-01'), ('IE', 'reduced', 13.5, '2025-07-01'), ('IE', 'second_reduced', 9, '2025-07-01'),
    ('IE', 'super_reduced', 4.8, '2025-07-01'),
    ('IT', 'standard', 22, '2025-07-01'), ('IT', 'reduced', 10, '2025-07-01'), ('IT', 'second_reduced', 5, '2025-07-01'),
    ('IT', 'super_reduced', 4, '2025-07-01'),
    ('LT', 'standard', 21, '2025-07-01'), ('LT', 'reduced', 9, '2025-07-01'), ('LT', 'second_reduced', 5, '2025-07-01'),
    ('LT', 'reduced', 12, '2026-01-01'),
    ('LU', 'standard', 17, '2025-07-01'), ('LU', 'reduced', 14, '2025-07-01'), ('LU', 'second_reduced', 8, '2025-07-01'),
    ('LU', 'super_reduced', 3, '2025-07-01'),
    ('LV', 'standard', 21, '2025-07-01'), ('LV', 'reduced', 12, '2025-07-01'), ('LV', 'second_reduced', 5, '2025-07-01'),
    ('MT', 'standard', 18, '2025-07-01'), ('MT', 'reduced', 7, '2025-07-01'), ('MT', 'second_reduced', 5, '2025-07-01'),
    ('NL', 'standard', 21, '2025-07-01'), ('NL', 'reduced', 9, '2025-07-01'),
    ('PL', 'standard', 23, '2025-07-01'), ('PL', 'reduced', 8, '2025-07-01'), ('PL', 'second_reduced', 5, '2025-07-01'),
    ('PT', 'standard', 23, '2025-07-01'), ('PT', 'reduced', 13, '2025-07-01'), ('PT', 'second_reduced', 6, '2025-07-01'),
    ('RO', 'standard', 21, '2025-08-01'), ('RO', 'reduced', 11, '2025-08-01'),
    ('SE', 'standard', 25, '2025-07-01'), ('SE', 'reduced', 12, '2025-07-01'), ('SE', 'second_reduced', 6, '2025-07-01'),
    ('SI', 'standard', 22, '2025-07-01'), ('SI', 'reduced', 9.5, '2025-07-01'), ('SI', 'second_reduced', 5, '2025-07-01'),
    ('SK', 'standard', 23, '2025-07-01'), ('SK', 'reduced', 19, '2025-07-01'), ('SK', 'second_reduced', 5, '2025-07-01');

-- ---------------------------------------------------------------------------------------
-- Assets (§7.5, A21). The uploaded original lives in the private bucket (`key`); re-encoded
-- variants live in the public bucket and are listed in `variants`.

CREATE TABLE assets (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    status     text NOT NULL DEFAULT 'pending'
               CHECK (status IN ('pending', 'processing', 'ready', 'failed')),
    filename   text CHECK (length(filename) <= 255),
    key        text NOT NULL,
    mime       text,
    bytes      bigint CHECK (bytes > 0),
    width      integer CHECK (width > 0),
    height     integer CHECK (height > 0),
    sha256     text CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    variants   jsonb NOT NULL DEFAULT '[]',
    error      text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);
CREATE INDEX assets_tenant_id ON assets (tenant_id, id DESC);

-- ---------------------------------------------------------------------------------------
-- Products (§7.1).

CREATE TABLE products (
    id               uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id        uuid NOT NULL REFERENCES platform.tenants (id),
    status           text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'archived')),
    brand            text CHECK (length(brand) BETWEEN 1 AND 200),
    -- Validated against commerce::catalog::Gpsr before every write.
    gpsr             jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(gpsr) = 'object'),
    -- Unit price basis (price per kg / l / ...): both set or both null.
    unit_measure     text CHECK (unit_measure IN ('kg', 'l', 'm', 'm2', 'pcs')),
    unit_quantity    numeric(12, 4) CHECK (unit_quantity > 0),
    heureka_category text CHECK (length(heureka_category) <= 500),
    google_category  text CHECK (length(google_category) <= 500),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK ((unit_measure IS NULL) = (unit_quantity IS NULL))
);
CREATE INDEX products_tenant_id ON products (tenant_id, id DESC);
CREATE INDEX products_tenant_status ON products (tenant_id, status, id DESC);

CREATE TABLE product_translations (
    tenant_id         uuid NOT NULL,
    product_id        uuid NOT NULL,
    locale            text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    name              text NOT NULL CHECK (length(name) BETWEEN 1 AND 300),
    slug              text NOT NULL CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 200),
    -- Sanitized with ammonia before storage (spec §14).
    description_html  text NOT NULL DEFAULT '',
    short_description text NOT NULL DEFAULT '',
    seo_title         text,
    seo_description   text,
    PRIMARY KEY (tenant_id, product_id, locale),
    UNIQUE (tenant_id, locale, slug),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);

-- `values`: [{code, name_i18n}]; variants reference option and value codes.
CREATE TABLE product_options (
    tenant_id  uuid NOT NULL,
    product_id uuid NOT NULL,
    code       text NOT NULL,
    position   integer NOT NULL,
    name_i18n  jsonb NOT NULL,
    "values"   jsonb NOT NULL,
    PRIMARY KEY (tenant_id, product_id, code),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);

-- Uniques are deferred so a replace can swap SKUs or option combinations between variants;
-- the service sets them IMMEDIATE before returning, so violations surface as 409s.
CREATE TABLE variants (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL,
    product_id    uuid NOT NULL,
    sku           text NOT NULL CHECK (length(sku) BETWEEN 1 AND 64),
    -- GTIN-8/12/13/14; the check digit is validated by the service.
    ean           text CHECK (ean ~ '^([0-9]{8}|[0-9]{12,14})$'),
    -- {"<option code>": "<value code>"}, one entry per product option.
    option_values jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(option_values) = 'object'),
    weight_g      integer CHECK (weight_g >= 0),
    position      integer NOT NULL,
    is_default    boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, product_id, id),
    CONSTRAINT variants_sku_unique UNIQUE (tenant_id, sku) DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT variants_options_unique UNIQUE (tenant_id, product_id, option_values)
        DEFERRABLE INITIALLY IMMEDIATE,
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX variants_one_default ON variants (tenant_id, product_id) WHERE is_default;

CREATE TABLE parameters (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    key        text NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
    name_i18n  jsonb NOT NULL,
    kind       text NOT NULL CHECK (kind IN ('text', 'number', 'bool')),
    unit       text CHECK (length(unit) BETWEEN 1 AND 20),
    filterable boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, key)
);

-- value: i18n object (text), number or boolean, matching the parameter kind (service-checked).
CREATE TABLE product_parameter_values (
    tenant_id    uuid NOT NULL,
    product_id   uuid NOT NULL,
    variant_id   uuid,
    parameter_id uuid NOT NULL,
    value        jsonb NOT NULL CHECK (jsonb_typeof(value) IN ('object', 'number', 'boolean')),
    position     integer NOT NULL,
    UNIQUE NULLS NOT DISTINCT (tenant_id, product_id, variant_id, parameter_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, product_id, variant_id)
        REFERENCES variants (tenant_id, product_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, parameter_id) REFERENCES parameters (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX product_parameter_values_parameter ON product_parameter_values (tenant_id, parameter_id);

CREATE TABLE categories (
    id             uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id      uuid NOT NULL REFERENCES platform.tenants (id),
    parent_id      uuid,
    position       integer NOT NULL,
    image_asset_id uuid,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK (parent_id <> id),
    FOREIGN KEY (tenant_id, parent_id) REFERENCES categories (tenant_id, id),
    FOREIGN KEY (tenant_id, image_asset_id) REFERENCES assets (tenant_id, id)
        ON DELETE SET NULL (image_asset_id)
);
CREATE INDEX categories_parent ON categories (tenant_id, parent_id, position);

CREATE TABLE category_translations (
    tenant_id        uuid NOT NULL,
    category_id      uuid NOT NULL,
    locale           text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    name             text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    slug             text NOT NULL CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 200),
    description_html text NOT NULL DEFAULT '',
    seo_title        text,
    seo_description  text,
    PRIMARY KEY (tenant_id, category_id, locale),
    UNIQUE (tenant_id, locale, slug),
    FOREIGN KEY (tenant_id, category_id) REFERENCES categories (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE product_categories (
    tenant_id   uuid NOT NULL,
    product_id  uuid NOT NULL,
    category_id uuid NOT NULL,
    -- Order of the product within the category listing.
    position    integer NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, product_id, category_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, category_id) REFERENCES categories (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX product_categories_category ON product_categories (tenant_id, category_id, position);

CREATE TABLE product_media (
    tenant_id  uuid NOT NULL,
    product_id uuid NOT NULL,
    variant_id uuid,
    asset_id   uuid NOT NULL,
    position   integer NOT NULL,
    alt_i18n   jsonb NOT NULL DEFAULT '{}',
    PRIMARY KEY (tenant_id, product_id, asset_id),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE,
    -- A deleted variant's images stay on the product.
    FOREIGN KEY (tenant_id, product_id, variant_id)
        REFERENCES variants (tenant_id, product_id, id) ON DELETE SET NULL (variant_id),
    -- An asset in use cannot be deleted.
    FOREIGN KEY (tenant_id, asset_id) REFERENCES assets (tenant_id, id)
);
CREATE INDEX product_media_asset ON product_media (tenant_id, asset_id);

-- A3: the product's tax category per country; no row means `standard`.
CREATE TABLE product_tax_categories (
    tenant_id  uuid NOT NULL,
    product_id uuid NOT NULL,
    country    text NOT NULL CHECK (country ~ '^[A-Z]{2}$'),
    code       text NOT NULL,
    PRIMARY KEY (tenant_id, product_id, country),
    FOREIGN KEY (tenant_id, product_id) REFERENCES products (tenant_id, id) ON DELETE CASCADE
);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['assets', 'products', 'product_translations', 'product_options',
        'variants', 'parameters', 'product_parameter_values', 'categories',
        'category_translations', 'product_categories', 'product_media', 'product_tax_categories']
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
