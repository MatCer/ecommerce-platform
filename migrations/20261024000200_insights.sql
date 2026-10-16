ALTER TABLE ai_settings ADD COLUMN insights boolean NOT NULL DEFAULT false;

CREATE TABLE insight_signals (
    tenant_id         uuid NOT NULL REFERENCES platform.tenants (id),
    id                uuid PRIMARY KEY,
    kind              text NOT NULL CHECK (kind IN ('funnel_drop', 'low_add_to_cart', 'slow_page')),
    subject           text NOT NULL CHECK (length(subject) BETWEEN 1 AND 200),
    detection         jsonb NOT NULL,
    score             double precision NOT NULL CHECK (score > 0 AND score < 'Infinity'),
    status            text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'dismissed', 'resolved')),
    first_detected_on date NOT NULL,
    last_detected_on  date NOT NULL CHECK (last_detected_on >= first_detected_on),
    hypothesis        text CHECK (length(hypothesis) BETWEEN 1 AND 500),
    theme_prompt      text CHECK (length(theme_prompt) BETWEEN 20 AND 1500),
    hypothesis_model  text,
    hypothesis_error  text,
    hypothesis_at     timestamptz,
    dismissed_at      timestamptz,
    dismissed_by      text,
    UNIQUE (tenant_id, kind, subject),
    CHECK ((hypothesis IS NULL) = (theme_prompt IS NULL)
           AND (hypothesis IS NULL) = (hypothesis_model IS NULL)),
    CHECK (hypothesis IS NULL OR hypothesis_error IS NULL),
    CHECK ((hypothesis_at IS NULL) = (hypothesis IS NULL AND hypothesis_error IS NULL)),
    CHECK ((status = 'dismissed') = (dismissed_at IS NOT NULL AND dismissed_by IS NOT NULL))
);
CREATE INDEX insight_signals_open ON insight_signals
    (tenant_id, score DESC, kind COLLATE "C", subject COLLATE "C") WHERE status = 'open';

ALTER TABLE insight_signals ENABLE ROW LEVEL SECURITY;
ALTER TABLE insight_signals FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON insight_signals TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);

GRANT SELECT, INSERT, UPDATE, DELETE ON insight_signals TO app_runtime;

CREATE FUNCTION insight_product_name(p uuid) RETURNS text
LANGUAGE sql STABLE SET search_path = '' AS $$
    SELECT t.name FROM public.product_translations t
    WHERE t.product_id = p
    ORDER BY (t.locale = (SELECT default_locale FROM public.markets WHERE is_default))
             DESC NULLS LAST, t.locale
    LIMIT 1
$$;
REVOKE ALL ON FUNCTION insight_product_name(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION insight_product_name(uuid) TO app_runtime;
