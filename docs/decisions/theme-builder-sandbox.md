# Decision record: theme builder sandbox (WP23)

Status: accepted for M3. Spec: §7.5, §9.1, §9.6, §12.3, amendments A6, A7, A21, A22, A26, A30.
Code: `apps/theme-builder` (service, sandbox steps, Docker policy proxy), `crates/commerce/src/themes*`
(revisions, archives, preview tokens, GC), `crates/api/src/admin_themes.rs` + `internal.rs`,
`apps/edge` (preview hosts, local artifact GC), `apps/admin/src/pages/Themes.tsx`.

## 1. What runs where

```text
admin ─/admin/v1/themes/*─► api ──job themes.build──► worker ──POST /builds──► theme-builder
                             ▲                                                    │
                             └── /internal/v1/themes/* (THEME_BUILDER_TOKEN) ◄────┤ Docker API
                                                           theme-sandbox-proxy ◄──┘ (policy)
                                                                  │ /var/run/docker.sock
                         disposable sandbox containers (same image), one per step:
                         static, build        --network none
                         check, functional    --network <project>_theme-check (internal)
edge: preview-<n>--<shop> ─► GET /internal/v1/previews/resolve (edge token) ─► preview artifact
```

| Component | Holds | Runs theme code |
|---|---|---|
| api | DB, storage, `THEME_SECRET` (preview HMAC, ASTRO_KEY derivation) | no |
| worker | DB, `THEME_BUILDER_TOKEN` | no |
| theme-builder | `THEME_BUILDER_TOKEN`, the proxy's address, the work volume | no |
| theme-sandbox-proxy | the Docker socket (root in its container) | no |
| sandbox container | the job's source (read-only), its own output dir, `ASTRO_KEY` | **yes** |
| edge | `INTERNAL_API_TOKEN`, `EDGE_PURGE_TOKEN` | yes (Miniflare, as before) |

The builder never mounts the socket. Nothing that holds the socket runs theme code.

## 2. Threat model

Protected: the host (Docker socket = root), other tenants' sources/artifacts/data, platform
secrets (service tokens, DB/S3 credentials, `THEME_SECRET`), the live shops, the outside world
(a sandbox must not become an attack or exfiltration relay).

Attacker: a tenant (or an AI acting for one, WP24) who controls a theme source archive,
including code that runs during `astro build` (prerendered pages), Playwright functional checks
(`checks/*.spec.ts`) and JavaScript that runs in the gates' browser.

| Threat | Control |
|---|---|
| Host takeover through Docker | Builder talks to `theme-sandbox-proxy`: create/start/wait/logs/kill/remove/list only; `create` bodies pass an allowlist (`policy.ts`): the one image, the project label, `ReadonlyRootfs`, `CapDrop ALL`, `no-new-privileges`, numeric non-root user, memory = swap ≤ 4 GiB, CPUs ≤ 4, pids ≤ 1024, shm ≤ 1 GiB, tmpfs only at `/work`,`/tmp` with `noexec,nosuid,nodev`, mounts only of the work volume through `<job>/in` (read-only) or `<job>/out/<step>` subpaths, network `none` or the internal check network, `json-file` logs capped at 8 MB. Unknown keys anywhere (Binds, Devices, PidMode, CapAdd, SecurityOpt extras, NetworkingConfig, ...) are refused. Per-container calls re-inspect label + image; lists are forced to the label filter. No exec, attach, archive, image, volume or network endpoints. Tested (`apps/theme-builder/test/policy.test.ts`, 34 escape attempts). |
| Network egress / SSRF from build code | Build steps run with `--network none` (proved by the e2e probe: `fetch` from a prerendered page fails, `NETWORK-PROBE: blocked`). Browser steps join `theme-check`, an `internal: true` network whose only peer is Caddy: no route out of the host; reachable = what an anonymous visitor reaches through the public proxy. |
| Secrets in the sandbox | Env allowlist in the proxy refuses `*TOKEN`, `*SECRET`, `*PASSWORD`, `*_KEY` (except `ASTRO_KEY`, which the tenant's own bundle embeds anyway). No credentials are mounted. Preview checks get a 1-hour preview token for this revision only. |
| Tampering with gate results | The builder trusts: exit codes; the `static` step's report (no theme code runs there: lint is static, content collections — the only thing `astro check` would execute — are forbidden by the lint); the `check` step's report (platform code only; theme JS runs in the browser, which cannot write files; the calls-per-page number is read from a separate navigation's response headers, never through page JavaScript) — and only if the step completed, every page was measured and every screenshot exists; the artifact only after `verifyArtifact` recomputes its content address. Each step has its own output dir; untrusted functional checks get no writable mount at all. |
| Reading builder files through outputs | Step outputs are read with `lstat` on every path component (`safeRead`); the whole artifact tree is walked without following anything (directories and regular files only, ≤ 52 MB, ≤ 20k files) before any file is opened, so a FIFO or a huge file cannot block or exhaust the builder; then `verifyArtifact`. The artifact is re-validated by the API (`read_artifact_tar`: regular files at artifact paths only) and again by the edge (content address). |
| Hostile archives (A6) | API: `.tar.gz` only, ≤ 20 MB upload, ≤ 50 MB expanded (capped while decompressing), ≤ 5000 entries; regular files only (symlinks, hard links, devices, FIFOs refused); relative plain-segment paths (no `/…`, `..`, `.`, empty, backslash); allowlisted top level (`src/`, `public/`, `checks/`, `theme.tokens.json`, `README.md`, platform files); schema-valid tokens. Every problem is listed. The sandbox re-checks the unpacked tree. |
| Dependency or config changes | `package.json` (dependencies and scripts), `astro.config.mjs` and `tsconfig.json` must equal the platform's (lint `locked-deps`/`locked-files`); the build overlays the platform copies anyway. `node_modules` are the image's (read-only); the sandbox links them into a writable dir so caches work but packages cannot change. |
| Resource exhaustion | Per-step memory, CPU, pids, tmpfs size, wall-clock timeouts (240/300/600/240 s) with SIGKILL; the job volume is a size- and inode-capped tmpfs (1 GB, 200k inodes), so outputs cannot fill the host disk; Docker logs are capped (8 MB) and the builder reads at most 12 MB of them, keeping the last 1 MB; one build at a time; at most 3 pending builds per tenant (`409 builds_in_progress`); stuck builds fail after 30 min. |
| Preview abuse (A21) | Previews need an HMAC-SHA256 token bound to tenant + revision + expiry (≤ 1 h), verified by the API only (the secret never reaches the edge). The token travels once in the link, then as a host-only `__Host-preview` cookie (`HttpOnly; Secure; SameSite=None; Partitioned`, so it works in the admin iframe with third-party cookies blocked). Every preview response is `no-store` + `noindex`; theme pages are framable by the admin origin only (`frame-ancestors`); the admin iframe is `sandbox="allow-scripts allow-same-origin allow-forms"` and only ever loads the preview origin. No checkout handoff (notice page), no events, no page counters. |
| Cross-tenant | Revisions are RLS-scoped (tests); builder callbacks name the tenant (`X-Tenant-Id`) and the revision is looked up under that tenant's RLS; preview tokens are verified against the host's tenant; theme workers stay one isolate per tenant (WP2). |

Residual risks (accepted, documented):
1. A kernel/container escape from a sandbox would reach the host; mitigated by seccomp default,
   no capabilities, non-root, read-only root. Production should run sandboxes under gVisor or a
   microVM runtime (Firecracker) and on separate hosts: `Runtime` is not allowed by the proxy yet
   (add it with a fixed value when the host has it).
2. The proxy runs as root inside its container to open the socket; it has no other inputs than
   the builder's HTTP calls on the compose network.
3. The `astro check` result is only a quality gate: a theme that forged it would only deceive
   itself (security gates do not depend on it).
4. The check network reaches Caddy, i.e. the public surface (in dev this includes Mailpit/MinIO
   UIs); prod equivalent is the public internet surface of the platform.
5. Chromium runs with `--no-sandbox` inside the container (Playwright default); theme JS is
   also constrained by the edge CSP.

## 3. Pipeline and gates

| Step | Container | Gate |
|---|---|---|
| `static` | none | unpack + tree re-check; contract lint (foreign fetch, URL imports, eval, inline scripts/handlers, `set:html` outside sanitized HTML/JSON-LD, cookies, `cloudflare:*`, required routes, tokens schema, locked platform files, no content collections); `astro check` (skipped for token-only revisions) |
| `build` | none | `astro build` with the tenant's `ASTRO_KEY` (HMAC of `THEME_SECRET`, reproducible: same source → same artifact id), `theme-kit pack`, then `verifyArtifact` in the builder and registration by the API (`checking`) |
| `check` | internal | on `https://preview-<n>--<shop>` via Caddy (h2, `--host-resolver-rules=MAP *.localhost caddy`): `theme-kit measure` (Lighthouse mobile, median of 3, skipped for token-only; JS gzip first load + full scroll, with and without the consented RUM visit; calls per page ≤ 10; axe serious/critical = 0; CSP violations; third-party origins) on home, the fullest category and its first product; `theme-kit smoke --preview` (browse → category → product → add to cart → handoff reaches "Checkout is disabled in preview"; skipped for token-only); merchant screenshots (home/category/product × mobile/desktop, private bucket) |
| `functional` | internal | the revision's `checks/*.spec.ts` (Playwright, `baseURL` = preview) — the hook WP24's AI loop writes one per change; untrusted, exit code only |

The token fast path is only granted on a base revision that passed the full gates (`ready`/`published`/`superseded`) or follows the platform default; otherwise `409 base_not_validated`.

`ready` needs every gate; failures are stored as readable reasons in `checks.failures`
(`lint: src/x.astro:2 foreign-fetch: …`, `budget /p/x: JS 58.6 kB gz > 30`, build log tail).
Measured on the local stack: a full pipeline takes ~45 s (static 4.5 s, build 2.4 s, check
31 s with three Lighthouse runs per page), a token-only one ~15 s.

## 4. Revisions, publish, rollback, reset, GC

- Statuses: `draft → building → checking → ready | failed`; `ready → published → superseded`;
  rollback = publishing a `superseded` revision again. Publish/rollback: Admin + fresh auth
  (A9), one transaction with `FOR UPDATE` on `theme_active`, audit (`theme.published` /
  `theme.rolled_back`), then an edge purge of the tenant.
- Fork = the default theme's source (`make theme-build` publishes it with the default artifact,
  deterministic tar.gz) copied to `theme-sources/<tenant>/<revision>.tar.gz`. Token edit = the
  base revision's source with a new `theme.tokens.json`. Reset = the latest default source +
  the active tokens. A tenant that publishes a custom revision stops following new defaults
  (A30); rolling back to a default revision follows the default again.
- Artifact GC (worker `themes.maintenance`, hourly): failed revisions and `ready` revisions
  beyond the newest 5 release their artifact after 7 days; theme artifacts no revision or
  channel references are then deleted (objects listed by their manifest; the row is deleted
  under the registration advisory lock; the foreign key from `theme_revisions` — checked across
  tenants regardless of RLS — keeps every referenced one). Published and superseded revisions
  keep their artifacts (rollback, retained `/_astro/*`, A22). Candidates exclude every artifact
  any tenant references (collected tenant by tenant), so referenced ones never starve the batch.
  The edge prunes local copies not needed for 7 days and not running; the decision is taken
  synchronously after the last await, loads wait for an in-progress deletion and then download
  (and verify) afresh.

## 5. Deviations and requirements

- Docker Engine ≥ 26 (API 1.45) for volume subpath mounts; the builder speaks API 1.47.
- Browser steps use an internal network instead of `--network none` (they must reach the
  preview); the only peer is the public proxy.
- One image for builder, proxy and sandbox (fewer moving parts; the proxy pins it).
- Source archives are `.tar.gz` only.
