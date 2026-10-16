-- NULL: AI alt texts are off; otherwise when they were turned on (only images uploaded since
-- then get them without an explicit backfill).
ALTER TABLE ai_settings ADD COLUMN alt_texts_since timestamptz;

-- Alt text marks name the image: `alt_<asset id, 32 hex>`.
ALTER TABLE ai_marks DROP CONSTRAINT ai_marks_field_check;
ALTER TABLE ai_marks ADD CONSTRAINT ai_marks_field_check CHECK (field ~ '^[a-z0-9_]{1,40}$');
