//! Personal data (spec §14, A29): salted IP hashes, and GDPR access (art. 15) and erasure
//! (art. 17) of a data subject: an email address together with the customer account of that
//! address, if any.

use std::net::IpAddr;

use object_store::ObjectStoreExt;
use platform::Error;
use platform::db::TenantTx;
use platform::queue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

/// SHA-256 of the address under today's salt (UTC day). Salts are random, per day and deleted
/// after two days (`platform.purge_customer_auth`), so a stored hash can be compared with
/// others from the same day (rate limits) but never turned back into an address.
pub async fn ip_hash(conn: &mut PgConnection, ip: IpAddr) -> Result<Vec<u8>, sqlx::Error> {
    let fresh: [u8; 32] = rand::random();
    // Two statements: when two requests create the day's salt at once, the loser's INSERT
    // waits for the winner and does nothing; the SELECT then runs with a fresh snapshot
    // (read committed) and sees the winner's row.
    sqlx::query!(
        "INSERT INTO platform.ip_salts (day, salt) VALUES ((now() AT TIME ZONE 'utc')::date, $1)
         ON CONFLICT (day) DO NOTHING",
        &fresh[..]
    )
    .execute(&mut *conn)
    .await?;
    let salt = sqlx::query_scalar!(
        "SELECT salt FROM platform.ip_salts WHERE day = (now() AT TIME ZONE 'utc')::date"
    )
    .fetch_one(&mut *conn)
    .await?;
    let mut h = Sha256::new();
    h.update(&salt);
    h.update(ip.to_string().as_bytes());
    Ok(h.finalize().to_vec())
}

/// What erased addresses become (RFC 2606 `.invalid`: never deliverable).
pub const ERASED_EMAIL: &str = "erased@erased.invalid";
const ERASED: &str = "[erased]";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErasureRequest {
    pub email: String,
    /// The same address again: erasure cannot be undone.
    pub confirm_email: String,
}

/// What an erasure did. Counts only: the report holds no personal data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ErasureReport {
    /// The customer account that was deleted.
    pub customer_id: Option<Uuid>,
    pub orders_anonymized: i64,
    pub archived_orders_anonymized: i64,
    /// Invoices and credit notes of the subject's orders, kept unchanged (tax law).
    pub invoices_retained: i64,
    pub subscribers_deleted: i64,
    pub consent_records_pseudonymized: i64,
    pub emails_anonymized: i64,
    /// Private files (labels, document sheets, exports, import CSVs) queued for deletion.
    pub files_deleted: i64,
}

struct Subject {
    email: String,
    customer_id: Option<Uuid>,
}

async fn subject(tx: &mut TenantTx, raw: &str) -> Result<Subject, Error> {
    let email = crate::staff::normalize_email(raw)?;
    let customer_id = sqlx::query_scalar!("SELECT id FROM customers WHERE email = $1", email)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(Subject { email, customer_id })
}

async fn order_ids(tx: &mut TenantTx, s: &Subject) -> Result<Vec<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM orders WHERE email = $1 OR customer_id = $2 ORDER BY placed_at",
        s.email,
        s.customer_id
    )
    .fetch_all(&mut **tx)
    .await?)
}

fn count(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Everything the shop holds about the subject, as one JSON document (audited). Secrets that
/// are not the subject's information (password hash, token and IP hashes) are left out.
pub async fn access(tx: &mut TenantTx, actor: &str, raw_email: &str) -> Result<Value, Error> {
    let s = subject(tx, raw_email).await?;
    let ids = order_ids(tx, &s).await?;
    let customer = sqlx::query_scalar!(
        r#"SELECT to_jsonb(c) - 'password_hash' AS "v!" FROM customers c WHERE c.id = $1"#,
        s.customer_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let addresses = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.created_at), '[]') AS "v!"
           FROM customer_addresses a WHERE a.customer_id = $1"#,
        s.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let orders = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(o) || jsonb_build_object(
                   'lines', (SELECT coalesce(jsonb_agg(to_jsonb(l) ORDER BY l.position), '[]')
                             FROM order_lines l WHERE l.order_id = o.id),
                   'addresses', (SELECT coalesce(jsonb_agg(to_jsonb(a)), '[]')
                                 FROM order_addresses a WHERE a.order_id = o.id),
                   'events', (SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY e.at), '[]')
                              FROM order_events e WHERE e.order_id = o.id))
                   ORDER BY o.placed_at), '[]') AS "v!"
           FROM orders o WHERE o.id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let archived = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.placed_at), '[]') AS "v!"
           FROM archived_orders a WHERE a.email = $1 OR a.customer_id = $2"#,
        s.email,
        s.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let invoices = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(jsonb_build_object(
                   'number', i.number, 'kind', i.kind, 'order_id', i.order_id,
                   'issued_on', i.issued_on, 'currency', i.currency,
                   'total_minor', i.total_minor, 'document', i.document)
                   ORDER BY i.created_at), '[]') AS "v!"
           FROM invoices i WHERE i.order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let withdrawals = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(w) ORDER BY w.declared_at), '[]') AS "v!"
           FROM withdrawals w WHERE w.order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let newsletter = sqlx::query_scalar!(
        r#"SELECT to_jsonb(s) - 'confirm_token_hash' - 'request_ip_hash' - 'confirm_ip_hash'
                  AS "v!"
           FROM subscribers s WHERE s.email = $1"#,
        s.email
    )
    .fetch_optional(&mut **tx)
    .await?;
    let consents = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(jsonb_build_object(
                   'purpose', c.purpose, 'granted', c.granted, 'text_version', c.text_version,
                   'source', c.source, 'at', c.at, 'subject_type', c.subject_type)
                   ORDER BY c.at), '[]') AS "v!"
           FROM consent_records c
           WHERE (c.subject_type = 'email' AND c.subject_id = $1)
              OR (c.subject_type = 'customer' AND c.subject_id = $2::uuid::text)"#,
        s.email,
        s.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let emails = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(jsonb_build_object(
                   'template', m.template, 'stream', m.stream, 'subject', m.subject,
                   'status', m.status, 'created_at', m.created_at) ORDER BY m.created_at), '[]')
                  AS "v!"
           FROM email_messages m WHERE lower(m.to_email) = $1"#,
        s.email
    )
    .fetch_one(&mut **tx)
    .await?;
    let events = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(jsonb_build_object('type', e.type, 'at', e.at,
                   'props', e.props) ORDER BY e.at), '[]') AS "v!"
           FROM events e WHERE e.customer_id = $1"#,
        s.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let carts = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(c) - 'shop_token_hash' - 'checkout_token_hash'
                   ORDER BY c.created_at), '[]') AS "v!"
           FROM carts c
           WHERE lower(btrim(c.email)) = $1 OR c.customer_id = $2
              OR c.id IN (SELECT o.cart_id FROM orders o WHERE o.id = ANY($3))"#,
        s.email,
        s.customer_id,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let shipments = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(x) - 'label_key' ORDER BY x.created_at), '[]')
                  AS "v!"
           FROM shipments x WHERE x.order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let bank_transactions = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(b) ORDER BY b.booked_on), '[]') AS "v!"
           FROM bank_transactions b
           WHERE b.attempt_id IN (SELECT p.id FROM payment_attempts p WHERE p.order_id = ANY($1))"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let payments = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(p) ORDER BY p.created_at), '[]') AS "v!"
           FROM payment_attempts p WHERE p.order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let refunds = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY r.created_at), '[]') AS "v!"
           FROM refunds r WHERE r.order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    let campaign_sends = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(c) - 'token_hash' ORDER BY c.created_at), '[]')
                  AS "v!"
           FROM campaign_sends c JOIN subscribers s ON s.id = c.subscriber_id
           WHERE s.email = $1"#,
        s.email
    )
    .fetch_one(&mut **tx)
    .await?;
    let affinity = sqlx::query_scalar!(
        r#"SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.dim, a.key), '[]') AS "v!"
           FROM customer_affinity a WHERE a.customer_id = $1"#,
        s.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    crate::audit::record(
        tx,
        actor,
        "privacy.accessed",
        "data_subject",
        s.customer_id.map(|c| c.to_string()).as_deref(),
        &json!({ "orders": ids.len() }),
    )
    .await?;
    Ok(json!({
        "email": s.email,
        "generated_at": chrono::Utc::now(),
        "customer": customer,
        "addresses": addresses,
        "orders": orders,
        "archived_orders": archived,
        "invoices": invoices,
        "withdrawals": withdrawals,
        "newsletter": newsletter,
        "consents": consents,
        "emails": emails,
        "analytics_events": events,
        "carts": carts,
        "shipments": shipments,
        "payments": payments,
        "refunds": refunds,
        "bank_transactions": bank_transactions,
        "campaign_sends": campaign_sends,
        "recommendation_affinity": affinity,
    }))
}

/// Erases a data subject. Refused while something still needs the data: an order not yet
/// delivered, cancelled or returned, an unrefunded withdrawal or a pending refund.
///
/// Also refused while a CSV import is being checked or applied or a data export is being
/// prepared (either could bring the data back).
///
/// Deleted: the customer account (addresses, sessions, affinity cascade), the newsletter
/// subscriber (its campaign sends cascade), analytics and ad-forwarding rows, suppressions,
/// sign-in links and rate-limit rows, delivered webhook payloads naming the subject, and (by
/// a queued job, retried until done) label PDFs, packing-slip/label sheets, previous data
/// exports and the files of imports not yet applied. Anonymized: orders and their addresses,
/// notes and address-change events, archived orders, carts, withdrawals, the mail log, import
/// reports. Consent records get a random subject id (anonymous evidence). Kept as issued:
/// invoices, credit notes and bank transactions (tax and accounting law). Audited with counts
/// only.
pub async fn erase(
    tx: &mut TenantTx,
    actor: &str,
    req: &ErasureRequest,
) -> Result<ErasureReport, Error> {
    let s = subject(tx, &req.email).await?;
    if crate::staff::normalize_email(&req.confirm_email)
        .ok()
        .as_deref()
        != Some(s.email.as_str())
    {
        return Err(Error::Validation {
            code: "confirmation_mismatch",
            detail: "confirm_email must repeat the address".into(),
        });
    }
    // Imports and exports take this lock before they start, so none can begin between the
    // busy check below and the commit.
    crate::portability::imports::lock_tenant(tx).await?;
    // The same row locks as refunds and withdrawals take (orders::lock), in a fixed order,
    // so nothing can start on these orders between the checks and the anonymization.
    let ids = sqlx::query_scalar!(
        "SELECT id FROM orders WHERE email = $1 OR customer_id = $2 ORDER BY id FOR UPDATE",
        s.email,
        s.customer_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let busy = sqlx::query_scalar!(
        r#"SELECT (EXISTS (SELECT 1 FROM data_imports WHERE status IN ('analyzing', 'applying'))
                   OR EXISTS (SELECT 1 FROM data_exports WHERE status IN ('pending', 'running')))
                  AS "busy!""#
    )
    .fetch_one(&mut **tx)
    .await?;
    if busy {
        return Err(Error::Conflict {
            code: "erasure_blocked",
            detail: "a CSV import or a data export is running; try again when it has finished"
                .into(),
        });
    }
    let blocking = sqlx::query_scalar!(
        "SELECT o.number FROM orders o WHERE o.id = ANY($1) AND (
             o.status NOT IN ('delivered', 'cancelled', 'returned')
             OR EXISTS (SELECT 1 FROM withdrawals w WHERE w.order_id = o.id AND w.status = 'open')
             OR EXISTS (SELECT 1 FROM refunds r WHERE r.order_id = o.id AND r.status = 'pending'))
         ORDER BY o.number LIMIT 10",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?;
    if !blocking.is_empty() {
        let numbers: Vec<String> = blocking.iter().map(i64::to_string).collect();
        return Err(Error::Conflict {
            code: "erasure_blocked",
            detail: format!(
                "orders still in progress (not finished, an open withdrawal or a pending refund): {}",
                numbers.join(", ")
            ),
        });
    }
    let mut report = ErasureReport {
        customer_id: s.customer_id,
        ..ErasureReport::default()
    };
    report.orders_anonymized = count(
        sqlx::query!(
            "UPDATE orders SET email = $2, phone = NULL, notes = NULL, exception_note = NULL,
                 customer_id = NULL, updated_at = now()
             WHERE id = ANY($1)",
            &ids,
            ERASED_EMAIL
        )
        .execute(&mut **tx)
        .await?
        .rows_affected(),
    );
    sqlx::query!(
        "UPDATE order_addresses SET name = $2, company = NULL, street = $2, city = $2,
             postal_code = '-', phone = NULL
         WHERE order_id = ANY($1)",
        &ids,
        ERASED
    )
    .execute(&mut **tx)
    .await?;
    // The timeline and withdrawal declarations are immutable for the application role;
    // erasure goes through the one function allowed to rewrite them.
    sqlx::query!(
        "SELECT platform.erase_order_records($1, $2, $3)",
        &ids,
        ERASED_EMAIL,
        ERASED
    )
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query!("DELETE FROM order_tokens WHERE order_id = ANY($1)", &ids)
        .execute(&mut **tx)
        .await?;
    // Files holding the subject's data: labels, packing-slip/label sheets, earlier exports,
    // the CSVs of imports not applied yet. Deleted by a retried job once this commits.
    let mut objects = sqlx::query_scalar!(
        r#"SELECT label_key AS "k!" FROM shipments
           WHERE order_id = ANY($1) AND label_key IS NOT NULL"#,
        &ids
    )
    .fetch_all(&mut **tx)
    .await?;
    objects.extend(
        sqlx::query_scalar!(
            r#"UPDATE documents d SET status = 'failed', object_key = NULL,
                   error = 'removed by a personal-data erasure', updated_at = now()
               FROM documents old
               WHERE old.id = d.id AND d.order_ids && $1
                 AND (d.object_key IS NOT NULL OR d.status = 'pending')
               RETURNING old.object_key"#,
            &ids
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .flatten(),
    );
    objects.extend(
        sqlx::query_scalar!("DELETE FROM data_exports RETURNING object_key")
            .fetch_all(&mut **tx)
            .await?,
    );
    let tenant = tx.tenant_id();
    // Runs not applied yet are dropped (their files may hold the person); applied runs keep
    // their counts. Every run's CSV objects are deleted (applied ones normally are already).
    let mut import_ids = sqlx::query_scalar!(
        "DELETE FROM data_imports WHERE status IN ('pending', 'analyzed', 'failed') RETURNING id"
    )
    .fetch_all(&mut **tx)
    .await?;
    import_ids.extend(
        sqlx::query_scalar!("SELECT id FROM data_imports")
            .fetch_all(&mut **tx)
            .await?,
    );
    for id in import_ids {
        objects.push(format!("data-import-uploads/{tenant}/{id}.csv"));
        objects.push(format!("data-imports/{tenant}/{id}.csv"));
    }
    // Applied imports keep their counts; previews and row errors may quote the subject.
    sqlx::query!(
        r#"UPDATE data_imports SET report = report || '{"preview": [], "errors": []}'
           WHERE status = 'applied' AND strpos(lower(report::text), $1) > 0"#,
        s.email
    )
    .execute(&mut **tx)
    .await?;
    if !objects.is_empty() {
        let mut job = queue::NewJob::new(DELETE_OBJECTS_JOB, json!({ "keys": objects }));
        job.tenant_id = Some(tenant);
        job.max_attempts = 10;
        queue::enqueue(&mut **tx, &job).await?;
    }
    report.files_deleted = count(u64::try_from(objects.len()).unwrap_or(u64::MAX));
    report.invoices_retained = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM invoices WHERE order_id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    report.archived_orders_anonymized = count(
        sqlx::query!(
            "UPDATE archived_orders SET email = $3, name = NULL, phone = NULL, address = NULL,
                 customer_id = NULL, updated_at = now()
             WHERE email = $1 OR customer_id = $2",
            s.email,
            s.customer_id,
            ERASED_EMAIL
        )
        .execute(&mut **tx)
        .await?
        .rows_affected(),
    );
    sqlx::query!(
        "UPDATE carts SET email = NULL, phone = NULL, billing_address = NULL,
             shipping_address = NULL, pickup_point = NULL, customer_id = NULL, updated_at = now()
         WHERE lower(btrim(email)) = $1 OR customer_id = $2
            OR id IN (SELECT cart_id FROM orders WHERE id = ANY($3))",
        s.email,
        s.customer_id,
        &ids
    )
    .execute(&mut **tx)
    .await?;
    report.subscribers_deleted = count(
        sqlx::query!(
            "DELETE FROM subscribers WHERE email = $1 OR customer_id = $2",
            s.email,
            s.customer_id
        )
        .execute(&mut **tx)
        .await?
        .rows_affected(),
    );
    report.emails_anonymized = count(
        sqlx::query!(
            "UPDATE email_messages SET to_email = $2, subject = $3, html = NULL,
                 body_text = NULL, list_unsubscribe = NULL, send_token = NULL,
                 status = CASE WHEN status IN ('pending', 'sending') THEN 'failed'
                               ELSE status END,
                 last_error = CASE WHEN status IN ('pending', 'sending') THEN 'recipient erased'
                                   ELSE last_error END,
                 updated_at = now()
             WHERE lower(to_email) = $1",
            s.email,
            ERASED_EMAIL,
            ERASED
        )
        .execute(&mut **tx)
        .await?
        .rows_affected(),
    );
    sqlx::query!("DELETE FROM email_suppressions WHERE email = $1", s.email)
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        "DELETE FROM customer_auth_attempts WHERE lower(email) = $1",
        s.email
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!("DELETE FROM customer_magic_links WHERE email = $1", s.email)
        .execute(&mut **tx)
        .await?;
    // Delivered webhook payloads may carry the address or the orders.
    sqlx::query!(
        "DELETE FROM webhook_deliveries d
         WHERE d.status IN ('succeeded', 'dead')
           AND (strpos(d.payload::text, $1) > 0
                OR EXISTS (SELECT 1 FROM unnest($2::uuid[]) i
                           WHERE strpos(d.payload::text, i::text) > 0))",
        s.email,
        &ids
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM ad_deliveries WHERE customer_id = $1 OR order_id = ANY($2)",
        s.customer_id,
        &ids
    )
    .execute(&mut **tx)
    .await?;
    report.consent_records_pseudonymized = sqlx::query_scalar!(
        r#"SELECT platform.erase_consent_subject('email', $1) AS "n!""#,
        s.email
    )
    .fetch_one(&mut **tx)
    .await?;
    if let Some(cid) = s.customer_id {
        report.consent_records_pseudonymized += sqlx::query_scalar!(
            r#"SELECT platform.erase_consent_subject('customer', $1) AS "n!""#,
            cid.to_string()
        )
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query!("DELETE FROM events WHERE customer_id = $1", cid)
            .execute(&mut **tx)
            .await?;
        // Addresses, sessions and affinity cascade; orders and carts were detached above.
        sqlx::query!("DELETE FROM customers WHERE id = $1", cid)
            .execute(&mut **tx)
            .await?;
    }
    crate::audit::record(
        tx,
        actor,
        "privacy.erased",
        "data_subject",
        s.customer_id.map(|c| c.to_string()).as_deref(),
        &serde_json::to_value(&report).map_err(|e| Error::Internal(e.to_string()))?,
    )
    .await?;
    Ok(report)
}

/// The job deleting private objects holding personal data (an erasure's files, applied import
/// CSVs), payload `{"keys": [...]}`; a missing object counts as deleted, any other failure
/// retries the job.
pub const DELETE_OBJECTS_JOB: &str = "privacy.delete_objects";

pub async fn delete_objects(
    storage: &platform::storage::Storage,
    tenant_id: Uuid,
    keys: &[String],
) -> Result<(), Error> {
    for k in keys {
        // Only this tenant's objects, whatever the payload says.
        if !k
            .split('/')
            .nth(1)
            .is_some_and(|t| t == tenant_id.to_string())
        {
            tracing::warn!(key = %k, "erasure object of another tenant skipped");
            continue;
        }
        let path = object_store::path::Path::from(k.as_str());
        match storage.private.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
