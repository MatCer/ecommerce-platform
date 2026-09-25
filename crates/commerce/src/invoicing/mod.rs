//! Invoicing (spec §7.4, §10.7, A15, A17): invoices and credit notes of orders.
//!
//! Scenarios (A17):
//! 1. Prepaid (card, bank transfer): the invoice is issued once the payment is received; its
//!    taxable supply date (DUZP) is the payment date and it is also the final invoice.
//! 2. Cash on delivery: the invoice is issued on dispatch (DUZP = dispatch date).
//! 3. Non-VAT payers: no VAT, no recap.
//! 4. Refunds (returns, withdrawals, cancellations after payment): a credit note reversing the
//!    original allocations ([`document::reverse_units`]).
//!
//! Issuing is a job ([`ISSUE_JOB`], subscribed to `order.paid` and `order.shipped`) because
//! a non-CZK document of a CZ VAT payer needs the ČNB fixing for its DUZP (a network call,
//! never inside a transaction); if that fixing is not published yet the job retries and the
//! order shows `invoice_delayed`. Numbers are gapless: taken from the series row under its
//! lock in the issuing transaction. Documents are immutable (the runtime role may only record
//! the rendered PDF); the worker renders them with Typst into the private bucket
//! ([`RENDER_JOB`]) and emails them to the customer.
//!
//! The templates require accountant approval before real use (A17).

pub mod cnb;
pub mod document;
pub mod prague;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use object_store::ObjectStoreExt;
use platform::Error;
use platform::db::TenantTx;
use platform::queue::{self, NewJob};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::content::legal;
use crate::money::{Currency, Locale, Money, MoneyView};
use crate::orders;
use crate::pricing::cart::{ChargeKind, ChargePortion};
use crate::tax::{self, TaxRate};
use document::{
    BankAccountRef, Customer, DocLine, Document, DocumentKind, Issue, OrderSource, OriginalRef,
    PaymentInfo, SourceCharge, SourceLine, Supplier,
};

/// Issues the order's invoice when its scenario says so (payload `{order_id}`).
pub const ISSUE_JOB: &str = "invoicing.issue";
/// Renders a document's PDF and emails it (payload `{invoice_id}`).
pub const RENDER_JOB: &str = "invoicing.render";
/// Stores today's ČNB fixing (cron).
pub const RATES_JOB: &str = "invoicing.rates";

/// The ČNB client for the worker.
#[derive(Debug, Clone)]
pub struct Rates {
    pub http: reqwest::Client,
    pub url: String,
}

// ---------------------------------------------------------------------------------------
// Sources

/// The seller from the legal entity (§14) and the tax profile (A3). `422
/// invoice_supplier_incomplete` when the legal entity lacks what a tax document needs.
pub async fn supplier(tx: &mut TenantTx) -> Result<(Supplier, bool, String), Error> {
    let e = legal::entity(tx).await?.entity;
    let profile = tax::require(tx).await?;
    if e.company_name.trim().is_empty()
        || e.company_id.trim().is_empty()
        || e.street.trim().is_empty()
        || e.city.trim().is_empty()
    {
        return Err(Error::Conflict {
            code: "invoice_supplier_incomplete",
            detail: "the legal entity (name, company id, address) is required for invoices".into(),
        });
    }
    let opt = |s: String| Some(s).filter(|s| !s.trim().is_empty());
    Ok((
        Supplier {
            name: e.company_name,
            street: e.street,
            city: e.city,
            postal_code: e.postal_code,
            country: e.country,
            company_id: e.company_id,
            vat_id: profile.vat_id.clone(),
            sk_ic_dph: profile.sk_ic_dph.clone(),
            registry: opt(e.registry),
            email: opt(e.email),
            phone: opt(e.phone),
        },
        profile.vat_payer,
        profile.establishment_country,
    ))
}

/// The order as persisted (A15), for documents.
pub async fn order_source(tx: &mut TenantTx, order_id: Uuid) -> Result<OrderSource, Error> {
    let o = sqlx::query!(
        "SELECT number, locale, currency, vat_payer, email, shipping_method_snapshot
         FROM orders WHERE id = $1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let bad = |e: String| Error::Internal(format!("stored order {order_id}: {e}"));
    let currency =
        Currency::parse(&o.currency).ok_or_else(|| bad(format!("currency {}", o.currency)))?;
    let lines = sqlx::query!(
        "SELECT id, name, options_label, sku, quantity, unit_gross_minor, total_minor, tax_rate,
                tax_minor
         FROM order_lines WHERE order_id = $1 ORDER BY position",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|l| {
        Ok(SourceLine {
            id: l.id,
            name: l.name,
            options_label: l.options_label,
            sku: l.sku,
            quantity: l.quantity,
            unit_gross_minor: l.unit_gross_minor,
            total_minor: l.total_minor,
            tax_rate: l
                .tax_rate
                .parse::<TaxRate>()
                .map_err(|()| bad(format!("tax rate {}", l.tax_rate)))?,
            tax_minor: l.tax_minor,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;
    let method_name = o
        .shipping_method_snapshot
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let charges = sqlx::query!(
        "SELECT kind, total_minor, tax_minor, portions FROM order_charges WHERE order_id = $1
         ORDER BY CASE kind WHEN 'shipping' THEN 0 WHEN 'payment_fee' THEN 1 ELSE 2 END",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter(|c| c.total_minor != 0)
    .map(|c| {
        Ok(SourceCharge {
            kind: serde_json::from_value::<ChargeKind>(json!(c.kind))
                .map_err(|e| bad(e.to_string()))?,
            name: method_name.clone(),
            total_minor: c.total_minor,
            tax_minor: c.tax_minor,
            portions: serde_json::from_value::<Vec<ChargePortion>>(c.portions)
                .map_err(|e| bad(e.to_string()))?,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;
    let a = sqlx::query!(
        "SELECT name, company, street, city, postal_code, country FROM order_addresses
         WHERE order_id = $1 ORDER BY CASE kind WHEN 'billing' THEN 0 ELSE 1 END LIMIT 1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let customer = match a {
        Some(a) => Customer {
            name: a.name,
            company: a.company,
            street: a.street,
            city: a.city,
            postal_code: a.postal_code,
            country: a.country,
            email: o.email,
        },
        None => Customer {
            name: o.email.clone(),
            company: None,
            street: String::new(),
            city: String::new(),
            postal_code: String::new(),
            country: String::new(),
            email: o.email,
        },
    };
    Ok(OrderSource {
        number: o.number.to_string(),
        locale: o.locale,
        currency,
        vat_payer: o.vat_payer,
        lines,
        charges,
        customer,
    })
}

/// The next number of a series (`{prefix}{YYYY}{seq:05}`), gapless: the series row stays
/// locked until the issuing transaction commits, and a rollback returns the number.
async fn next_number(
    tx: &mut TenantTx,
    kind: DocumentKind,
    year: i32,
) -> Result<(Uuid, String), Error> {
    sqlx::query!(
        "INSERT INTO invoice_series (tenant_id, kind, year, prefix) VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, kind, year) DO NOTHING",
        tx.tenant_id(),
        kind.as_str(),
        year,
        kind.prefix()
    )
    .execute(&mut **tx)
    .await?;
    let s = sqlx::query!(
        "SELECT id, prefix, next_number FROM invoice_series WHERE kind = $1 AND year = $2
         FOR UPDATE",
        kind.as_str(),
        year
    )
    .fetch_one(&mut **tx)
    .await?;
    if s.next_number > 99_999 {
        return Err(Error::Conflict {
            code: "invoice_series_exhausted",
            detail: format!("the {year} series is full"),
        });
    }
    sqlx::query!(
        "UPDATE invoice_series SET next_number = next_number + 1 WHERE id = $1",
        s.id
    )
    .execute(&mut **tx)
    .await?;
    Ok((s.id, format!("{}{year}{:05}", s.prefix, s.next_number)))
}

async fn insert(
    tx: &mut TenantTx,
    series_id: Uuid,
    order_id: Uuid,
    doc: &Document,
    actor: &str,
) -> Result<Uuid, Error> {
    let id = crate::id::new_id();
    let body = serde_json::to_value(doc).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "INSERT INTO invoices (id, tenant_id, series_id, kind, number, order_id, original_id,
             issued_on, taxable_supply_date, due_on, currency, vat_payer, document, total_minor,
             created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        id,
        tx.tenant_id(),
        series_id,
        doc.kind.as_str(),
        doc.number,
        order_id,
        doc.original.as_ref().map(|o| o.id),
        doc.issued_on,
        doc.taxable_supply_date,
        doc.due_on,
        doc.currency.code(),
        doc.vat_payer,
        body,
        doc.totals.gross_minor,
        actor
    )
    .execute(&mut **tx)
    .await?;
    orders::event(
        tx,
        order_id,
        &format!("{}_issued", doc.kind.as_str()),
        &json!({ "invoice_id": id, "number": doc.number, "total_minor": doc.totals.gross_minor }),
        actor,
    )
    .await?;
    let mut job = NewJob::new(RENDER_JOB, json!({ "invoice_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.idempotency_key = Some(format!("invoice_render:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    Ok(id)
}

async fn payment_info(tx: &mut TenantTx, order_id: Uuid, paid: bool) -> Result<PaymentInfo, Error> {
    let o = sqlx::query!(
        "SELECT number, payment_method, market_id FROM orders WHERE id = $1",
        order_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let bank = crate::payments::bank::account(tx, o.market_id)
        .await?
        .map(|b| BankAccountRef {
            iban: b.iban,
            bic: b.bic,
            name: b.account_name,
        });
    Ok(PaymentInfo {
        method: o.payment_method,
        variable_symbol: o.number.to_string(),
        paid,
        bank_account: bank,
    })
}

// ---------------------------------------------------------------------------------------
// Scenarios (A17)

/// Why and when an order's invoice is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Not yet (unpaid prepaid order, COD order not dispatched) or never (cancelled unpaid).
    No,
    /// Prepaid: the payment date. COD: the dispatch date.
    On { duzp: NaiveDate, paid: bool },
}

/// The A17 scenario of an order right now.
pub async fn due(tx: &mut TenantTx, order_id: Uuid) -> Result<Due, Error> {
    let o = sqlx::query!(
        r#"SELECT o.payment_method, a.completed_at AS "paid_at?",
                  (SELECT min(s.shipped_at) FROM shipments s
                   WHERE s.order_id = o.id AND s.shipped_at IS NOT NULL) AS "shipped_at?"
           FROM orders o LEFT JOIN payment_attempts a ON a.id = o.paid_attempt_id
           WHERE o.id = $1"#,
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(match (o.payment_method.as_str(), o.paid_at, o.shipped_at) {
        ("cod", paid, Some(shipped)) => Due::On {
            duzp: prague::date(shipped),
            paid: paid.is_some(),
        },
        ("cod", _, None) | (_, None, _) => Due::No,
        (_, Some(paid), _) => Due::On {
            duzp: prague::date(paid),
            paid: true,
        },
    })
}

/// The order's invoice, if issued.
pub async fn invoice_of(tx: &mut TenantTx, order_id: Uuid) -> Result<Option<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM invoices WHERE order_id = $1 AND kind = 'invoice'",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issued {
    New(Uuid),
    Existing(Uuid),
    NotDue,
    /// The ČNB rate for the DUZP is not published yet: retry later.
    RateUnavailable,
}

/// Issues the order's invoice if its scenario is due (idempotent). Runs its own transactions:
/// the ČNB fixing is fetched between them.
pub async fn issue(
    db: &PgPool,
    rates: &Rates,
    tenant_id: Uuid,
    order_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Issued, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    if let Some(id) = invoice_of(&mut tx, order_id).await? {
        return Ok(Issued::Existing(id));
    }
    let Due::On { duzp, paid } = due(&mut tx, order_id).await? else {
        return Ok(Issued::NotDue);
    };
    let (supplier, _, _) = supplier(&mut tx).await?;
    let src = order_source(&mut tx, order_id).await?;
    tx.commit().await?;

    let rate = if document::needs_czk_recap(src.vat_payer, &supplier.country, src.currency) {
        match cnb::rate_for(db, &rates.http, &rates.url, src.currency, duzp, now).await {
            Ok(Some(r)) => Some(r),
            Ok(None) => {
                warn_delayed(
                    db,
                    tenant_id,
                    order_id,
                    "exchange_rate_unpublished",
                    src.currency,
                    duzp,
                )
                .await?;
                return Ok(Issued::RateUnavailable);
            }
            Err(e) => {
                // ČNB unreachable: the admin sees why the invoice waits; the job retries.
                warn_delayed(
                    db,
                    tenant_id,
                    order_id,
                    "exchange_rate_unavailable",
                    src.currency,
                    duzp,
                )
                .await?;
                return Err(e);
            }
        }
    } else {
        None
    };

    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    // Serializes with a concurrent issue of the same order (the unique index backs it up).
    orders::lock(&mut tx, order_id).await?;
    if let Some(id) = invoice_of(&mut tx, order_id).await? {
        return Ok(Issued::Existing(id));
    }
    let today = prague::date(now);
    let (series, number) = next_number(&mut tx, DocumentKind::Invoice, today.year()).await?;
    let payment = payment_info(&mut tx, order_id, paid).await?;
    let doc = document::invoice(
        &src,
        Issue {
            number,
            issued_on: today,
            taxable_supply_date: duzp,
            due_on: today,
            supplier,
            payment,
            rate,
        },
    );
    let id = insert(&mut tx, series, order_id, &doc, "system").await?;
    tx.commit().await?;
    Ok(Issued::New(id))
}

/// Records once per order why its invoice waits (`invoice_delayed`, shown in the admin).
async fn warn_delayed(
    db: &PgPool,
    tenant_id: Uuid,
    order_id: Uuid,
    reason: &str,
    currency: Currency,
    duzp: NaiveDate,
) -> Result<(), Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let warned = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM order_events
                          WHERE order_id = $1 AND kind = 'invoice_delayed') AS "x!""#,
        order_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !warned {
        orders::event(
            &mut tx,
            order_id,
            "invoice_delayed",
            &json!({ "reason": reason, "currency": currency, "taxable_supply_date": duzp }),
            "system",
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A later issue attempt for an order whose ČNB fixing is not published yet: a fresh job every
/// 30 minutes, so waiting for publication never uses up the job's failure retries.
pub fn delayed_issue_job(tenant_id: Uuid, order_id: Uuid, now: DateTime<Utc>) -> NewJob<'static> {
    let slot = now.timestamp().div_euclid(1800);
    let mut job = NewJob::new(ISSUE_JOB, json!({ "payload": { "order_id": order_id } }));
    job.tenant_id = Some(tenant_id);
    job.run_at = Some(now + chrono::Duration::minutes(30));
    job.idempotency_key = Some(format!("invoice_issue_wait:{order_id}:{slot}"));
    job
}

/// Whether the order has (or will get) an invoice, so a refund must come with a credit note.
pub async fn expects_invoice(tx: &mut TenantTx, order_id: Uuid) -> Result<bool, Error> {
    Ok(matches!(due(tx, order_id).await?, Due::On { .. }))
}

/// Issues a credit note for refunded allocations (`lines` positive) in the caller's
/// transaction. The CZK recap uses the original invoice's rate. `409 invoice_pending` when the
/// order should have an invoice that is not issued yet.
pub async fn issue_credit_note(
    tx: &mut TenantTx,
    order_id: Uuid,
    lines: Vec<DocLine>,
    reason: Option<String>,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, Error> {
    let Some(original_id) = invoice_of(tx, order_id).await? else {
        if expects_invoice(tx, order_id).await? {
            return Err(Error::Conflict {
                code: "invoice_pending",
                detail: "the order's invoice is being issued; retry in a moment".into(),
            });
        }
        return Ok(None);
    };
    let original = sqlx::query!(
        "SELECT number, issued_on, document FROM invoices WHERE id = $1",
        original_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let orig_doc: Document = serde_json::from_value(original.document)
        .map_err(|e| Error::Internal(format!("stored invoice {original_id}: {e}")))?;
    let src = order_source(tx, order_id).await?;
    let today = prague::date(now);
    let (series, number) = next_number(tx, DocumentKind::CreditNote, today.year()).await?;
    let payment = PaymentInfo {
        paid: true,
        ..orig_doc.payment.clone()
    };
    let mut doc = document::credit_note(
        &src,
        Issue {
            number,
            issued_on: today,
            taxable_supply_date: today,
            due_on: today,
            supplier: orig_doc.supplier.clone(),
            payment,
            rate: orig_doc.czk_recap.as_ref().map(|r| r.rate),
        },
        lines,
        OriginalRef {
            id: original_id,
            number: original.number,
            issued_on: original.issued_on,
        },
        reason,
    );
    if let (Some(orig), Some(this)) = (&orig_doc.czk_recap, doc.czk_recap.as_mut()) {
        let prior = sqlx::query_scalar!(
            "SELECT document FROM invoices WHERE original_id = $1",
            original_id
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .filter_map(|d| serde_json::from_value::<Document>(d).ok())
        .collect::<Vec<_>>();
        document::settle_czk_residual(&orig_doc, orig, &prior, &doc.vat_recap, this);
    }
    Ok(Some(insert(tx, series, order_id, &doc, actor).await?))
}

// ---------------------------------------------------------------------------------------
// Read models

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct InvoiceSummary {
    pub id: Uuid,
    pub kind: DocumentKind,
    pub number: String,
    pub issued_on: NaiveDate,
    pub taxable_supply_date: NaiveDate,
    pub total: MoneyView,
    /// The PDF is rendered (downloadable).
    pub pdf_ready: bool,
}

pub async fn list_for_order(
    tx: &mut TenantTx,
    order_id: Uuid,
) -> Result<Vec<InvoiceSummary>, Error> {
    let locale = sqlx::query_scalar!("SELECT locale FROM orders WHERE id = $1", order_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    sqlx::query!(
        "SELECT id, kind, number, issued_on, taxable_supply_date, total_minor, currency,
                pdf_key IS NOT NULL AS \"pdf_ready!\"
         FROM invoices WHERE order_id = $1 ORDER BY created_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        let currency = Currency::parse(&r.currency)
            .ok_or_else(|| Error::Internal(format!("stored currency {}", r.currency)))?;
        Ok(InvoiceSummary {
            id: r.id,
            kind: if r.kind == "credit_note" {
                DocumentKind::CreditNote
            } else {
                DocumentKind::Invoice
            },
            number: r.number,
            issued_on: r.issued_on,
            taxable_supply_date: r.taxable_supply_date,
            total: Money::new(r.total_minor, currency).view(Locale::from_tag(&locale)),
            pdf_ready: r.pdf_ready,
        })
    })
    .collect()
}

/// The order's documents whose PDF is rendered, with their private-bucket keys (customer
/// downloads).
pub async fn rendered_for_order(
    tx: &mut TenantTx,
    order_id: Uuid,
) -> Result<Vec<(InvoiceSummary, String)>, Error> {
    let keys = sqlx::query!(
        r#"SELECT id, pdf_key AS "pdf_key!" FROM invoices
           WHERE order_id = $1 AND pdf_key IS NOT NULL"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(list_for_order(tx, order_id)
        .await?
        .into_iter()
        .filter_map(|d| {
            let key = keys.iter().find(|k| k.id == d.id)?.pdf_key.clone();
            Some((d, key))
        })
        .collect())
}

/// The stored document and its PDF key.
pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<(Uuid, Document, Option<String>), Error> {
    let r = sqlx::query!(
        "SELECT order_id, document, pdf_key FROM invoices WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let doc = serde_json::from_value(r.document)
        .map_err(|e| Error::Internal(format!("stored invoice {id}: {e}")))?;
    Ok((r.order_id, doc, r.pdf_key))
}

/// Records the rendered PDF (the only change an issued document allows).
pub async fn set_pdf(tx: &mut TenantTx, id: Uuid, key: &str) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE invoices SET pdf_key = $2, pdf_rendered_at = now()
         WHERE id = $1 AND pdf_key IS NULL",
        id,
        key
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Private-bucket key of a document's PDF; the last segment is the file name customers see
/// (`FV202600001.pdf`).
pub fn pdf_key(tenant_id: Uuid, id: Uuid, number: &str) -> String {
    format!("invoices/{tenant_id}/{id}/{number}.pdf")
}

/// Renders a document's PDF into the private bucket and emails it to the customer (the
/// [`RENDER_JOB`]). Idempotent: an existing PDF is kept, the email is keyed per document.
pub async fn render(
    db: &PgPool,
    storage: &platform::storage::Storage,
    typst: &crate::documents::Typst,
    urls: &crate::storefront::PublicUrls,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<(), Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let (order_id, doc, existing) = get(&mut tx, id).await?;
    tx.commit().await?;
    let key = match existing {
        Some(k) => k,
        None => {
            let pdf = typst
                .render(
                    crate::documents::Template::Invoice,
                    &document::render_model(&doc),
                    &[],
                )
                .await?;
            let key = pdf_key(tenant_id, id, &doc.number);
            storage
                .private
                .put(&object_store::path::Path::from(key.as_str()), pdf.into())
                .await?;
            key
        }
    };
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    set_pdf(&mut tx, id, &key).await?;
    let loc = Locale::from_tag(&doc.locale);
    let (template, kind) = match doc.kind {
        DocumentKind::Invoice => (crate::notifications::Template::Invoice, "invoice"),
        DocumentKind::CreditNote => (crate::notifications::Template::CreditNote, "credit_note"),
    };
    let issued = match loc {
        Locale::En => doc.issued_on.format("%-d %b %Y").to_string(),
        _ => doc.issued_on.format("%-d. %-m. %Y").to_string(),
    };
    orders::mail::send(
        &mut tx,
        urls,
        order_id,
        template,
        json!({ "kind": kind, "document": { "number": doc.number, "issued_on": issued,
                "total": Money::new(doc.totals.gross_minor, doc.currency).format(loc) } }),
        format!("invoice_mail:{id}"),
        &[crate::notifications::AttachmentRef {
            key: key.clone(),
            filename: format!("{}.pdf", doc.number),
            content_type: "application/pdf".into(),
        }],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
