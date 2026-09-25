-- WP13a: content (pages, blog, menus), legal entity, feed imports/exports, search synonyms
-- (spec §7.5, §9.5, §10.8, §14, A2, A18, A21, A28, A29).
--
-- Tenant tables follow the WP1/WP3 rules: tenant_id, RLS + FORCE, composite FKs.

-- ---------------------------------------------------------------------------------------
-- Pages: CMS pages, legal pages and blog posts. Content lives per locale as typed blocks
-- (commerce::content::Block), validated and sanitized on write.

CREATE TABLE pages (
    id             uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id      uuid NOT NULL REFERENCES platform.tenants (id),
    kind           text NOT NULL CHECK (kind IN ('page', 'legal', 'blog_post')),
    -- Legal pages installed from a platform template (one page per type and tenant).
    legal_type     text CHECK (legal_type IN ('terms', 'privacy', 'cookies', 'withdrawal',
                                              'complaints', 'reviews')),
    status         text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    -- Visible from this instant once published (scheduling); set on first publish.
    published_at   timestamptz,
    -- Cover image (blog cards, OpenGraph).
    image_asset_id uuid,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CHECK ((kind = 'legal') = (legal_type IS NOT NULL)),
    CHECK (status = 'draft' OR published_at IS NOT NULL),
    FOREIGN KEY (tenant_id, image_asset_id) REFERENCES assets (tenant_id, id)
        ON DELETE SET NULL (image_asset_id)
);
CREATE UNIQUE INDEX pages_legal_type ON pages (tenant_id, legal_type) WHERE legal_type IS NOT NULL;
CREATE INDEX pages_published ON pages (tenant_id, kind, published_at DESC) WHERE status = 'published';

CREATE TABLE page_translations (
    tenant_id       uuid NOT NULL,
    page_id         uuid NOT NULL,
    locale          text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    title           text NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    -- Pages live at /pages/<slug>, posts at /blog/<slug>; one slug namespace per locale keeps
    -- links unambiguous.
    slug            text NOT NULL CHECK (slug ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(slug) <= 200),
    excerpt         text NOT NULL DEFAULT '' CHECK (length(excerpt) <= 500),
    blocks          jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(blocks) = 'array'),
    seo_title       text CHECK (length(seo_title) <= 200),
    seo_description text CHECK (length(seo_description) <= 500),
    PRIMARY KEY (tenant_id, page_id, locale),
    CONSTRAINT page_translations_locale_slug_key UNIQUE (tenant_id, locale, slug),
    FOREIGN KEY (tenant_id, page_id) REFERENCES pages (tenant_id, id) ON DELETE CASCADE
);

-- Menus by handle (`main`, `footer`, ...). Items are validated JSON (commerce::content::menus):
-- links to categories, products, pages or URLs, at most two levels.
CREATE TABLE menus (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    handle     text NOT NULL CHECK (handle ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
    items      jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(items) = 'array'),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT menus_handle_unique UNIQUE (tenant_id, handle)
);

-- The seller's legal identity (§14, A29): fills the legal templates and gates go-live.
CREATE TABLE legal_entities (
    tenant_id  uuid PRIMARY KEY REFERENCES platform.tenants (id),
    data       jsonb NOT NULL CHECK (jsonb_typeof(data) = 'object'),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- Feed imports (§10.8, A28). A run holds the source file (private bucket), the dry-run report
-- and the apply progress. Mappings tie external ids to created entities, so a re-import
-- updates instead of duplicating.

CREATE TABLE import_runs (
    id          uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    source      text NOT NULL CHECK (source IN ('heureka', 'google')),
    market_id   uuid NOT NULL,
    -- The merchant's URL (downloaded through the SSRF-safe client) or NULL for an upload.
    url         text CHECK (length(url) <= 2000),
    object_key  text NOT NULL,
    -- New products are drafts unless the merchant chose to publish them right away (A28).
    activate    boolean NOT NULL DEFAULT false,
    status      text NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'analyzing', 'analyzed', 'applying', 'applied',
                                  'failed')),
    report      jsonb,
    progress    jsonb NOT NULL DEFAULT '{}',
    error       text CHECK (length(error) <= 2000),
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    applied_at  timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id)
);
CREATE INDEX import_runs_recent ON import_runs (tenant_id, id DESC);

CREATE TABLE import_mappings (
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    source      text NOT NULL CHECK (source IN ('heureka', 'google')),
    entity_type text NOT NULL CHECK (entity_type IN ('product', 'variant', 'category',
                                                     'parameter', 'asset')),
    external_id text NOT NULL CHECK (length(external_id) BETWEEN 1 AND 2000),
    -- No FK: the entity may be deleted later; a stale mapping is ignored and replaced.
    entity_id   uuid NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, source, entity_type, external_id)
);
CREATE INDEX import_mappings_entity ON import_mappings (tenant_id, entity_type, entity_id);

-- Generated export feeds per market and channel (the XML itself is in the private bucket).
CREATE TABLE feed_exports (
    tenant_id    uuid NOT NULL,
    market_id    uuid NOT NULL,
    channel      text NOT NULL CHECK (channel IN ('google', 'heureka', 'zbozi')),
    object_key   text NOT NULL,
    items        integer NOT NULL CHECK (items >= 0),
    bytes        bigint NOT NULL CHECK (bytes >= 0),
    generated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, market_id, channel),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

-- Tenant synonyms (WP7 follow-up): groups of equivalent words as typed by the merchant; the
-- worker normalizes them and applies them to every index of the tenant.
CREATE TABLE search_synonyms (
    tenant_id  uuid PRIMARY KEY REFERENCES platform.tenants (id),
    groups     jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(groups) = 'array'),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['pages', 'page_translations', 'menus', 'legal_entities',
        'import_runs', 'import_mappings', 'feed_exports', 'search_synonyms']
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
