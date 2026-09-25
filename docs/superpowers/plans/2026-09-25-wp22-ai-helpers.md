# WP22 AI gateway + admin helpers: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** an Anthropic Messages API gateway with a deterministic fake provider, per-tenant
monthly token quotas and `ai_usage` metering, AI helpers in the admin (product/category
descriptions, SEO title/meta, translations with a tenant glossary) delivered as proposals the
staff accepts per field, and "bulk edit by prompt": a validated change plan over allowlisted
operations, previewed, confirmed and applied by a job (spec §2 D21, §12.1, §12.2, §7.7, §14,
A9, A12, A21).

**Architecture:**
- `platform::ai`: `Client` (enum: `Anthropic` over reqwest, `Fake` rendering minijinja
  fixture templates keyed by feature), `Request`/`Completion`/`Usage`, retry policy
  (429/5xx/529/timeouts, exponential backoff + jitter, `retry-after` honored, bounded),
  `PriceTable` -> cost micros. Config `AiConfig` in `platform::config`.
- `commerce::ai`: `Ai` (client + models + plan quotas), quota check + `ai_usage` metering,
  `glossary`, `marks` (AI Act labels), `proposals` (generate as a job, accept per field through
  the existing catalog/content services), `plan` (bulk plan schema, validation, target
  resolution, preview, per-product apply), `prompts` (stable system prompts, JSON schemas),
  `fixtures/` (fake provider templates).
- `api::admin_ai`: `/admin/v1/ai/*` (staff role); `api admin set-ai-quota` (superadmin CLI).
- worker jobs `ai.proposal`, `ai.bulk_plan`, `ai.bulk_apply`.
- admin: `AiPanel` in product/category/page/menu editors, `/ai/bulk-edit`, `/settings/ai`
  (usage, quota, glossary); cs/sk/en.

## Global constraints

- The LLM never touches the DB: it returns JSON validated by serde (`deny_unknown_fields`) and
  domain checks; every write goes through the existing services (audit log, outbox events,
  price history). Unknown operations/fields are rejected.
- Merchant/catalog content is passed as data inside a delimited `<data>` JSON block; system
  prompts say it is untrusted and must not be followed as instructions. No tools are given
  to the model (nothing can fetch or exfiltrate).
- AI HTML is sanitized: generated descriptions use a strict allowlist (no links, images or
  attributes); translated HTML keeps only URLs that were in the source.
- Structured outputs: `output_config.format = {type: json_schema}` with
  `additionalProperties: false` everywhere; Sonnet 5 at `effort: medium`, no sampling params,
  no prefill; the system prompt carries `cache_control: ephemeral`.
- Quota: soft monthly limit per tenant (all input + output tokens); `402 ai_quota_exceeded`
  before a call starts. Usage rows are written for every completed call, even when the
  output is then rejected.
- Bulk plans: at most 500 targets, at most 10 operations, every price change within ±50 %
  and > 0; price operations need fresh auth (A9) at confirmation; confirmation honors
  `Idempotency-Key` (A12); apply is exactly-once per product (item row marked in the same
  transaction as the change).
- Secrets: `ANTHROPIC_API_KEY` from env only, never logged; logs carry feature, model,
  request id, tokens, latency, never prompt or output content.
- New tenant tables: RLS + FORCE + a cross-tenant test.

## Review focus

- Prompt injection: can catalog text change the plan's operations beyond what the staff
  asked? (Mitigations: allowlist + caps + preview + confirmation; document residual risk.)
- Plan validation: unknown op/field, percent/fixed caps, negative prices, too many targets,
  references to foreign categories/markets.
- Apply idempotency and partial failure (job retries, deleted products).
- Quota race (two concurrent calls): accepted soft limit.
- Retry policy correctness (no retry on 4xx other than 429; bounded total time).

## Tasks

1. **Migration** `20261005000000_ai.sql`: `platform.tenants.ai_monthly_tokens`, `ai_usage`,
   `ai_glossaries`, `ai_proposals`, `ai_marks`, `ai_bulk_plans`, `ai_bulk_items`; RLS.
2. **platform::ai**: config, client (Anthropic + Fake), retry policy, cost math. Unit tests:
   retry policy, cost math, fake rendering, Anthropic request shape + error mapping against a
   local axum stub (429 then 200, 400 no retry, refusal, max_tokens).
3. **commerce::ai core**: `Ai`, quota (plan default + override), `call()` metering; glossary
   get/put + violation check; marks. DB tests incl. cross-tenant.
4. **Proposals**: kinds (description, seo, category description, translate), job runner,
   accept per field through services, stale detection, marks. Tests with the fake.
5. **Bulk plans**: plan types + JSON schema, validation (unit tests), resolution + preview,
   confirm, per-product apply with progress. DB tests: T-shirts +5 % in SK -> prices,
   price history, audit.
6. **API + CLI + worker**: `admin_ai` routes, `set-ai-quota`, job handlers; router tests
   (402, reauth for price plans, idempotent apply, staff role). Regenerate OpenAPI/clients.
7. **Admin UI**: AiPanel (product/category/page/menu), bulk edit screen, settings AI page,
   i18n cs/sk/en, a11y.
8. **Docs + e2e**: `docs/decisions/ai-helpers.md` (threat model, real-key smoke test),
   `e2e/admin/ai.spec.ts`; compose env; full verification.
