# Runbook

Operations for the local stack and the checklist for going to production. Spec: §3.1 (local vs
prod), §13 (background processing), §15 (operations), §17 (M1 = local pilot acceptance).

## 1. Operations

| Task | Command |
|---|---|
| Start everything (build, wait until healthy) | `make up` |
| Dependencies only (run api/worker natively) | `make dev-infra` |
| Stop (volumes kept) / wipe data | `make down` / `docker compose down -v` |
| Status / logs of one service | `make ps` / `make logs s=api` |
| Migrations | applied by the one-shot `migrate` service on every `make up`; natively `make migrate` |
| Demo shop + published theme/checkout (idempotent) | `make seed` |
| Build + publish theme and checkout artifacts for every tenant | `make theme-build` |
| Superadmin CLI | `make admin args="<command>"` (in the api container) |

**Health.** `GET /healthz` = liveness (process up). `GET /readyz` = database + object storage;
`503` when either is down; `200` with `degraded` when only Meilisearch is down (search is a
degraded component, A27). The edge has `/_edge/healthz` on its internal port 8788.

**Logs.** `tracing` JSON lines on stdout. Every API response carries `x-request-id` (generated
unless the caller sent one); search logs for it. API request logs use route templates, and edge
error logs use safe path labels. For any historical logs retained from before WP25, search for
`/storefront/v1/orders/` and `/storefront/v1/withdrawals/` and purge matching entries; revoke
exposed order/withdrawal capabilities before granting log access. Do not export raw matches.

### Local pilot and verification

From a fresh checkout: `pnpm install && make up && make seed`. The demo has 60 products across
CZ/SK markets; browse `demo.localhost:8080` and use the `owner@lnen.example` magic link in
Mailpit for admin. Ports and local credentials are in `.env` (copied from `.env.example`); use
`make ps`, `make logs s=api`, and `make down` for routine operation. `make seed` publishes the
theme and checkout artifacts, so a separate `make theme-build` is needed only after source edits.

| Layer | Command | Needs |
|---|---|---|
| Rust unit + database integration, TS unit | `make test` | `make dev-infra` or `make up` |
| Search relevance integration | `make test-search` | Meilisearch + Postgres |
| rustfmt, Clippy, Biome, TS typecheck | `make lint` | installed Rust/TS dependencies |
| Full browser acceptance (four workers) | `make e2e` | `make up && make seed`, Playwright Chromium |
| Serial Lighthouse + JS + axe gate | `make perf` | the seeded stack, local HTTPS port |
| Built-image boot/degraded readiness | `scripts/smoke-images.sh` | freshly built images |

Playwright covers keyboard browsing, cart and checkout, including pickup-point and payment
selection, plus an admin product edit. For a manual keyboard pass, start on a product page and
Tab through variant, cart and checkout; verify visible focus, the Packeta dialog's Tab trap and
Escape, legal checkboxes, payment choice, and focus after the admin product save. Record any
visual or interaction defects for the UI owner; automated axe checks cannot prove the full WCAG
2.2 AA target. Local e2e contexts send a signed rate identity so concurrent browsers have
separate buckets. Auth and edge reject `E2E_RATE_SECRET` unless `APP_ENV=dev`; normal traffic
keeps the peer-IP rate limit.
CI runs the same seeded browser and performance gates on `main` and on PRs labelled
`acceptance` (`.github/workflows/ci.yml`).
The M3 criterion-to-test map is [`docs/acceptance/m3.md`](acceptance/m3.md).

## 2. Metrics

api and worker serve Prometheus `/metrics` on a separate internal port (api
`METRICS_BIND=0.0.0.0:9100`, worker `0.0.0.0:9101`). The ports are not published and Caddy does
not route them. Scrape locally:

```bash
docker compose exec caddy wget -qO- http://api:9100/metrics
docker compose exec caddy wget -qO- http://worker:9101/metrics
```

| Metric | Type, labels | Meaning |
|---|---|---|
| `http_requests_duration_seconds` | histogram; `method`, `route`, `status` | API latency per route template |
| `jobs_queue_depth` | gauge; `kind`, `status` | jobs per kind and status (`queued`, `running`, `dead`, ...) |
| `jobs_lag_seconds` | gauge; `kind` | age of the oldest due queued job |
| `outbox_lag_seconds` | gauge | age of the oldest undispatched outbox event |
| `job_duration_seconds` | histogram; `kind`, `outcome` | handler run time |

Suggested alerts:

| Alert | Condition |
|---|---|
| Outbox stuck | `outbox_lag_seconds > 60` for 5 min |
| Dead jobs | `increase(jobs_queue_depth{status="dead"}[15m]) > 0` |
| Job backlog | `jobs_lag_seconds > 300` for 5 min |
| 5xx rate | `sum(rate(http_requests_duration_seconds_count{status=~"5.."}[5m])) / sum(rate(http_requests_duration_seconds_count[5m])) > 0.01` |
| Latency | p95 of `http_requests_duration_seconds` > 500 ms for 10 min |

## 3. Dead jobs and sweepers

A job is `dead` when its attempts are exhausted, when the handler returned a permanent error,
or when its lease expired on the final attempt (the worker died mid-run). Dead jobs are kept
30 days, then deleted.

- **Admin:** `/platform/jobs` in the admin, visible only to users in `platform.platform_admins`:
  list, inspect the last error, requeue.
- **CLI:** `make admin args="dead-jobs"` lists them; `make admin args="requeue-job --id <id>"`
  requeues one. Fix the cause first, or it dies again.

Sweepers run in the worker cron every 15 min:

| Sweeper | Action |
|---|---|
| Media stuck in `processing` whose job died | asset → `failed` with a reason (re-upload) |
| Abandoned uploads | `pending` > 24 h → deleted; upload objects removed after completion |
| Carts | open, inactive 30 days, no order → deleted; used/expired checkout handoffs → deleted |
| Search indexes | indexes of locales no longer sold in any market → dropped |

## 4. Search reindex

Full rebuild for all tenants: `make admin args="reindex"`. One tenant: the admin's search page
or `POST /admin/v1/search/rebuild`. The rebuild builds a fresh index and swaps it in, so search
keeps serving the old index meanwhile. While Meilisearch is down or empty, storefront search
answers `503` and `/readyz` reports `degraded`; everything else works.

## 5. Webhooks

| Aspect | Behaviour |
|---|---|
| Signature | `X-Signature: t=<unix>,v1=<hex>`, HMAC-SHA256 with the subscription secret over `<t>.<raw body>`. Receivers verify with a constant-time compare and reject `t` older than 5 min. |
| Retries | exponential backoff for 24 h, then the delivery is `dead`; redeliver from the admin |
| Delivery | at least once: receivers must be idempotent |
| SSRF | the client connects to public IPs only (the resolved addresses are checked). `SAFE_FETCH_ALLOW_HOSTS` is a dev-only allowlist (e.g. the local `mocks` service) and is ignored (with a warning) unless `APP_ENV=dev`. |
| Secrets | stored encrypted (AES-256-GCM) with `SECRETS_KEY` (64 hex characters: `openssl rand -hex 32`). Rotating `SECRETS_KEY` means re-encrypting every secret: not supported yet (manual; see follow-ups). Losing it means every subscription needs a new secret. |

## 6. Analytics and privacy

- Before consent: only edge counters per route template and day, no identifiers, no cookies.
- Events are stored only when the server resolves an `analytics` consent for the session
  (never trusting a client flag).
- `events` is partitioned by month; retention 13 months (the nightly job drops older
  partitions). Rollups run hourly.
- The dashboard funnel counts consented sessions only, so it undercounts by the non-consent
  share.

### Ad-platform forwarders (WP20)

| Aspect | Behaviour |
|---|---|
| Platforms | Meta Conversions API (Graph v26.0), GA4 Measurement Protocol (EU endpoint), Google Ads via the Data Manager API (`events:ingest`, OAuth refresh token), Seznam SEM server-to-server (`sem.seznam.cz/rtgconv`). Admin → Settings → Ad tracking. |
| Consent | Captured only when the consent records grant `ads` to the visitor's subject; resolved again right before each send (a refusal on the customer account also stops purchases). Recording `ads = false` cancels the subject's waiting deliveries. |
| Data | `ad_deliveries` keeps the pseudonymous subject, order id, SKUs, the page (derived from the catalog, never a client-reported path) and the user agent (cleared when finished); email/phone are read from the order and hashed per vendor at send time. The log has no payloads. Finished deliveries are purged after 90 days. |
| Retries | Queue retries (`adtracking.deliver`, 12 attempts, backoff 5 s doubling to 1 h, about 2.5 h); 408/429/5xx retry, other 4xx fail at once. Dead jobs show in the superadmin jobs view. Per-tenant rate limit per platform in each worker process. |
| Credentials | Sealed with `SECRETS_KEY` (same key as webhooks); without it the admin answers 503 and deliveries wait. |
| Local | `AD_PLATFORMS_BASE_URL=http://mocks:4010/ads` (dev only) sends every vendor call to `apps/mocks`; `GET http://mocks.localhost:<port>/ads/<meta|ga4|google|sklik>/requests` shows what arrived, `PUT .../config {status, fail_times}` injects failures. |

## 6b. Email marketing (WP18)

| Topic | Operations |
|---|---|
| Sending | `marketing.campaign_batch` jobs: 500 recipients per batch, at most 500 marketing messages per tenant and minute (`email_settings` window). A stuck campaign shows as `sending` with a dead batch job in the superadmin job view; retrying the job is safe (one send per campaign and subscriber). |
| Re-checks | Status, `email_marketing` consent (address or linked customer, latest wins) and suppression are checked when a batch runs and again right before SMTP; a withdrawal between the two marks the message `failed` (`not_subscribed` / `no_consent`). Marketing mail with an uncertain SMTP outcome is never resent (A14). |
| Bounces | `POST /webhooks/ses` (HTTP Basic, `MAIL_EVENTS_SECRET`, unset = off). Permanent bounce: suppressed for every stream, subscriber `bounced`; complaint: suppressed for marketing, subscriber `complained` + consent withdrawal. Only the recipient of the referenced message is ever suppressed. Staff can remove a suppression in Admin → Emails (audited). |
| Links | Unsubscribe/preference/click links carry a per-recipient token (hashed at rest); click targets are HMAC-signed per campaign, so the redirect is never open. |

## 6c. Data import, export and GDPR requests (WP13b)

| Topic | Operations |
|---|---|
| CSV imports | Admin → Data → CSV import. `data.import` jobs (`analyze`, then `apply`); a run stuck in `analyzing`/`applying` has a dead job in the job view, and retrying it is safe (natural-key upserts; `apply` resumes after the last committed batch of 200). Files: 20 MB, 50 000 rows; deleted once applied. Historical orders go to `archived_orders` only and never trigger payments, stock, invoices, mail or events. |
| Subscriber evidence | Rows with `consent_at` + `consent_source` record an `email_marketing` grant at that time (source `import`) and subscribe the address only if it is the latest decision of the address and of a customer account with it, and the address is not suppressed. Rows without evidence stay `pending` (never marketable, never mailed). |
| Exports | `data.export` job writes `exports/<tenant>/<id>.zip` (private bucket): every RLS-forced tenant table as JSONL without `bytea` columns, password hashes, mail bodies or unsubscribe URLs. Download = 5-minute presigned URL, owner/admin with a login under 15 minutes, audited. |
| Access / erasure | Admin → Data → Export and privacy (owner/admin, fresh login, audited). Erasure is refused while an order of the person is unfinished, a withdrawal is open, a refund pending, or an import/export is running. It deletes the account, subscriber, analytics/ad rows; anonymizes orders, addresses, carts, withdrawals, archived orders and the mail log; pseudonymizes consent records; keeps invoices, credit notes and bank transactions. Files (labels, document sheets, all earlier exports, unapplied import CSVs) are removed by a retried `privacy.delete_objects` job. Backups keep the data until they rotate. |

## 6d. AI theme editing (WP24)

| What | How |
|---|---|
| Runs | Admin → Theme → "Edit with AI": status, turns/check runs, tool steps, diff, last check report. DB: `ai_theme_runs` (per tenant, RLS). |
| Stuck run | A run silent for 60 min is failed by the hourly `themes.maintenance` (`interrupted`); a worker restart mid-run does the same on the job retry. The merchant starts a new run. |
| Cost | `ai_usage` rows with `feature = 'theme_edit'` (Admin → Settings → AI). Per-run caps: 25 turns, 4 check runs, 3 M tokens, USD 8, 45 min. |
| Logs | worker `ai turn` (model, stop reason, tokens, cache reads, ms) and `ai theme run finished` (status, turns, checks, tokens, cost); never prompt or file content. |
| Manual smoke with a real key | `.env`: `ANTHROPIC_API_KEY=sk-ant-...`, `make up && make seed`, then in the admin run a few prompts from `docs/decisions/ai-edit-prompts.md` on the demo shop; check `cache_read` > 0 from the second turn on, the diff, the report, accept → preview → publish → rollback. |

Keep `ANTHROPIC_API_KEY` in the secret manager, never in source or browser configuration. Set
`AI_PLAN_QUOTAS` for each plan and review the monthly token and USD-micro cost counters in
Admin → Settings → AI. The theme agent has server-side per-run ceilings (table above); set a
monthly tenant override with `api admin set-ai-quota --tenant <slug> --tokens <n>` when needed.
Test refusals, truncated responses, repair exhaustion and cancellation with a real provider on
a disposable shop before enabling the feature for merchants. A successful fake-provider run
does not measure model quality or production cost.

## 7. Backups and restore

### Local (A29)

| Command | Effect |
|---|---|
| `make backup` | `backups/<UTC timestamp>/`: `app.dump` (`pg_dump -Fc` of database `app`, all schemas incl. `auth`), `minio/public` + `minio/private` (bucket mirrors), `manifest.txt` (git commit, checksum, object counts). Online; stack keeps running. |
| `make restore BACKUP=backups/<ts>` | Into a running stack (intended: fresh, `docker compose down -v && make up`): stops auth/api/worker/edge, recreates database `app` from the dump (owners, grants, RLS as dumped), mirrors the buckets back, starts the stack, enqueues a search rebuild for every tenant. |
| `DRILL_CONFIRM=destroy make backup-drill` | The scripted drill: counts rows of key tables, backs up, **wipes the volumes**, `make up`, restores, verifies row counts + a product page + an image variant + `/readyz`, prints PASS/FAIL and timings. Refuses the default project `ecommerce` unless `DRILL_ALLOW_DEFAULT=1`. |

Not included: Meilisearch (rebuilt from Postgres), Mailpit (test mail), the edge artifact cache
(refilled from the private bucket), roles (created by `init-roles.sh`; passwords from `.env`).
Keep `.env` with the backup: `BETTER_AUTH_SECRET` (encrypted JWT keys) and `SECRETS_KEY`
(webhook secrets) must match the data. A failed restore leaves `app` partial: fix and rerun.

### Production

| Layer | Mechanism |
|---|---|
| Postgres | PlanetScale Postgres PITR (provider) + nightly logical dump (`pg_dump -Fc`) to R2 |
| Dump storage | R2, EU jurisdiction, separate account and bucket, write-only credentials for the dump job, encrypted, retention 30 days |
| Object storage | R2 bucket versioning/replication for `public` and `private` |
| Search | not backed up; rebuild with `api admin reindex` |
| Drill | quarterly restore into a scratch environment, recorded (date, timings, checks) |

| Target | Expectation |
|---|---|
| RPO | ≤ 5 min with PITR; ≤ 24 h when falling back to the logical dump |
| RTO | ≤ 1 h for the database; ≤ 4 h for the full platform incl. search rebuild |

Who: the on-call engineer runs the restore; the platform owner decides on PITR target time and
communicates with merchants. Verify a restore like the drill: row counts of key tables against
the source, a storefront product page and an image, a staff login, `/readyz` `ok`, then watch
`outbox_lag_seconds` and dead jobs for an hour.

## 8. Incidents

Triage checklist:

1. `make ps` (prod: platform status), what was deployed recently, roll back if it correlates.
2. `/readyz` of the api: which dependency fails.
3. Logs by `x-request-id` of a failing request; error rate and latency in metrics.
4. Queue health: `outbox_lag_seconds`, `jobs_lag_seconds`, dead jobs (`/platform/jobs`).

| Failure | Symptom | Action |
|---|---|---|
| Postgres down | `/readyz` 503, API 5xx, worker idles | restore the DB service; worker and outbox resume by themselves |
| Meilisearch down | `/readyz` `degraded`, search 503, rest works | restart; `make admin args="reindex"` if the data was lost |
| MinIO/R2 down | `/readyz` 503, uploads and images fail | restore; stuck media assets are failed by the sweeper, re-upload |
| Mail provider down | emails stay `pending` or `uncertain` (A14), retried; no duplicates by design | fix credentials/provider; check `uncertain` ones manually before resending |
| Outbox stuck | `outbox_lag_seconds` grows | worker logs (dispatcher errors), worker running and leader lock held |
| Dead jobs | `jobs_queue_depth{status="dead"}` grows | read the error, fix, requeue (§3) |
| 429s from one IP | rate-limit hits in logs | abusive client or a merchant integration; block upstream or raise the limit deliberately |

Communicate early (affected merchants, scope, next update time). After the incident write a
short note: timeline, cause, impact, what changes; add follow-ups to `docs/follow-ups.md`.

## 9. Pre-launch checklist (real providers)

M1 is verified locally against mocks (spec §17). Before a real shop launches:

- [ ] **Stripe** (live test mode, then live): Connect onboarding of a merchant; direct charge
      with the application fee on the connected account; webhooks signature-verified; an
      `account.updated` capability loss disables the payment method. Verify: Stripe dashboard
      test payments + webhook delivery log.
- [ ] **Packeta + PPL**: sandbox labels and tracking; the real Packeta widget (`library.js` +
      callback) in the checkout CSP; the chosen pickup point verified against the API. Verify:
      test order end to end, label PDF, tracking status. WP12 specifics: the request shapes of
      `createPacket` / `packetLabelPdf` / `packetStatus`, the widget validation endpoint
      (`PACKETA_VALIDATE_URL`), the home-delivery carrier ids (CZ 106, SK 131), COD amounts
      (Packeta may require whole CZK) and the PPL CPL batch/label/tracking fields
      (`PPL_API_URL`, OAuth scope `myapi2`) were built from public docs and are only exercised
      against `apps/mocks`; carrier-side cancellation of a voided label is manual (portal).
      Real COD payout files (Packeta/PPL) still go through the generic CSV import.
- [ ] **AI theme editing**: the manual smoke of §6d with a real key (the loop only ran against
      the scripted fake agent); Opus 5.5 tool-use behaviour (no forced tool choice, preserved
      thinking blocks echoed unchanged) and the prompt cache hit rate.
- [ ] **SES**: domain verified with DKIM, SPF, DMARC; production access granted; a
      configuration set per stream publishes Bounce + Complaint to an SNS topic with an HTTPS
      subscription `https://ses:<MAIL_EVENTS_SECRET>@api.<domain>/webhooks/ses` (confirm the
      subscription by hand; confirmation URLs are never logged). Production boot refuses
      `MAIL_EVENTS_SECRET` until SNS signature, pinned certificate URL, authorized TopicArn,
      freshness and MessageId replay checks are implemented. Verify:
      mail-tester score, a bounce to `bounce@simulator.amazonses.com` shows in Admin → Emails →
      Suppressions; a campaign to Gmail shows the one-click "Unsubscribe" (RFC 8058).
- [ ] **QR payments**: SPAYD and PAY by square codes scanned with several CZ/SK banking apps
      (amount, IBAN, VS, message). Verify: screenshots per bank.
- [ ] **ČNB rates**: the daily fixing URL (`CNB_RATES_URL`), weekends/holidays (last fixing
      used), a supply dated today before ~14:30 waits for that day's fixing (the order shows
      `invoice_delayed`). Verify: a holiday date against the ČNB site.
- [ ] **Legal review** of the templates: terms, privacy, cookies, withdrawal form, complaints.
- [ ] **Accountant review**: VAT setup (OSS / distance-sales mode, tax categories) and the
      invoice templates (A17): **the Typst templates require accountant approval before real
      use** (`crates/commerce/src/documents/templates`, samples in `samples/`). Open points to
      confirm: COD cash rounding happens at collection after the dispatch invoice (outside the
      VAT base by default), credit notes use the original invoice's ČNB rate, prefixes `FV` /
      `DB`. Verify: sample invoices and credit notes signed off.
- [ ] **Regulatory status**: EET 2.0 (CZ) and SK e-invoicing (from Jan 2027) checked for the
      launch date.
- [ ] **Domain + TLS**: real domain on Cloudflare for SaaS, certificates issued, HSTS.
- [ ] **Secrets**: `SECRETS_KEY`, `BETTER_AUTH_SECRET`, service tokens freshly generated, none
      from `.env.example`; stored in the secret manager.
- [ ] **Prod mode**: `APP_ENV=prod` refuses the fake payment gateway (boot fails) and ignores
      `SAFE_FETCH_ALLOW_HOSTS` (warning in the log). Verify both.
- [ ] **Theme runtime**: run merchant-authored themes on managed Workers with verified CPU,
      memory and wall-clock enforcement, or isolate each runtime in a separate gVisor/Firecracker
      process with hard per-instance limits, kill-on-deadline and bounded admission. The local
      Miniflare entrypoint refuses `APP_ENV=prod` because it does not provide these guarantees;
      deploy a managed runtime before exposing themes to production traffic. Build sandboxes
      likewise need gVisor/Firecracker on separate hosts;
      plain Docker is a local-development implementation only.
- [ ] **AI provider**: provision a scoped real API key in the secret manager, set tenant monthly
      quotas and a cost budget, and run the §6d real-provider prompt table and review/publish
      workflow on a disposable tenant. Verify the feature is disabled without a key in prod.
- [ ] **Backups**: PITR enabled, nightly dump running, a restore drill done (§7).
- [ ] **Monitoring**: `/metrics` scraped, the alerts of §2 routed to on-call.
- [ ] **Rate limits** reviewed for the expected traffic (storefront, auth, admin).
