# WP18 Email marketing: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** newsletter subscribers with double opt-in and consent evidence, allowlisted segments
compiled to parameterized SQL, campaigns with a block editor (incl. per-recipient personalized
products), throttled idempotent batch sending with send-time re-checks, click tracking, RFC 8058
one-click unsubscribe + a preference page on the checkout origin, SES/SNS-shaped
bounce/complaint ingestion, admin views of sent mail and suppressions, tenant logo + editable
subject/intro texts (spec §7.6, §11.4, §11.5, A12, A14, A20, A21; follow-ups of WP9/WP17).

## Architecture

- Migration `20261015000000_email_marketing.sql` (all tenant tables RLS + FORCE + grants):
  - `subscribers(email, status pending|subscribed|unsubscribed|bounced|complained, locale,
    market_id, customer_id null, confirm_token_hash, confirm_expires_at, evidence: requested_at,
    request_ip_hash, confirmed_at, confirm_ip_hash, text_version, unsubscribed_at)`.
  - `segments(name, rules jsonb)`, `campaigns(name, segment_id, content jsonb {locale →
    {subject, preheader, blocks}}, status draft|scheduled|sending|sent|cancelled,
    scheduled_at, link_key bytea(32))`, `campaign_sends(campaign_id, subscriber_id, status
    sent|skipped, skip_reason, message_id, token_hash, clicked_at, click_count,
    unsubscribed_at, bounced_at, complained_at)` unique `(campaign_id, subscriber_id)`.
  - `email_messages` + `list_unsubscribe`, `subscriber_id`; `email_settings(logo_asset_id,
    throttle window)`, `email_template_texts(template, locale, subject, intro)`.
  - `platform.email_message_tenant(uuid)` (SECURITY DEFINER) for bounce routing.
- `commerce::marketing`:
  - `subscribers`: subscribe (always 202, no enumeration; per-address resend cooldown,
    per-IP hourly cap), confirm (POST, token hashed, 48 h expiry, consent record
    `email_marketing` granted + evidence), unsubscribe by send token / admin / consent
    withdrawal, customer linking on verified email, admin list + CSV export.
  - `segments`: typed `Rules { match all|any, conditions: [Condition] }` (serde
    `deny_unknown_fields`, closed enum) → `sqlx::QueryBuilder` with binds only; preview count +
    sample.
  - `campaigns`: blocks (heading, text, image, button, product_grid, personalized_products),
    per-locale content, MJML render (platform layout + footer with unsubscribe/preferences
    link), preview for a subscriber, test send (≤ 5 addresses, marketing stream, no
    tracking), schedule/cancel, batch job (500 per batch, per-tenant window of 500/min,
    idempotent per (campaign, subscriber), consent + suppression + status re-checked),
    signed click links (HMAC-SHA256 with the campaign's `link_key`), stats.
  - `deliverability`: SNS envelope → SES `Bounce`/`Complaint` (notification or event
    publishing shape) → message id → tenant → suppression + subscriber status + send flags.
- `notifications`: `List-Unsubscribe` + `List-Unsubscribe-Post` on marketing mail, marketing
  re-check in `begin_send` (subscriber status + consent resolved now, A20), tenant logo and
  subject/intro overrides in `Brand`, `newsletter_confirm` template, admin message log (no
  bodies for sensitive mail) and suppression add/remove with audit.
- API: storefront `newsletter/*` (subscribe, confirmation, confirm, preferences, unsubscribe,
  resubscribe, click); admin subscribers/segments/campaigns/emails/suppressions/branding/
  templates; `POST /webhooks/ses` (HTTP Basic secret `MAIL_EVENTS_SECRET`, SNS signature
  verification designed + documented for prod).
- Worker: `marketing.campaign_batch`; `customer.email_verified` also links subscribers.
- Edge (checkout origin): `/_p/newsletter/{confirm,unsubscribe,resubscribe}` (forms + RFC 8058
  POST), `/_p/newsletter/click` (302 after API validation); checkout binding reads
  `newsletter/confirmation` + `newsletter/preferences`. Checkout app pages `/newsletter` and
  `/newsletter/confirm`.
- Admin SPA: Subscribers, Segments, Campaigns (+ editor, preview, test, schedule, stats),
  Emails (log + suppressions), Email settings (logo + texts).

## Global constraints

- Business logic only in `commerce`; no string SQL from input (segments bind every value).
- Marketing mail is never retried when uncertain (A14, existing pipeline); consent is read from
  `consent_records` at send time (A20); the confirm GET never mutates (link scanners).
- Tokens: 256-bit random, SHA-256 at rest (`capability`). Click links: HMAC over token+url.
- No open tracking (privacy default); the optional consented pixel is a follow-up.

## Review focus

- Segment compiler: only allowlisted fragments, every value bound; injection tests.
- Send path idempotency (retry of a batch, concurrent jobs), throttle, re-checks.
- Unsubscribe/click endpoints: capability handling, no open redirect, RFC 8058 shape.
- RLS + cross-tenant tests for the new tables.

## Tasks

1. Migration + RLS/cross-tenant test.
2. Notifications: headers, marketing re-check, branding/texts, confirm template, admin views.
3. Subscribers (+ consent withdrawal hook, customer link).
4. Segments compiler + preview (+ injection tests).
5. Campaigns: blocks, render, preview/test, schedule, batch send, clicks, stats.
6. Deliverability ingestion.
7. API routes + OpenAPI/clients.
8. Worker jobs.
9. Edge routes + checkout pages.
10. Admin UI.
11. e2e (`e2e/storefront/newsletter.spec.ts`, `e2e/admin/newsletter.spec.ts`), docs, follow-ups.
