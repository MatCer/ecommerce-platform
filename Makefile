# Local entrypoints (spec §15). `make help` lists targets.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := help

# .env holds host ports and local credentials; created from .env.example on first use.
-include .env
.env:
	cp .env.example .env

export CARGO_BUILD_JOBS ?= 6

PG_PORT ?= 55432
MEILI_PORT ?= 57700
MEILI_SEARCH_KEY ?= 2245a27fd200f741b246ce0479586838d71d3f5925e973144596c1ed10e3d918
MEILI_ADMIN_KEY ?= 39acad57a641ce3bb328074fcae83ae01e5cc0edbb28c2ae113d3c6b246901f6
HTTPS_PORT ?= 8443
APP_OWNER_PASSWORD ?= app-owner-local
OWNER_DATABASE_URL ?= postgres://app_owner:$(APP_OWNER_PASSWORD)@localhost:$(PG_PORT)/app
TEST_DATABASE_URL ?= postgres://app_owner:$(APP_OWNER_PASSWORD)@localhost:$(PG_PORT)/app_test

COMPOSE_FULL := COMPOSE_PROFILES=full docker compose
COMPOSE_INFRA := COMPOSE_PROFILES=infra docker compose

.PHONY: help up down dev-infra migrate sqlx-prepare test test-rust test-ts test-search lint fmt openapi openapi-check admin seed logs ps theme-build perf e2e backup restore backup-drill

help: ## List targets
	@grep -hE '^[a-z-]+:.*## ' Makefile | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

up: .env ## Build and start the whole stack, wait until healthy
	$(COMPOSE_FULL) up -d --build --wait

down: ## Stop the stack (volumes are kept; `docker compose down -v` wipes data)
	$(COMPOSE_FULL) down

dev-infra: .env ## Start dependencies only; run api/worker natively against them
	$(COMPOSE_INFRA) up -d --build --wait

migrate: ## Apply migrations as app_owner (needs sqlx-cli)
	sqlx migrate run --source migrations --database-url "$(OWNER_DATABASE_URL)"

sqlx-prepare: ## Refresh .sqlx/ (offline query data) after SQL changes; needs `make migrate` first
	SQLX_OFFLINE=false DATABASE_URL="$(OWNER_DATABASE_URL)" cargo sqlx prepare --workspace -- --all-targets

test: test-rust test-ts ## Rust + TS tests (Rust integration tests need `make dev-infra`)

test-rust:
	DATABASE_URL="$(TEST_DATABASE_URL)" cargo test --workspace --locked

test-ts:
	pnpm test

test-search: ## Search integration tests + cs/sk relevance fixtures against Meilisearch (needs `make dev-infra`)
	DATABASE_URL="$(TEST_DATABASE_URL)" MEILI_URL="http://localhost:$(MEILI_PORT)" \
	MEILI_ADMIN_KEY="$(MEILI_ADMIN_KEY)" MEILI_SEARCH_KEY="$(MEILI_SEARCH_KEY)" \
	cargo test --workspace --locked -- --ignored

lint: ## rustfmt check, clippy, Biome, TS typecheck
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	pnpm run lint
	pnpm run typecheck

fmt: ## Format Rust and TS
	cargo fmt --all
	pnpm run fmt

openapi: ## Regenerate openapi.json (+ the storefront subset) and the TS clients from the Rust API
	cargo run --quiet --locked -p api -- openapi > openapi.json.tmp
	mv openapi.json.tmp openapi.json
	cargo run --quiet --locked -p api -- openapi --storefront > openapi.storefront.json.tmp
	mv openapi.storefront.json.tmp openapi.storefront.json
	pnpm run openapi:generate

openapi-check: ## Fail if openapi.json or the generated clients are stale
	scripts/openapi-drift.sh

admin: ## Superadmin CLI in the api container, e.g. make admin args="create-tenant --slug demo --name Demo --owner-email you@example.com"
	$(COMPOSE_FULL) exec api /usr/local/bin/api admin $(args)

seed: ## Create or complete the demo shop (demo.localhost CZ, demo-sk.localhost SK); idempotent
	$(COMPOSE_FULL) exec api /usr/local/bin/api admin seed-demo

logs: ## Follow logs (`make logs s=api` for one service)
	$(COMPOSE_FULL) logs -f --tail=100 $(s)

ps: ## Show stack status
	$(COMPOSE_FULL) ps

theme-build: ## Build + pack the default theme and checkout (A22), upload + publish them for every tenant (A30)
	scripts/build-artifacts.sh
	node packages/theme-kit/src/cli.ts verify --root .artifacts \
		"$$(cat .artifacts/channels/default-theme)" "$$(cat .artifacts/channels/checkout)"
	$(COMPOSE_FULL) run --rm --no-deps -v "$(CURDIR)/.artifacts:/artifacts:ro" api \
		/usr/local/bin/api admin publish-artifacts --root /artifacts \
		--theme "$$(cat .artifacts/channels/default-theme)" --checkout "$$(cat .artifacts/channels/checkout)"

backup: ## Dump Postgres + mirror the MinIO buckets to backups/<UTC timestamp>/ (runbook §7)
	scripts/backup.sh

restore: ## Restore a backup into the running stack: make restore BACKUP=backups/<ts>
	scripts/restore.sh "$(BACKUP)"

backup-drill: ## A29 drill: backup, wipe volumes, make up, restore, verify (needs DRILL_CONFIRM=destroy)
	scripts/backup-drill.sh

e2e: ## Playwright suites against the running stack (`make up` first; at most 4 workers)
	HTTP_PORT=$(or $(HTTP_PORT),8080) pnpm --filter @platform/e2e exec playwright test $(args)

perf: ## Lab budget + axe gate (spec §9.6/A26) against the running stack over HTTPS/h2
	node packages/theme-kit/src/measure.ts --base https://demo.localhost:$(HTTPS_PORT) --runs 3
