-- WP11: payment adapters (spec §7.4, §10.4, A10, A11, A16, A21, A25).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Provider
-- credentials are stored encrypted (AES-256-GCM, `platform::crypto`), never in plaintext.

-- ---------------------------------------------------------------------------------------
-- Stripe Connect (A11): the tenant's connected account and what Stripe last told us about it.
-- `account.updated` keeps the flags current; Stripe is offered at checkout only while charges
-- are enabled and the card_payments capability is active.

CREATE TABLE stripe_accounts (
    tenant_id         uuid PRIMARY KEY REFERENCES platform.tenants (id),
    account_id        text NOT NULL UNIQUE CHECK (account_id ~ '^acct_[A-Za-z0-9_]{1,250}$'),
    -- The mode the account was created in: events must carry the same livemode flag.
    livemode          boolean NOT NULL,
    charges_enabled   boolean NOT NULL DEFAULT false,
    details_submitted boolean NOT NULL DEFAULT false,
    card_payments     text NOT NULL DEFAULT 'inactive'
                      CHECK (card_payments IN ('active', 'inactive', 'pending')),
    disabled_reason   text CHECK (length(disabled_reason) <= 200),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- A11: every provider event, stored before the webhook answers 200 and processed by a job.
-- Platform table: the tenant is only known after the account is matched.
CREATE TABLE platform.provider_events (
    id           uuid PRIMARY KEY DEFAULT platform.uuid_v7(),
    provider     text NOT NULL CHECK (provider IN ('stripe')),
    event_id     text NOT NULL CHECK (length(event_id) BETWEEN 1 AND 255),
    type         text NOT NULL CHECK (length(type) BETWEEN 1 AND 100),
    account      text CHECK (length(account) <= 255),
    livemode     boolean NOT NULL,
    payload      jsonb NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now(),
    tenant_id    uuid REFERENCES platform.tenants (id),
    -- applied | ignored | rejected, with the reason; NULL until processed.
    outcome      text CHECK (outcome IN ('applied', 'ignored', 'rejected')),
    detail       text CHECK (length(detail) <= 500),
    processed_at timestamptz,
    CONSTRAINT provider_events_unique UNIQUE (provider, event_id)
);
CREATE INDEX provider_events_unprocessed ON platform.provider_events (received_at)
    WHERE processed_at IS NULL;

-- ---------------------------------------------------------------------------------------
-- Bank transfer (A25): one receiving account per market. The Fio API token (optional) is
-- AES-256-GCM ciphertext bound to the tenant and account (`platform::crypto`).

CREATE TABLE bank_accounts (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL REFERENCES platform.tenants (id),
    market_id     uuid NOT NULL,
    currency      text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    iban          text NOT NULL CHECK (iban ~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$'),
    bic           text CHECK (bic ~ '^[A-Z]{6}[A-Z0-9]{2}([A-Z0-9]{3})?$'),
    account_name  text NOT NULL CHECK (length(account_name) BETWEEN 1 AND 70),
    fio_token     bytea CHECK (length(fio_token) BETWEEN 29 AND 400),
    fio_synced_at timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT bank_accounts_market_unique UNIQUE (tenant_id, market_id),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id) ON DELETE CASCADE
);

-- Statement lines (credits only). The bank's transaction id is unique per account, so a
-- statement imported twice (or overlapping Fio API windows) adds nothing.
CREATE TABLE bank_transactions (
    id                uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id         uuid NOT NULL REFERENCES platform.tenants (id),
    bank_account_id   uuid NOT NULL,
    bank_tx_id        text NOT NULL CHECK (length(bank_tx_id) BETWEEN 1 AND 100),
    booked_on         date NOT NULL,
    amount_minor      bigint NOT NULL CHECK (amount_minor > 0),
    currency          text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    variable_symbol   text CHECK (variable_symbol ~ '^[0-9]{1,10}$'),
    counterparty      text CHECK (length(counterparty) <= 100),
    counterparty_name text CHECK (length(counterparty_name) <= 200),
    message           text CHECK (length(message) <= 500),
    source            text NOT NULL CHECK (source IN ('camt053', 'fio_csv', 'gpc', 'fio_api')),
    raw               jsonb NOT NULL,
    -- matched: paid an attempt; the others wait in the exceptions queue until resolved.
    status            text NOT NULL CHECK (status IN ('matched', 'unmatched', 'partial', 'overpaid', 'dismissed')),
    reason            text CHECK (reason IN ('no_variable_symbol', 'unknown_variable_symbol',
                                             'currency_mismatch', 'already_paid', 'amount_short',
                                             'amount_over', 'manual')),
    attempt_id        uuid,
    note              text CHECK (length(note) <= 500),
    resolved_by       text,
    resolved_at       timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT bank_transactions_unique UNIQUE (tenant_id, bank_account_id, bank_tx_id),
    FOREIGN KEY (tenant_id, bank_account_id) REFERENCES bank_accounts (tenant_id, id),
    FOREIGN KEY (tenant_id, attempt_id) REFERENCES payment_attempts (tenant_id, id)
);
CREATE INDEX bank_transactions_list ON bank_transactions (tenant_id, created_at DESC, id DESC);
CREATE INDEX bank_transactions_open ON bank_transactions (tenant_id, created_at)
    WHERE status IN ('unmatched', 'partial', 'overpaid');

-- ---------------------------------------------------------------------------------------
-- Attempts: bank transfer details (A25), cash on delivery (A16), payment reminders.

ALTER TABLE payment_attempts
    ADD COLUMN bank_account_id uuid,
    ADD COLUMN variable_symbol text CHECK (variable_symbol ~ '^[0-9]{1,10}$'),
    -- What the customer needs to pay (account, VS, amount, message, QR payload), fixed at
    -- placement so a later account change does not alter an open order.
    ADD COLUMN instructions    jsonb CHECK (jsonb_typeof(instructions) = 'object'),
    ADD COLUMN tender          text CHECK (tender IN ('cash', 'card', 'unknown')),
    ADD COLUMN collector       text CHECK (collector IN ('carrier', 'merchant')),
    ADD COLUMN cod_status      text CHECK (cod_status IN ('pending', 'delivered', 'collected', 'remitted')),
    ADD COLUMN delivered_at    timestamptz,
    ADD COLUMN collected_at    timestamptz,
    ADD COLUMN remitted_at     timestamptz,
    ADD COLUMN reminders_sent  smallint NOT NULL DEFAULT 0 CHECK (reminders_sent BETWEEN 0 AND 2),
    ADD CONSTRAINT payment_attempts_bank_account_fk FOREIGN KEY (tenant_id, bank_account_id)
        REFERENCES bank_accounts (tenant_id, id);

-- COD attempts placed before WP11 get a COD state (the owner is subject to RLS, hence the
-- temporary policy).
CREATE POLICY wp11_backfill ON payment_attempts TO app_owner USING (true) WITH CHECK (true);
UPDATE payment_attempts
SET cod_status = CASE WHEN status = 'succeeded' THEN 'collected' ELSE 'pending' END,
    tender = 'unknown'
WHERE method = 'cod';
DROP POLICY wp11_backfill ON payment_attempts;

ALTER TABLE payment_attempts
    ADD CONSTRAINT payment_attempts_bank_details CHECK (
        (method = 'bank_transfer') = (variable_symbol IS NOT NULL AND bank_account_id IS NOT NULL)),
    ADD CONSTRAINT payment_attempts_cod_details CHECK ((method = 'cod') = (cod_status IS NOT NULL));
-- A25: the variable symbol identifies one payment per tenant and receiving account.
CREATE UNIQUE INDEX payment_attempts_variable_symbol
    ON payment_attempts (tenant_id, bank_account_id, variable_symbol)
    WHERE variable_symbol IS NOT NULL;
CREATE INDEX payment_attempts_reminders ON payment_attempts (created_at)
    WHERE method = 'bank_transfer' AND status = 'pending';

-- Refunds (§7.4): Stripe via the API with the refund id as idempotency key and the
-- application fee refunded proportionally (A11); bank/COD refunds are recorded manually.
CREATE TABLE refunds (
    id           uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    order_id     uuid NOT NULL,
    attempt_id   uuid NOT NULL,
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    currency     text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    reason       text CHECK (length(reason) <= 500),
    status       text NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    provider_ref text CHECK (length(provider_ref) <= 255),
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id),
    FOREIGN KEY (tenant_id, attempt_id) REFERENCES payment_attempts (tenant_id, id)
);
CREATE INDEX refunds_order ON refunds (tenant_id, order_id, created_at);

-- A10 exceptions (late or duplicate payments) leave the queue once someone resolved them.
ALTER TABLE orders
    ADD COLUMN exception_resolved_at timestamptz,
    ADD COLUMN exception_note        text CHECK (length(exception_note) <= 500);

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['stripe_accounts', 'bank_accounts', 'bank_transactions', 'refunds']
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I TO app_runtime
                 USING (tenant_id = current_setting(''app.tenant_id'')::uuid)
                 WITH CHECK (tenant_id = current_setting(''app.tenant_id'')::uuid)', t);
    END LOOP;
END
$$;

GRANT SELECT, INSERT, UPDATE ON stripe_accounts, bank_accounts, bank_transactions, refunds
    TO app_runtime;
-- Provider events are append-only apart from their processing state.
REVOKE DELETE ON platform.provider_events FROM app_runtime;

-- Cross-tenant scans for jobs and webhook routing: only ids leave these functions; the work
-- itself runs per tenant in `tenant_tx`.

CREATE POLICY webhook_routing ON stripe_accounts FOR SELECT TO app_owner USING (true);

-- The tenant of a connected account (Stripe webhooks carry only the account id).
CREATE FUNCTION platform.stripe_account_tenant(p_account text) RETURNS uuid
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT s.tenant_id FROM public.stripe_accounts s WHERE s.account_id = p_account
$$;

CREATE POLICY reminder_scan ON payment_attempts FOR SELECT TO app_owner
    USING (method = 'bank_transfer' AND status = 'pending');

-- Bank transfers waiting for money: the first reminder after 3 days, the second after 6
-- (spec §10.3), only while the payment window is open.
CREATE FUNCTION platform.due_payment_reminders(max_rows integer)
RETURNS TABLE (tenant_id uuid, attempt_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT a.tenant_id, a.id FROM public.payment_attempts a
    WHERE a.method = 'bank_transfer' AND a.status = 'pending'
      AND (a.expires_at IS NULL OR a.expires_at > now())
      AND ((a.reminders_sent = 0 AND a.created_at <= now() - interval '3 days')
        OR (a.reminders_sent = 1 AND a.created_at <= now() - interval '6 days'))
    ORDER BY a.created_at
    LIMIT least(greatest(max_rows, 1), 1000)
$$;

CREATE POLICY fio_scan ON bank_accounts FOR SELECT TO app_owner USING (fio_token IS NOT NULL);

-- Accounts with a Fio API token (polled by the worker).
CREATE FUNCTION platform.fio_bank_accounts()
RETURNS TABLE (tenant_id uuid, bank_account_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT b.tenant_id, b.id FROM public.bank_accounts b
    WHERE b.fio_token IS NOT NULL
    ORDER BY b.fio_synced_at NULLS FIRST
    LIMIT 1000
$$;

REVOKE ALL ON FUNCTION platform.stripe_account_tenant(text), platform.due_payment_reminders(integer),
    platform.fio_bank_accounts() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.stripe_account_tenant(text), platform.due_payment_reminders(integer),
    platform.fio_bank_accounts() TO app_runtime;
