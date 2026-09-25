# WP24 AI theme editing: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** a merchant types a prompt ("make the product page more premium"); an agent loop
(Claude tool use, **outside** the sandbox, A6) edits a copy of the theme source through
file tools restricted to the contract, writes a functional check for its change, runs the
WP23 sandboxed gates, repairs up to 3 times, and leaves a run with transcript, diff and check
report. The merchant reviews the diff and report, then accepts (the revision enters the
normal WP23 preview/publish path) or discards it (spec §12.3, §17 WP24, A6, A9, A21;
`docs/decisions/ai-edit-prompts.md`, `theme-builder-sandbox.md`, `ai-helpers.md`).

**Architecture:**

```text
admin ─POST /admin/v1/themes/ai-runs─► api ─job themes.ai_edit─► worker: agent loop
                                                                   │  ▲
                        platform::ai::Client::converse (Anthropic tool use | fake agent)
                                                                   │
            tools on an in-memory workspace (archive::Source): list/read/write/delete_file
            run_checks ─► themes::create(Change::Ai) ─► themes.build ─► builder (WP23 gates,
                          incl. checks/*.spec.ts)  ◄── poll revision status (ready|failed)
```

- `platform::ai`: `Client::converse(&Conversation) -> Turn` (Messages API with `tools`,
  append-only history, prompt caching, effort; `refusal`/`max_tokens` are errors). The fake
  provider runs registered scripted agents (closures over the message history) and returns
  raw API-shaped responses that go through the same parser.
- `commerce::ai`: `record_usage` extracted from `call` (shared metering), `theme_model`,
  the fake theme agent (deterministic scripted patch).
- `commerce::themes::ai_edit`: runs table access, workspace tools (path/size/binary
  validation), the loop with hard limits, diff (`similar`), accept/discard/cancel, publish
  gate for AI revisions.
- `api`: `/admin/v1/themes/ai-runs[/{id}[/cancel|/accept|/discard]]`,
  `/admin/v1/themes/revisions/{id}/diff` (WP23 follow-up).
- `worker`: `themes.ai_edit` handler.
- `admin`: AI edit panel on `/themes` (prompt, progress, diff, report, accept/discard/cancel).
- `theme-kit` lint: `client-visible` rule (WP23 follow-up from WP2 prompt 8).

## Global Constraints

- Model output is untrusted: tool inputs are validated server-side (paths, sizes, UTF-8,
  allowed prefixes), unknown tools are errors fed back to the model, nothing but the
  workspace is reachable. The gates (WP23 sandbox) are the only place theme code runs.
- Tool scope: `src/**`, `public/**`, `theme.tokens.json`, and `checks/*.spec.ts` (the
  functional-check hook of WP23; a documented extension of the spec's list). No symlinks can
  exist (the workspace is a path → bytes map written as regular files); binary files cannot be
  read or written; ≤ 256 kB per file, archive limits of A6 for the whole source.
- Hard limits per run: 25 model turns, 4 check runs (1 + 3 repairs), 3 M tokens, USD 8 at
  list prices, 45 min wall clock, the tenant's monthly AI quota before every call;
  cancellation checked before each turn and while waiting for a build. One active run per
  tenant.
- Every endpoint: tenant-scoped (`TenantStaff`), Staff to read, Admin to start/cancel/accept/
  discard. Publishing an AI revision requires its run to be accepted (and A9 fresh auth as
  before). New table with `tenant_id`, RLS + FORCE RLS, cross-tenant test.
- Rust: no `unwrap()` outside tests, `thiserror`, sqlx macros with `.sqlx` committed.
  TS strict, Biome clean. Migration `20261018000000_ai_theme_edits.sql`.

## Review Focus

- Path validation of tool inputs (traversal, prefixes, `checks/` shape, sizes, binary).
- Limits cannot be bypassed (turn/check/token/cost/time counters persisted per run).
- Append-only conversation (thinking blocks echoed unchanged), `tool_result` for every
  `tool_use` in one user message.
- Tenant isolation of runs and of the revisions they create; publish gate.

## Tasks

### 1. `platform::ai::converse` + fake agents
Files: `crates/platform/src/ai.rs`.
- `Conversation { feature, model, system, tools: &Value, messages: &[Value], max_tokens, effort }`,
  `Turn { content: Vec<Value>, stop_reason: String, usage, model }`.
- Anthropic body: `system` block with `cache_control`, top-level `cache_control` (auto-cache the
  growing history), `tools`, `output_config.effort`, per-request timeout 10 min.
- `parse_turn(raw)`: refusal → `Refused`, `max_tokens` → `Truncated`, content must be an array.
- `Fake::with_agent(feature, Arc<dyn Fn(&[Value]) -> Value>)`: raw response JSON → `parse_turn`.
- Tests: body shape, parse of tool_use/refusal/max_tokens, fake agent round trip.

### 2. Migration + runs model
Files: `migrations/20261018000000_ai_theme_edits.sql`, `crates/commerce/src/themes/ai_edit.rs`.
- `ai_theme_runs` (status `queued|running|succeeded|failed|cancelled|accepted|discarded`,
  prompt, base revision, counters, usage, transcript, steps, diff, report, summary, error),
  `theme_revisions.ai_run_id`. RLS + FORCE.
- `start`, `list`, `detail`, `cancel`, `accept`, `discard`; publish gate in `revisions::publish`.

### 3. Workspace tools
- `Workspace { source, base }`, `run_tool(name, input) -> ToolOutcome { content, is_error }`;
  path rules reuse `archive::path_problem`. Unit tests for every rejection.

### 4. Agent loop + worker job
- `run(db, storage, ai, tenant, run_id)`: marks `running` (a retried job of a started run fails
  it as interrupted), loop with limits, `run_checks` → `create(Change::Ai)` + poll,
  final auto-check of unverified changes, diff, status. Usage metered per call.
- Fake theme agent: list → read `src/pages/index.astro` → write a note + `checks/ai-edit.spec.ts`
  → run_checks → summary.
- Integration tests (Postgres + in-memory storage, builder simulated by a task that answers the
  revision callbacks): happy path, repair after a failed check, turn limit, cancellation,
  quota, cross-tenant isolation, publish gate.

### 5. API + OpenAPI + clients
- Routes above, OpenAPI regenerated, admin client regenerated. API tests: roles, 404 across
  tenants, 409 on a second active run, accept/discard transitions.

### 6. Admin UX
- `/themes`: "Edit with AI" panel (prompt, base = active revision), runs list, run view with
  live progress (polling), diff, check report, cancel/accept/discard; accept links to the
  revision (preview/publish as in WP23). "Demo AI" badge on the fake provider.

### 7. Lint + e2e + docs
- theme-kit `client-visible` rule with a test.
- e2e `admin/theme-ai.spec.ts`: prompt → checks → accept → preview → publish → storefront shows
  the change → rollback restores it (fake provider, real builder).
- Decision record `docs/decisions/ai-theme-editing.md`, follow-ups ledger, runbook note.
