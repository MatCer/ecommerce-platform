# Follow-ups ledger (gaps reported by merged WPs, with owning WP)

| From | Gap | Owner |
|---|---|---|
| WP0 | ~~CI does not smoke-test the auth image~~: CI now boots it against disposable Postgres and checks `/healthz` + clean shutdown in `scripts/smoke-images.sh`. | done (WP15) |
| WP1 | ~~Manual smoke paths lack browser coverage~~: staff, catalog, search, checkout and cross-tenant checks are in `e2e/`; smoke scripts remain optional diagnostics. | done (WP15) |
| WP2 | ~~Artifact GC (private bucket `artifacts/` + edge cache volume)~~ done in WP23 (`themes.maintenance`, edge `pruneArtifacts`) | done |
| WP2 | Size chart, dispatch cutoff/holidays on the PDP and a curated font library: not in the M1 spec (§9.2 asks for a delivery estimate, which the PDP shows); each needs a new merchant data model (size tables per product type, a dispatch calendar, self-hosted font assets the builder can subset). Font tokens stay free text but allowlist-validated (A6), so nothing unsafe reaches CSS. | later (content model) |
| WP4 | A sale change recomputes every priced variant of the tenant; narrow it for very large catalogs | later (perf) |
| WP4 | Price history returns the full timeline per variant (no pagination) | later |
| WP5 | The staff table has no invitation-accepted state: §5.3 defines no invitation lifecycle, and acceptance happens in the auth service (its own schema and DB role), so the API would need a first-sign-in marker on `staff_members` written from the staff extractor. | later (staff UX) |
| WP5 | Deterministic >15 min reauth / tenant-switch-in-flight browser race remains. API integration tests cover stale JWT denial and tenant membership/RLS; a browser race needs a dev auth clock plus request barrier. | deferred: identity/security owner, before launch |
| WP5 | ~~Quick e2e reruns share the auth IP bucket~~: signed per-context local identities separate buckets; the secret is refused outside `APP_ENV=dev`. | done (WP15) |
| WP6 | ~~Artifact builds are not reproducible~~ done in WP23: fixed `ASTRO_KEY` for the default artifact, per-tenant HMAC-derived key for tenant builds | done |
| WP6 | Moved: cart creation and handoff still lack `Idempotency-Key` replay after a lost response. One order per cart and idempotent placement prevent duplicate charges; retrying a lost handoff needs a new cart. This is recovery work beyond the local happy-path gate. | later (checkout reliability) |
| WP8 | `checkout.<host>/withdraw` is a placeholder page (the withdrawal flow, A19) | WP12 |
| WP8 | ~~Payment/carrier marks in `/shop` are generic catalog text~~: `trust.payment_methods`/`trust.carriers` list what checkout offers in the market (text marks, no logos). Still open: the checkout links the Czech legal slugs `/pages/obchodni-podminky`, `/pages/odstoupeni-od-smlouvy` for every locale | WP12 |
| WP8 | PDP JS headroom is 2.5 kB (27.5 kB gz first visit, 28.0 kB with every consent + the RUM sample); keep islands lean | WP8 successors / WP23 gates |
| WP9/WP18/WP25 | Local SES/SNS fixture ingestion is Basic-authenticated. Production boot refuses `MAIL_EVENTS_SECRET` until signature, certificate URL, authorized topic, freshness and replay checks exist. Confirmation URLs are not logged. | deferred: mail integration owner, before SES launch |
| WP10 | Stripe and bank transfer are configurable but not offered at checkout (no adapter); the order email has a bank-transfer placeholder | WP11 |
| WP10 | COD cash rounding is not applied at placement (the tender is unknown until collection, A16) | WP11 |
| WP10 | The Packeta widget key is a platform setting (`PACKETA_API_KEY`); per-tenant carrier credentials and verifying the chosen point against the Packeta API | WP12 |
| WP10 | Moved: real Packeta `library.js` and callback require carrier credentials/sandbox; the local suite tests the mock and keyboard path. | pre-launch checklist (§9) |
| WP10 | No cancellation email when an unpaid order expires; refunds of late/duplicate payments are done by hand and then marked settled in the exceptions queue (WP11), no refund UI yet | WP12 |
| WP10 | Payment timeouts are one global scan per minute (orders expire up to ~1.5 min late); per-tenant order numbers serialize placements of one tenant on the counter row | later (perf) |
| WP14 | Meilisearch is not backed up; a restore rebuilds every tenant's index (`api admin reindex`), search is degraded until it finishes | accepted (A27) |
| WP14 | Mailpit (local test mail) is not backed up | accepted (local only) |
| WP14 | Local backups mirror buckets to files: object metadata is dropped (Content-Type restored from the extension, public Cache-Control re-applied); prod relies on R2 versioning/replication instead | accepted (local only) |
| WP14 | Moved: PITR, off-account R2 dump automation, versioning and quarterly drill require a production account; the local backup/restore drill is available. | pre-launch infrastructure |
| WP14 | `SECRETS_KEY` rotation (re-encrypt stored secrets) is a manual, unsupported operation | later |
| WP14 | A failed local restore leaves database `app` partial (DROP/CREATE DATABASE cannot be transactional); rerun it | accepted (local only) |
| WP14 | Storefront rate limits are in-process buckets per API replica (N replicas allow N× the rate) | later (scale-out / CDN rate limiting) |
| WP14 | Dashboard days are UTC, not the merchant's time zone; revenue is placed, non-cancelled order totals (refunds not netted until WP11/WP12 publish them) | later / WP12 |
| WP14 | Top searches come from consented sessions only (A20); zero-result searches are still the API-side log of all visitors (edge-cache misses only, per locale, accepted in WP7) | later |
| WP14 | Refund analytics: WP11 publishes `order.refunded` (`refunded_minor`, `full`) when an order's payment becomes (partially) refunded; netting it in the dashboard is left | WP12 |
| WP14 | Every webhook-type outbox event gets a fan-out job even for tenants without subscriptions | later (perf) |
| WP13a | Feed import applies product by product and feeds render in memory per market; batch/stream for 100k-item catalogs | later (perf) |
| WP13a | Legal templates are starting points; every shop needs a lawyer's review (the admin says so) | pre-launch |
| WP13a | ~~Orders/customers CSV import, tenant data export, customer access/erasure (A29)~~ done in WP13b | done |
| WP17 | The hourly rollup recomputes the tenant's co-purchases, scores and customer affinity in full (stats for the last 2 days; the nightly run recomputes all 400 retained days so late cancellations leave every result); fine for demo-sized shops, narrow it to changed orders for large catalogs/order books | later (perf) |
| WP11 | `payments::refund(attempt)` (Stripe with `refund_application_fee` and the refund id as idempotency key, bank/COD recorded) and `payments::retry_refund` have no admin screen yet; a Stripe refund whose outcome is unknown stays `pending` until retried or reconciled by `refund.*` webhooks; WP12 wires them into returns/withdrawals and a pending-refund list | WP12 |
| WP11 | COD `delivered` is set by hand (or the carrier CSV stub `POST /admin/v1/cod-reports`); carrier tracking and real COD payout imports (Packeta/PPL) | WP12 |
| WP11 | The QR code in emails is inline SVG: Gmail and some clients do not render it (the text instructions always are); a CID PNG attachment needs attachments in the mail pipeline | WP14 / pre-launch |
| WP11 | Moved: real Stripe test-mode onboarding/Payment Element/webhooks and bank-app QR scans require external accounts/apps; local mocks and QR vectors are covered. | pre-launch checklist (§9) |
| WP11 | `platform.provider_events` keeps payloads indefinitely (PaymentIntent objects may hold billing details); add a retention rule to WP14's `ops.sweep` (e.g. drop payloads of processed events after 90 days) | later (ops) |
| WP11 | Fio tokens share WP14's single `SECRETS_KEY` (no key id in the ciphertext); no rotation tooling | later |
| WP11 | Payment reminder and email due dates are the UTC date of the deadline | later |
| WP20 | Seznam SEM S2S attribution normally needs the `sid`/`udid` cookies of its `sul.js` browser script, which the platform does not load (no third-party scripts); matching relies on hashed email/phone. Capturing the `sznaiid` click id at the edge would help | later |
| WP20 | Google Ads gets purchases only (Data Manager API offline conversions / enhanced conversions for leads by hashed email/phone); no gclid capture, no refund retractions | later |
| WP20 | Meta receives no `client_ip_address` (IPs are only stored hashed, §14) and no `fbp`/`fbc` (no Meta pixel); match quality relies on hashed email/phone/external_id + user agent | accepted |
| WP20 | Ad-platform rate limits and Google access-token caches are per worker process | later (scale-out) |
| WP22 | Translations cover names, descriptions, SEO, page blocks and menu labels; option/value names, parameter texts and image alt texts are not translated by AI yet | later |
| WP22 | One entity per proposal: no "translate every product missing sk" batch job (bulk plans cover non-text fields) | later |
| WP22 | The AI quota is a soft limit (concurrent calls may overshoot by one call); no superadmin UI for quotas (CLI `set-ai-quota`) | later (only if it matters) |
| WP22 | Old `ai_proposals` / `ai_bulk_plans` rows are never purged | later (ops) |
| WP23/WP25 | Build sandboxes run under runc on the app host; prod requires gVisor/Firecracker on dedicated build hosts (proxy-pinned `Runtime`). Functional checks now have a separate preview-only internal network in Compose. | deferred: platform infrastructure owner, before theme launch |
| WP23 | Builder queue is in memory (concurrency 1): a builder restart drops queued builds; they fail after 30 min and must be re-created (no retry button yet) | later |
| WP23 | Theme source archives and screenshots of old revisions are never deleted (small; artifacts are GC'd) | later (ops) |
| WP23 | ~~`client:visible` lint~~ done in WP24 (`client-visible`, every use in `.astro`); still open: stale-preload lint, image-bytes budget, desktop CLS run in the gates | later |
| WP23 | ~~No diff view between revisions~~ done in WP24 (`GET /themes/revisions/{id}/diff`, "Show changes" in the report); no "rebuild" action (a new token edit/upload/reset creates a new revision) | later |
| WP20/WP22 | ~~Full four-worker e2e handoff/auth collisions~~: signed per-context local rate identities isolate parallel browsers; checkout and account journeys remain in the full suite. | done (WP15) |
| WP12 | Packeta/PPL are built from public docs and exercised only against `apps/mocks` (request shapes, home-delivery carrier ids, COD rounding rules); carrier-side cancellation of a voided label and an unanswered shipment announcement (`label_in_progress`) are reconciled by hand in the carrier portal | pre-launch checklist |
| WP12 | Moved: carrier-specific COD payout file formats need real carrier sample files; the generic CSV endpoint covers local pilot reconciliation. | pre-launch carrier integration |
| WP12 | Invoice/credit-note Typst templates and the COD cash-rounding treatment (rounding at collection, after the dispatch invoice, outside the VAT base) need accountant approval before real use | pre-launch |
| WP12 | Presigned PDF downloads are named by their key (`FV…pdf`); no `Content-Disposition` override (object_store's signer lacks response-header params) | later |
| WP12 | ~~Payment/carrier marks in the theme footer are generic strings~~: the footer and trust row show the market's enabled payment and shipping methods by name; licensed provider logos are not used. | done (WP15) |
| WP12 | ~~Parallel checkout specs share a storefront IP bucket~~: signed per-context local identities keep the production limiter intact. | done (WP15) |
| WP18 | No open tracking at all (privacy default); the optional consented tracking pixel of §11.5 is not built | later (only if merchants ask) |
| WP18 | Marketing message bodies stay in `email_messages` indefinitely (one row per recipient); add a retention rule to `ops.sweep` (e.g. drop bodies of final marketing mail after 30 days) | later (ops) |
| WP18 | The marketing rate is one platform constant (500 messages per tenant and minute) and limits how fast campaign messages are queued, not SMTP itself (a backlog after an outage drains faster); per-tenant quotas, a delivery-time rate limit | later |
| WP18 | ~~Subscriber import (CSV)~~ done in WP13b; the AI copy assist per segment (§11.5) | M3 |
| WP18 | Segment purchase conditions use placed orders of the same address or linked customer; refunds are not netted in `total_spent` | later |
| WP24/WP25 | The agent loop is acceptance-tested with the scripted fake. Run the real `ANTHROPIC_API_KEY` smoke (runbook §6d) and record pass rate, turns, repairs, tokens and cost per prompt in `ai-edit-prompts.md`. | deferred: AI owner, before enabling real provider |
| WP25 | Miniflare render timeout evicts the tenant instance; admission and the local edge container are bounded. Per-instance OS CPU/memory/process enforcement is unavailable, so this entrypoint refuses `APP_ENV=prod`. | deferred: platform infrastructure owner, before theme launch |
| WP25 | Existing deployment logs from before WP25 may contain order/withdrawal capabilities. Locate and purge matching retained entries and revoke affected capabilities; the new log-capture regression proves current paths are safe. | deferred: deployment operations owner, before pilot access |
| WP24 | AI runs have their own queue with 2 loops per worker process (constant); more concurrent runs wait queued. Make it configurable / a separate worker when many shops edit at once | later (scale-out) |
| WP24 | A refused or cut-off (`max_tokens`) model response is metered but its content is not kept in the transcript | later |
| WP24 | `ai_theme_runs` transcripts (the full API history, can be MBs) are kept indefinitely; add a retention rule to `ops.sweep` | later (ops) |
| WP24 | No "retry" or "continue with feedback" on a failed/finished run (the merchant starts a new run with a refined prompt) | later |
| WP24 | The agent cannot add storefront message-catalog keys (platform-owned): new copy is written in the shop's locale directly in markup, so multi-locale shops get one language for AI-added strings | later (theme-owned catalog overrides) |
| WP13b | `audit_log` is append-only: diffs written by earlier staff edits may still quote an erased customer's email or address; erasure cannot scrub them (the erasure entry itself holds counts only) | later (privacy) |
| WP13b | Backups (`make backup`) keep erased data until they rotate out; document the retention in the privacy policy template or re-apply erasures after a restore | pre-launch |
| WP13b | Data exports have no automatic expiry (a new erasure deletes all of them); add a retention sweep (e.g. 7 days) to `ops.sweep` | later (ops) |
| WP13b | The export's assets manifest lists public image variants only; merchant-uploaded originals and invoice PDFs (private bucket) are not in the zip | later |
| WP13b | ~~M2 flow/watch customer data in `privacy::access`/`erase`~~: WP19 added watches, runs, restore/unsubscribe tokens and queued mail cleanup; WP21 acceptance exercises access and erasure of a confirmed watch through the admin UI. | done (WP21 verified) |
| WP13b | Re-importing an old CSV after an erasure brings the person back; the import cannot know (no tombstones by design) | later (privacy) |
| WP13b | ~~No customer list in the admin~~: Orders → Customers (`GET /admin/v1/customers`, search by email/name, cursor pages) links to the customer's orders (`/orders?customer_id=`). No customer detail page/edit yet. | done (WP15) |
| WP13b | The import UI maps columns only when a run is created; re-checking with a new mapping is API-only (`analyze` takes `mapping`) | later |
| WP16 | ~~Consent-gated review invites after delivery~~: WP19 issues tokens and mails links after the dev clock advances; `e2e/checkout/reviews.spec.ts` verifies the delivered-order journey through moderation and JSON-LD. | done (WP21 verified) |
| WP16 | ~~GDPR erasure/export must cover `reviews` and `review_tokens`~~ done in WP13b | done |
| WP16 | The product page shows the 20 newest published reviews (summary and JSON-LD cover all); no pagination, sorting or filtering by rating yet | later |
| WP16 | Tokens are issued per order, not per returned/withdrawn line: a line returned after delivery can still be reviewed while its token lives | later |
| WP16 | ~~Old seeded review disclosure survives reruns~~: `make seed` refreshes only the pre-WP16 stock text and preserves merchant edits. | done (WP15) |
| WP26 | The checkout handoff cookie (`__Secure-hf-<digest prefix>`, SHA-256 of the token, `Domain=<shop_host>`) can still be tossed by a same-site host under the shop domain (a descendant of a custom domain, or a sibling where the platform domain is not a public suffix); the old Sec-Fetch-Site check had the same gap | accepted: same-site hosts are trusted |
| WP26 | Product decision: move checkout from `checkout.<shop_host>` to a path on the storefront domain (e.g. `<shop_host>/checkouts/<token>`, like Shopify). Removes the cross-host handoff (token, 303, handoff cookie), the checkout DNS/certificate per custom domain and the Firefox `Sec-Fetch-Site` class of bugs. Prerequisite: theme JS must no longer share an origin with checkout, i.e. themes lose arbitrary client JS on the storefront origin (sandboxed/allowlisted islands, strict CSP) or checkout gets equivalent isolation another way; the separate origin is currently the security boundary between AI/merchant-editable theme code and payment/PII. Needs its own architecture WP (security review, edge routing, cookies, CSP). | planned (checkout architecture) |
| WP26 | Merchant content on the order confirmation / status page (`checkout.<shop>/o/<token>`): the page stays platform-owned (the capability URL shows PII and allows payment retry and withdrawal, so theme JS must never reach it), but merchants should be able to add safe, data-only blocks such as a thank-you text, links, a next-order coupon or product recommendations (Shopify: Checkout UI extensions). Admin-configured and rendered by the checkout; no merchant or AI code. | planned (checkout content) |
| Conversion theme | Checkout and thank-you upsell from the Conversion UI Figma (frames 19:*, 23:3641+, 52:8143+), in `apps/checkout` | next |
| Conversion theme | Add-on picker after add-to-cart (offer related add-ons to the item just added) | next |
| Conversion theme | Warranty, add-on and discount popups; the owner supplies Figma screenshots (35:3655 and 36:4033 never loaded) | next |
| Conversion theme | Shop-wide review cards on the home page: aggregate the existing product reviews into a storefront `/shop` field | next |
| Conversion theme | Brand logos: needs a brand entity (name, logo asset) on products, plus a home strip | next |
| Conversion theme | Volume price tiers ("buy more and save"), product benefit bullets and feature blocks: needs a pricing-tier model and structured product content | next |
| Conversion theme | Footer contacts: phone, email and address already exist in the legal entity (admin) but the storefront SDK does not expose them; social links have no model yet | next |
| Conversion theme | Instagram feed as an optional module | later |
| Conversion theme | Real social proof: units sold on the PDP (from the bestseller counts, hidden under a minimum) and a countdown tied to the sale's real end date (no per-visitor reset). No live "N people viewing" | next |
| Conversion theme | External reviews (Google, Trustpilot, Heureka): a merchant-entered badge (rating, count, as-of date) plus a few hand-picked quotes labelled as selected and unverified, linking to the profile. Platform policy forbids storing Google API reviews; native product reviews stay the verified source | next |
| Conversion theme | New CMS blocks for landing pages: stats (number + label) and a logo strip. No pricing table | next |
