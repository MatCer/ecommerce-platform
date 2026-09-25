# WP23 Theme builder pipeline: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** tenants get their own theme source: fork the default theme, edit tokens, upload an
archive, reset to default. Every revision is built and checked in a disposable sandbox
container (A6), previewed on `preview-<n>--<shop>` behind an HMAC token (A21), and published
or rolled back atomically with an edge purge. Unreferenced artifacts are garbage-collected
(spec §7.5, §9.1, §9.6, §9.7, §12.3, §17 WP23, A6, A7, A21, A22, A26, A30;
`docs/decisions/runtime-contract.md` §9, `ai-edit-prompts.md`, follow-ups WP2/WP6/WP8).

**Architecture:**

```text
admin ──/admin/v1/themes/*──► api ──(job themes.build)──► worker ──POST /builds──► theme-builder
                                ▲                                                     │
                                └──── /internal/v1/themes/* (THEME_BUILDER_TOKEN) ◄───┤
                                                                                      │ Docker API
                                                           sandbox-proxy (policy) ◄───┘
                                                                  │ raw socket
                                              sandbox containers (one image, per step):
                                              lint, build  --network none --read-only …
                                              check, functional  internal net → caddy only
edge: preview-<n>--<shop> ─► GET /internal/v1/previews/resolve (edge token) ─► preview artifact
```

- `commerce::themes`: revisions (fork, tokens, upload, reset), source archives (tar.gz,
  validated per A6), builder callbacks (status, artifact, screenshots), preview tokens
  (HMAC-SHA256, tenant + revision + expiry ≤ 1 h), publish/rollback, GC, stuck-build expiry,
  per-tenant `ASTRO_KEY` (HMAC-derived, reproducible builds).
- `api`: `admin_themes.rs` (`/admin/v1/themes/*`), internal builder + preview routes,
  `BuilderService` extractor (distinct token, A7), `publish-artifacts --theme-source`.
- `worker`: `themes.build` (dispatch to the builder, retried), `themes.maintenance` cron
  (expire stuck builds, artifact GC).
- `apps/theme-builder`: `server.ts` (queue + pipeline), `docker.ts` (Engine API client),
  `sandbox.ts` (step runner inside the container), `proxy.ts` (policy socket proxy),
  Dockerfile with the locked theme `node_modules`, Playwright Chromium, Lighthouse, axe.
- `packages/theme-kit`: lint (platform-owned files untouched, tokens schema), measure/smoke
  (preview cookie, host-resolver rules, in-page fetches, `--preview`, screenshots).
- `apps/edge`: preview hosts (token exchange → partitioned cookie, never cached, `noindex`,
  `frame-ancestors <admin>`, checkout disabled), local artifact pruning.
- `apps/admin`: Themes page (revisions, check report, screenshots, preview iframe, publish,
  rollback, reset, upload/download, token editor).

## Global constraints

- Theme code only ever runs inside a sandbox container: non-root (1000:1000), read-only
  rootfs, tmpfs `/work` + `/tmp`, `CapDrop ALL`, `no-new-privileges`, memory/pids/CPU limits,
  wall-clock timeout, no credentials in env (only `ASTRO_KEY`, which ends up in the tenant's
  own bundle anyway). Build steps: `--network none`. Browser steps: an `internal` Docker
  network whose only peer is Caddy (the public entry point).
- Nothing holding the Docker socket runs theme code. The builder talks to a policy proxy
  that allows create/start/wait/logs/kill/remove/list for one image, one label, one volume,
  two networks and bounded limits; everything else is 403.
- Results the builder trusts come from container exit codes, platform-run measurements and
  the content-addressed artifact (verified), never from files theme code could write.
- Archives (A6): tar.gz, regular files + directories only, relative paths of plain
  segments, allowlisted top-level entries, ≤ 5,000 entries, expanded ≤ 50 MB (decompression
  capped while reading), upload ≤ 20 MB. Platform-owned files (`package.json`,
  `astro.config.mjs`, `tsconfig.json`) must equal the platform's; the build overlays them.
- Tokens are `theme.tokens.json`, schema-validated (A6); token edits never touch code.
- Preview tokens: HMAC-SHA256 over tenant + revision + expiry (≤ 1 h), verified by the API
  only (the secret never reaches the edge). Previews are never cached, `noindex`, framable
  only by the admin origin, no checkout handoff, no analytics counters/events.
- Publish/rollback: Admin role + fresh auth (A9), one transaction (`FOR UPDATE` on
  `theme_active`), audit entry, then edge purge. Only revisions whose gates passed (`ready`)
  or that were published before (`superseded`) can be published.
- Service tokens are distinct (A7): `INTERNAL_API_TOKEN` (edge), `EDGE_PURGE_TOKEN`,
  `THEME_BUILDER_TOKEN` (builder ↔ api/worker).
- New columns on RLS tables only; no new tenant table. Cross-tenant tests for revisions.
- Machine limits: one build at a time (builder concurrency 1), sandbox ≤ 2 CPUs, Lighthouse
  serial.

## Review focus

- Sandbox escape paths: proxy policy completeness (HostConfig keys, mounts, subpaths,
  network, user), symlinks/special files in step outputs read by the builder.
- Archive validation (symlink, hardlink, `..`, absolute, device, gzip bomb).
- Preview token binding (tenant, revision, expiry), cookie flags, cache bypass.
- Publish/rollback atomicity, status machine, stuck builds, GC never deleting referenced or
  retained artifacts (FK + advisory lock).

## Tasks

### 1. Migration + domain model (`commerce::themes`)
Files: `migrations/20261013230000_theme_builder.sql`, `crates/commerce/src/themes.rs`
(+ `themes/archive.rs`, `themes/preview.rs`), `crates/commerce/tests/themes.rs`.
- `theme_revisions`: `artifact_id` nullable, `source_key`, `change`
  (`default|fork|tokens|upload|reset|ai`), `prompt`, `status_changed_at`, CHECKs (artifact
  present unless draft/building/failed; source present unless origin default).
  `platform.theme_artifacts.source_key` (source of the default artifact).
- Tests first: archive validation cases, token replacement, preview token tamper/expiry/
  tenant/revision, astro key determinism, status transitions, publish/rollback, RLS.

### 2. API: admin + internal routes, CLI
Files: `crates/api/src/admin_themes.rs`, `internal.rs`, `auth.rs`, `lib.rs`, `cli.rs`,
`crates/platform/src/config.rs` (`ThemeConfig`: `THEME_SECRET`, `THEME_BUILDER_TOKEN`,
`THEME_BUILDER_URL`), `crates/api/tests/themes.rs`.
- Admin: list, detail (+ presigned screenshot URLs), fork, tokens, reset, upload (20 MB raw
  body), source download URL, preview URL, publish.
- Internal (builder token): build spec, source, status, artifact (tar, 60 MB), screenshots.
  Internal (edge token): `previews/resolve`.

### 3. Worker: dispatch + maintenance
`themes.build` → `POST {THEME_BUILDER_URL}/builds`; `themes.maintenance` hourly: builds
stuck > 30 min → failed; artifact GC (unreferenced theme artifacts older than 7 days; failed
revisions and ready revisions beyond the newest 5 per tenant lose their artifact first).

### 4. theme-kit
Lint: `locked-files` (platform files equal the reference dir), `tokens` (schema). measure/
smoke: `--cookie`, `THEME_KIT_CHROMIUM_ARGS`, in-page fetches, `--preview`, screenshots
(home/category/product × mobile/desktop).

### 5. apps/theme-builder + compose
Proxy policy (unit-tested), Docker client (log demux unit-tested), sandbox steps, pipeline,
Dockerfile, compose services `theme-builder` + `theme-sandbox-proxy`, volume `theme-work`,
internal network `theme-check` (caddy attached), Caddy TLS for `*.localhost`,
`scripts/smoke-images.sh` + CI image build.

### 6. Edge previews + artifact pruning
`preview-<n>--<shop>`: `?preview_token=` → 303 + `__Host-preview` (HttpOnly, Secure,
SameSite=None, Partitioned, 1 h); cookie → `GET /internal/v1/previews/resolve` (cached
60 s); no cache, `x-robots-tag`, `frame-ancestors <ADMIN_ORIGIN>`; `/_p/checkout/start` →
"checkout disabled in preview" page. Hourly prune of local artifacts unused for 7 days.

### 7. Admin Themes page (cs/sk/en) + OpenAPI/clients

### 8. Default theme source + `make theme-build`
Deterministic `default-theme.tar.gz` (sorted, mtime 0, `gzip -n`), fixed default
`ASTRO_KEY`; `publish-artifacts --theme-source`.

### 9. e2e `e2e/admin/themes.spec.ts`, docs, verification
Fork → tokens → checks → preview (token-bound) → publish → storefront changed → rollback;
malicious archives (symlink, `..`, oversized, extra dependency, foreign fetch, network during
build) and a JS-budget blow-out → rejected/failed with reasons. Decision record
`docs/decisions/theme-builder-sandbox.md`, runtime-contract §14, follow-ups.
