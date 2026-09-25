-- WP12: carriers, shipments, exchange rates, invoices and credit notes, refunds by line,
-- withdrawals and returns, generated documents, email attachments (spec §7.4, §10.5-10.7,
-- A13, A15, A16, A17, A19, A21).
--
-- Tenant tables follow the WP1 rules (tenant_id, RLS + FORCE, composite FKs). Carrier
-- credentials are AES-256-GCM ciphertext (`platform::crypto`), never plaintext. Invoices are
-- immutable: the runtime role may only record the rendered PDF.

-- ---------------------------------------------------------------------------------------
-- Carrier accounts (per tenant). `credentials`: sealed JSON (Packeta API password; PPL client
-- id + secret), bound to `carrier:<tenant>:<carrier>`. `public_key`: the Packeta widget key,
-- which the checkout sends to browsers anyway.

CREATE TABLE carrier_accounts (
    tenant_id    uuid NOT NULL REFERENCES platform.tenants (id),
    carrier      text NOT NULL CHECK (carrier IN ('packeta', 'ppl')),
    credentials  bytea NOT NULL CHECK (length(credentials) BETWEEN 29 AND 2000),
    public_key   text CHECK (public_key ~ '^[A-Za-z0-9]{1,64}$'),
    sender_label text NOT NULL CHECK (length(sender_label) BETWEEN 1 AND 100),
    updated_by   text NOT NULL,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, carrier)
);

-- ---------------------------------------------------------------------------------------
-- Shipments (§7.4). `creating` is written before the carrier call and replaced by its
-- outcome, so a crash in between leaves a row an admin can cancel. One live shipment per
-- order (M1: no split shipments).

CREATE TABLE shipments (
    id              uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    order_id        uuid NOT NULL,
    carrier         text NOT NULL CHECK (carrier IN ('packeta_pickup', 'packeta_home', 'ppl', 'personal_pickup')),
    status          text NOT NULL CHECK (status IN ('creating', 'label_created', 'shipped',
                                                    'delivered', 'returned', 'cancelled')),
    carrier_ref     text CHECK (length(carrier_ref) <= 100),
    tracking_number text CHECK (length(tracking_number) <= 100),
    tracking_url    text CHECK (length(tracking_url) <= 500),
    -- Private bucket (A21).
    label_key       text CHECK (length(label_key) <= 300),
    -- The carrier's last reported state (code + text), from tracking polls.
    carrier_status  text CHECK (length(carrier_status) <= 200),
    tracked_at      timestamptz,
    shipped_at      timestamptz,
    delivered_at    timestamptz,
    created_by      text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id)
);
CREATE UNIQUE INDEX shipments_live ON shipments (tenant_id, order_id) WHERE status <> 'cancelled';
CREATE INDEX shipments_tracking ON shipments (tracked_at NULLS FIRST)
    WHERE status IN ('label_created', 'shipped') AND carrier <> 'personal_pickup';

-- ---------------------------------------------------------------------------------------
-- ČNB daily fixing (A17): CZK per `amount` units, × 1000 (ČNB publishes three decimals).
-- Platform data, not tenant-owned.

CREATE TABLE platform.exchange_rates (
    currency    text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    fixing_date date NOT NULL,
    amount      integer NOT NULL CHECK (amount > 0),
    rate_milli  bigint NOT NULL CHECK (rate_milli > 0),
    fetched_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (currency, fixing_date)
);
GRANT SELECT, INSERT, UPDATE ON platform.exchange_rates TO app_runtime;

-- ---------------------------------------------------------------------------------------
-- Invoices and credit notes (§7.4, §10.7, A17). Numbers `{prefix}{YYYY}{seq:05}`, gapless:
-- assigned in the issuing transaction under the series row lock.

CREATE TABLE invoice_series (
    id          uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id   uuid NOT NULL REFERENCES platform.tenants (id),
    kind        text NOT NULL CHECK (kind IN ('invoice', 'credit_note')),
    year        integer NOT NULL CHECK (year BETWEEN 2000 AND 9999),
    prefix      text NOT NULL CHECK (prefix ~ '^[A-Z]{0,6}$'),
    next_number integer NOT NULL DEFAULT 1 CHECK (next_number BETWEEN 1 AND 99999),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, kind, year)
);

CREATE TABLE invoices (
    id                  uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id           uuid NOT NULL REFERENCES platform.tenants (id),
    series_id           uuid NOT NULL,
    kind                text NOT NULL CHECK (kind IN ('invoice', 'credit_note')),
    number              text NOT NULL CHECK (number ~ '^[A-Z]{0,6}[0-9]{9}$'),
    order_id            uuid NOT NULL,
    -- Credit notes: the invoice they correct.
    original_id         uuid,
    issued_on           date NOT NULL,
    taxable_supply_date date NOT NULL,
    due_on              date NOT NULL,
    currency            text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    vat_payer           boolean NOT NULL,
    -- The structured document (supplier, customer, lines, VAT recap, CZK recap, totals):
    -- the source of the PDF and of later e-invoice exports.
    document            jsonb NOT NULL CHECK (jsonb_typeof(document) = 'object'),
    total_minor         bigint NOT NULL,
    pdf_key             text CHECK (length(pdf_key) <= 300),
    pdf_rendered_at     timestamptz,
    created_by          text NOT NULL,
    created_at          timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    CONSTRAINT invoices_number_unique UNIQUE (tenant_id, number),
    CHECK ((kind = 'credit_note') = (original_id IS NOT NULL)),
    FOREIGN KEY (tenant_id, series_id) REFERENCES invoice_series (tenant_id, id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id),
    FOREIGN KEY (tenant_id, original_id) REFERENCES invoices (tenant_id, id)
);
-- One (final) invoice per order; any number of credit notes.
CREATE UNIQUE INDEX invoices_one_per_order ON invoices (tenant_id, order_id) WHERE kind = 'invoice';
CREATE INDEX invoices_order ON invoices (tenant_id, order_id, created_at);

-- ---------------------------------------------------------------------------------------
-- Withdrawals (A19) and returned lines (A13).

CREATE TABLE withdrawals (
    id                uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id         uuid NOT NULL REFERENCES platform.tenants (id),
    order_id          uuid NOT NULL,
    email             text NOT NULL CHECK (length(email) BETWEEN 3 AND 254),
    locale            text NOT NULL CHECK (locale ~ '^[a-z]{2}(-[A-Z]{2})?$'),
    -- web (guest link) | account (signed-in customer)
    channel           text NOT NULL CHECK (channel IN ('web', 'account')),
    status            text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'refunded')),
    -- The full declaration as the customer confirmed it (the durable receipt's content).
    declaration       text NOT NULL CHECK (length(declaration) BETWEEN 1 AND 20000),
    -- Bank refunds (bank transfer, cash on delivery) go to this account.
    iban              text CHECK (iban ~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$'),
    note              text CHECK (length(note) <= 1000),
    delivered_at      timestamptz,
    declared_at       timestamptz NOT NULL DEFAULT now(),
    goods_received_at timestamptz,
    return_proof_at   timestamptz,
    refund_due_at     timestamptz NOT NULL,
    refunded_at       timestamptz,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id)
);
CREATE INDEX withdrawals_open ON withdrawals (tenant_id, refund_due_at) WHERE status = 'open';
CREATE INDEX withdrawals_order ON withdrawals (tenant_id, order_id);

-- The emailed confirmation link of the public form: proves control of the order's mailbox.
-- Single use (consumed by the declaration), 24 hours, SHA-256 at rest.
CREATE TABLE withdrawal_tokens (
    token_hash bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id  uuid NOT NULL,
    order_id   uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, order_id) REFERENCES orders (tenant_id, id)
);

CREATE TABLE return_lines (
    id            uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id     uuid NOT NULL,
    withdrawal_id uuid NOT NULL,
    order_line_id uuid NOT NULL,
    quantity      integer NOT NULL CHECK (quantity > 0),
    status        text NOT NULL CHECK (status IN ('requested', 'approved', 'received',
                                  'refunded_awaiting_goods', 'refunded', 'rejected')),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, withdrawal_id, order_line_id),
    FOREIGN KEY (tenant_id, withdrawal_id) REFERENCES withdrawals (tenant_id, id),
    FOREIGN KEY (tenant_id, order_line_id) REFERENCES order_lines (tenant_id, id)
);
CREATE INDEX return_lines_order_line ON return_lines (tenant_id, order_line_id);

-- ---------------------------------------------------------------------------------------
-- Refunds by line (A15): what a refund covered, and its credit note.

ALTER TABLE refunds
    ADD COLUMN credit_note_id uuid,
    ADD COLUMN withdrawal_id  uuid,
    -- The refunded allocations: [{"order_line_id", "quantity", "gross_minor", "vat_minor"}]
    -- plus charges [{"charge": "shipping", ...}]; NULL for amount-only refunds (A10 exceptions).
    ADD COLUMN lines          jsonb CHECK (jsonb_typeof(lines) = 'array'),
    ADD COLUMN iban           text CHECK (iban ~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$'),
    ADD CONSTRAINT refunds_credit_note_fk FOREIGN KEY (tenant_id, credit_note_id)
        REFERENCES invoices (tenant_id, id),
    ADD CONSTRAINT refunds_withdrawal_fk FOREIGN KEY (tenant_id, withdrawal_id)
        REFERENCES withdrawals (tenant_id, id);

-- ---------------------------------------------------------------------------------------
-- Generated documents (packing slips, label sheets): rendered by the worker into the private
-- bucket, downloaded through a presigned URL.

CREATE TABLE documents (
    id         uuid NOT NULL DEFAULT platform.uuid_v7(),
    tenant_id  uuid NOT NULL REFERENCES platform.tenants (id),
    kind       text NOT NULL CHECK (kind IN ('packing_slips', 'labels')),
    order_ids  uuid[] NOT NULL CHECK (cardinality(order_ids) BETWEEN 1 AND 100),
    locale     text NOT NULL CHECK (locale IN ('cs', 'sk', 'en')),
    status     text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'ready', 'failed')),
    object_key text CHECK (length(object_key) <= 300),
    error      text CHECK (length(error) <= 1000),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (tenant_id, id)
);

-- ---------------------------------------------------------------------------------------
-- Email attachments: private-bucket objects the worker attaches at send time
-- (`[{"key", "filename", "content_type"}]`).

ALTER TABLE email_messages
    ADD COLUMN attachments jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(attachments) = 'array');

-- ---------------------------------------------------------------------------------------
-- Row-level security and grants.

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['carrier_accounts', 'shipments', 'invoice_series', 'invoices',
        'withdrawals', 'withdrawal_tokens', 'return_lines', 'documents']
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

GRANT SELECT, INSERT, UPDATE, DELETE ON carrier_accounts TO app_runtime;
GRANT SELECT, INSERT, UPDATE ON shipments, invoice_series, withdrawals, withdrawal_tokens,
    return_lines, documents TO app_runtime;
-- Invoices are immutable once issued (§7.4): only the rendered PDF is recorded afterwards.
GRANT SELECT, INSERT ON invoices TO app_runtime;
GRANT UPDATE (pdf_key, pdf_rendered_at) ON invoices TO app_runtime;
-- The declaration is the durable receipt: never rewritten.
REVOKE UPDATE ON withdrawals FROM app_runtime;
GRANT UPDATE (status, note, goods_received_at, return_proof_at, refunded_at) ON withdrawals
    TO app_runtime;

-- Tracking polls scan every tenant: only ids leave this function; the poll itself runs per
-- tenant in `tenant_tx`.
CREATE POLICY tracking_scan ON shipments FOR SELECT TO app_owner
    USING (status IN ('label_created', 'shipped') AND carrier <> 'personal_pickup');

CREATE FUNCTION platform.trackable_shipments(max_rows integer)
RETURNS TABLE (tenant_id uuid, shipment_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = ''
AS $$
    SELECT s.tenant_id, s.id FROM public.shipments s
    WHERE s.status IN ('label_created', 'shipped') AND s.carrier <> 'personal_pickup'
    ORDER BY s.tracked_at NULLS FIRST
    LIMIT least(greatest(max_rows, 1), 1000)
$$;
REVOKE ALL ON FUNCTION platform.trackable_shipments(integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.trackable_shipments(integer) TO app_runtime;
