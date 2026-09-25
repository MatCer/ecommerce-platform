# Decision record: AI theme editing (WP24)

Status: accepted for M3. Spec: §12.3, §16 (M3 e2e), §17 WP24, amendments A6, A9, A21.
Builds on `ai-helpers.md` (WP22 provider, quota, metering), `theme-builder-sandbox.md` (WP23
gates) and `ai-edit-prompts.md` (WP2 feasibility study).
Code: `crates/platform/src/ai.rs` (`Client::converse`), `crates/commerce/src/themes/ai_edit/`
(runs, tools, loop, fake agent, system prompt), `crates/api/src/admin_themes.rs`
(`/admin/v1/themes/ai-runs*`, `/revisions/{id}/diff`), worker `themes.ai_edit`,
`apps/admin/src/components/ThemeAiEditor.tsx`, `e2e/admin/theme-ai.spec.ts`.

## 1. Shape

```text
admin ─POST /themes/ai-runs─► api (Admin, quota, one active run) ─job themes.ai_edit─► worker
worker: agent loop ── converse (Anthropic tool use | fake agent) ──► model
          │ tools on an in-memory copy of the base revision's source
          │ run_checks ─► revision (change = 'ai', ai_run_id) ─► themes.build ─► builder (WP23)
          │               ◄── poll status ready | failed + failures
          └─► ai_theme_runs: transcript, steps, counters, diff, report, summary, status
admin: progress (poll), diff, check report ─► accept ─► normal preview / publish (A9) ─► rollback
```

- **The loop runs in the worker, outside the sandbox (A6).** The model only ever sees file
  contents and check reports; the sandbox only ever sees a source archive. No theme code runs
  outside the WP23 sandbox, and the model has no network, shell or database tool.
- **Provider:** the WP22 client got `converse` (Messages API with client tools, append-only
  history, the system prompt + tools cached with `cache_control`, top-level automatic caching of
  the growing history, `output_config.effort = high`, no forced `tool_choice` and no thinking
  parameter, both of which Opus 5.5 rejects when set). `refusal` and `max_tokens` stops end the
  run (a cut `tool_use` input is never executed). Model: `AI_THEME_MODEL` (default
  `claude-opus-5-5`). Each turn may take minutes: a 10-minute per-attempt timeout.
- **Fake provider:** a scripted agent (`ai_edit/fake.rs`) answering raw API-shaped responses,
  so the same parser and loop run in tests, local stacks and e2e: list → read the home page →
  write a note quoting the request + `checks/ai-edit.spec.ts` → `run_checks` → summary. The
  admin shows the "Demo AI" badge.

## 2. Tools and trust boundary

Everything the model sends is untrusted input.

| Tool | Rules |
|---|---|
| `list_files {prefix}` | editable files only, ≤ 1000 entries |
| (all file tools) | binary or oversized existing files are opaque: they cannot be read, replaced or deleted |
| `read_file {path}` | path rules below; UTF-8 text without NUL only (fonts/images refused), ≤ 256 kB |
| `write_file {path, content}` | path rules; ≤ 256 kB, no NUL; `theme.tokens.json` must pass the token schema (A6); no file/directory clashes; the whole source stays within A6 limits (50 MB, 5000 files) |
| `delete_file {path}` | path rules |
| `run_checks {}` | refused unless the source changed and a `checks/*.spec.ts` was added or changed in this run; an unchanged source reuses the last result; otherwise builds a revision and waits for the gates |

Path rules: the archive validator's syntax (`archive::path_problem`: no absolute paths, `..`,
`.`, empty segments, backslashes, odd characters, ≤ 240 chars) plus the scope
`src/**`, `public/**`, `theme.tokens.json`, `checks/<name>.spec.ts`. Platform files
(`package.json`, `astro.config.mjs`, `tsconfig.json`), `README.md` and anything else are out of
reach. Symlinks cannot exist: the workspace is a path → bytes map and the archive writer emits
regular files only; the builder re-validates the unpacked tree anyway (WP23).

Unknown tools and malformed inputs (`deny_unknown_fields`) become `is_error` tool results the
model can react to. Every `tool_use` of a turn gets its result in one user message.

## 3. Limits (all enforced server-side, per run)

| Limit | Value | End state |
|---|---|---|
| Model turns | 25 | `failed: turn_limit` |
| Check runs | 4 (the first + 3 repairs) | `failed: repair_limit` after the 4th failed check |
| Tokens | 3 M (input incl. cache reads/writes + output) | `failed: budget` |
| Cost | USD 8 at the configured list prices | `failed: budget` |
| Wall clock | 45 min, builds included | `failed: timeout` |
| Tenant monthly quota (WP22) | checked at start (402) and before every call | `failed: ai_quota_exceeded` |
| Concurrency | one queued/running run per tenant (unique partial index) | `409 ai_run_in_progress` |
| Output per turn | 32k tokens, shrunk so the call's worst case (estimated input at ~3 characters per token, uncached, plus the output allowance) fits the remaining token and cost budget | `failed: ai_truncated` |

The budget is also checked after every response, before its tools run (a response that
overshoots ends the run, even a final answer). Cancellation (`POST …/cancel`) stops a queued
run at once and a running one within seconds: it is polled while the model answers (the call
is abandoned) and while a build runs, and the final status write turns a success into
`cancelled` if the cancellation raced the end. The deadline bounds model calls the same way.
Each response is saved before its tools run, and each check's report and diff snapshot are
saved when the check finishes, so a crash keeps the evidence. A job retry of a run that already started marks it
`interrupted` instead of resuming (never spends twice); the hourly `themes.maintenance` fails
runs `running` but silent for 60 minutes (a dead worker would otherwise block the tenant's
next run).

When the model stops with changes it never checked, one final check runs if a check run is
left; otherwise the run fails as `unverified`. A run succeeds only if the last check of the
**final** source passed.

## 4. Review, accept, publish

A run keeps: the transcript (the API history, append-only, thinking blocks included), the
steps (tool, path, ok, short detail) the admin shows as progress, the diff from the base
revision's source (unified, `similar`, ≤ 1 MB), the last check report, the model's summary,
counters and usage (`ai_usage.feature = 'theme_edit'`).

Every `run_checks` is a normal revision (`change = 'ai'`, `ai_run_id`, the prompt) so the
gates, previews and screenshots of WP23 apply unchanged. **An AI revision that was never
published can be published, or used as the base of a token edit or another AI run, only if it
is the final revision of an accepted run** (`409 ai_run_not_accepted`). Accepting needs Admin
and the revision still `ready`; publishing still needs Admin + a fresh login (A9) and moves the
pointer atomically with an edge purge; rollback is the WP23 one. Discarded runs keep their
revisions unpublishable.

## 5. Threat model additions

| Threat | Control |
|---|---|
| Prompt injection via the merchant's prompt, theme files or check output ("read ../../.env", "write package.json") | The model's capabilities are the five tools; the server validates every call (§2). Injected text can at most produce a theme change inside the contract, which the gates check and the staff reviews (diff + report) before accepting. |
| Model writes hostile theme code (exfiltration, crypto-mining, XSS) | Same as a hostile upload (WP23 threat model): lint, sandboxed build without network, browser checks on an internal network, CSP at the edge, restricted binding (A7). Functional checks written by the model run as untrusted code in the sandbox (exit code only). |
| Model forges a passing check | The gate result comes from the builder, not from the model; a check the model writes can only make its own change fail. |
| Budget abuse (a tenant or an injected prompt burning tokens) | Per-run limits (§3), monthly quota, one active run per tenant, Admin role. |
| Cross-tenant | `ai_theme_runs` has RLS + FORCE (tests); revisions are created in the run's tenant transaction; the worker reads the run under its tenant. |
| Unreviewed AI code going live | Publish gate (§4) + audit (`theme.ai_run_started/accepted/discarded/cancelled`, `theme.published`). |

Residual risks: an injected instruction producing a plausible, contract-conforming but
unwanted change (the review is the control); the model's quality (the WP2 study found 8/10
prompts pass within 3 repairs with full repository knowledge; a 25-turn loop with only the
contract will do worse on cross-cutting requests).

## 6. Data portability and GDPR (WP13b)

`ai_theme_runs` is included in the tenant export (every RLS-forced table, WP13b) with all
columns: prompts, transcripts, diffs and reports are the merchant's own data and hold no
secrets (the API key is a request header, never stored). Customer erasure does not touch it:
runs are written by staff about theme code and carry no shopper data (like `ai_proposals`);
`created_by` is a staff user id.

## 7. Deviations and follow-ups

- Tool scope includes `checks/<name>.spec.ts` (the WP23 functional-check hook the spec asks the
  agent to write); the spec's list names `theme.config.ts`, which A6 replaced with
  `theme.tokens.json`.
- "3 check-repair cycles" is read as 4 builds (the first check + 3 repairs).
- Budgets are constants, not configuration (no requirement to tune them per tenant yet).
- Runs are claimed from their own queue (`themes-ai`) by 2 dedicated loops per worker
  process, so a run waiting for its build never holds a slot of the default queue the build
  job needs. More concurrent runs than loops wait `queued` (cancellable at once).
- AI check revisions count towards the WP23 GC of "ready beyond the newest 5".
- The `client-visible` lint flags every `client:visible` in `.astro` files (over-approximation
  of "an island whose server render can be empty").
