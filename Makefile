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
HTTPS_PORT ?= 8443
APP_OWNER_PASSWORD ?= app-owner-local
OWNER_DATABASE_URL ?= postgres://app_owner:$(APP_OWNER_PASSWORD)@localhost:$(PG_PORT)/app
TEST_DATABASE_URL ?= postgres://app_owner:$(APP_OWNER_PASSWORD)@localhost:$(PG_PORT)/app_test

COMPOSE_FULL := COMPOSE_PROFILES=full docker compose
COMPOSE_INFRA := COMPOSE_PROFILES=infra docker compose

.PHONY: help up down dev-infra migrate test test-rust test-ts lint fmt openapi openapi-check logs ps theme-build perf

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

test: test-rust test-ts ## Rust + TS tests (Rust integration tests need `make dev-infra`)

test-rust:
	DATABASE_URL="$(TEST_DATABASE_URL)" cargo test --workspace --locked

test-ts:
	pnpm test

lint: ## rustfmt check, clippy, Biome, TS typecheck
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	pnpm run lint
	pnpm run typecheck

fmt: ## Format Rust and TS
	cargo fmt --all
	pnpm run fmt

openapi: ## Regenerate openapi.json and the TS clients from the Rust API
	cargo run --quiet --locked -p api -- openapi > openapi.json.tmp
	mv openapi.json.tmp openapi.json
	pnpm run openapi:generate

openapi-check: ## Fail if openapi.json or the generated clients are stale
	scripts/openapi-drift.sh

logs: ## Follow logs (`make logs s=api` for one service)
	$(COMPOSE_FULL) logs -f --tail=100 $(s)

ps: ## Show stack status
	$(COMPOSE_FULL) ps

theme-build: ## Build + pack the default theme and checkout artifacts into .artifacts (spec A22)
	scripts/build-artifacts.sh

perf: ## Lab budget + axe gate (spec §9.6/A26) against the running stack over HTTPS/h2
	node packages/theme-kit/src/measure.ts --base https://demo.localhost:$(HTTPS_PORT) --runs 3
