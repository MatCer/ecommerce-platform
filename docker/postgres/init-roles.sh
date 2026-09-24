#!/usr/bin/env bash
# Creates the application roles and databases. Runs once on first start of an empty data
# directory (docker-entrypoint-initdb.d); CI runs it against its Postgres service too.
#   app_owner   owns the databases and runs migrations; CREATEDB for #[sqlx::test] databases.
#   app_runtime what api and worker connect as: not an owner, no BYPASSRLS, so RLS applies.
# Passwords arrive as psql variables (:'var' quoting), never interpolated into SQL text.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres \
  -v owner_pw="$APP_OWNER_PASSWORD" -v runtime_pw="$APP_RUNTIME_PASSWORD" <<'SQL'
CREATE ROLE app_owner LOGIN CREATEDB NOSUPERUSER NOCREATEROLE NOBYPASSRLS PASSWORD :'owner_pw';
CREATE ROLE app_runtime LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS PASSWORD :'runtime_pw';

CREATE DATABASE app OWNER app_owner;
CREATE DATABASE app_test OWNER app_owner;

REVOKE ALL ON DATABASE app, app_test FROM PUBLIC;
GRANT CONNECT, TEMPORARY ON DATABASE app, app_test TO app_runtime;
SQL
