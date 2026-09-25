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
| Demo shop (idempotent) | `make seed` |
| Build + publish theme and checkout artifacts for every tenant | `make theme-build` |
| Superadmin CLI | `make admin args="<command>"` (in the api container) |

**Health.** `GET /healthz` = liveness (process up). `GET /readyz` = database + object storage;
`503` when either is down; `200` with `degraded` when only Meilisearch is down (search is a
degraded component, A27). The edge has `/_edge/healthz` on its internal port 8788.

**Logs.** `tracing` JSON lines on stdout. Every API response carries `x-request-id` (generated
unless the caller sent one); search logs for it. Logs contain ids, never PII, tokens or secrets.

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
- [ ] **SES**: domain verified with DKIM, SPF, DMARC; production access granted; bounce and
      complaint notifications feed the suppression list (today manual:
      `api admin suppress-email`). Verify: mail-tester score, a bounce to the simulator lands.
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
- [ ] **Backups**: PITR enabled, nightly dump running, a restore drill done (§7).
- [ ] **Monitoring**: `/metrics` scraped, the alerts of §2 routed to on-call.
- [ ] **Rate limits** reviewed for the expected traffic (storefront, auth, admin).
