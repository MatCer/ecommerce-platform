-- Platform-level schema (tenants, domains, platform admins arrive in WP1).
-- Runs as app_owner; app_runtime (no BYPASSRLS, not owner) gets data access only.
CREATE SCHEMA IF NOT EXISTS platform;

GRANT USAGE ON SCHEMA platform TO app_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA platform
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA platform
    GRANT USAGE, SELECT ON SEQUENCES TO app_runtime;
