//! WP13b against Postgres (runtime role) and in-memory storage: CSV imports (customers,
//! archived orders, subscribers with consent evidence), the tenant export zip, GDPR access
//! and erasure, and tenant isolation of the new tables (spec §10.8, §14, A20, A28, A29).
#![allow(clippy::unwrap_used)]

use std::io::Read;

use commerce::consent::{self, ConsentPurpose, Subject};
use commerce::marketing::subscribers;
use commerce::portability::export;
use commerce::portability::imports::{
    self, AnalyzeInput, DataImport, Kind, Mapping, NewDataImport, RunStatus,
};
use commerce::privacy::{self, ErasureRequest};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::storage::Storage;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

struct Ctx {
    owner: PgPool,
    runtime: PgPool,
    storage: Storage,
    shop: Shop,
}

async fn setup(db: PgPool) -> Ctx {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    Ctx {
        owner: db,
        runtime,
        storage: testkit::memory_storage(),
        shop,
    }
}

impl Ctx {
    /// Creates a run, "uploads" `csv` where the presigned PUT would, runs the dry run.
    async fn analyzed(&self, kind: Kind, csv: &str, mapping: Mapping) -> DataImport {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let created = imports::create(
            &mut tx,
            &self.storage,
            "boss",
            &NewDataImport {
                kind,
                market_id: self.shop.cz,
                upload_size: csv.len() as u64,
                mapping,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(created.upload.method, "PUT");
        let id = created.import.id;
        let key = Path::from(format!("data-import-uploads/{}/{id}.csv", self.shop.tenant));
        self.storage
            .private
            .put(&key, PutPayload::from(csv.to_owned().into_bytes()))
            .await
            .unwrap();
        self.step(id, "analyze").await
    }

    async fn step(&self, id: Uuid, step: &str) -> DataImport {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        match step {
            "analyze" => imports::analyze(&mut tx, "boss", id, &AnalyzeInput::default())
                .await
                .unwrap(),
            _ => imports::apply(&mut tx, "boss", id).await.unwrap(),
        };
        tx.commit().await.unwrap();
        imports::run_step(&self.runtime, &self.storage, self.shop.tenant, id, step)
            .await
            .unwrap();
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        imports::get(&mut tx, id).await.unwrap()
    }

    async fn import(&self, kind: Kind, csv: &str) -> DataImport {
        let run = self.analyzed(kind, csv, Mapping::new()).await;
        assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
        let run = self.step(run.id, "apply").await;
        assert_eq!(run.status, RunStatus::Applied, "{:?}", run.error);
        run
    }

    async fn count(&self, sql: &str) -> i64 {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(&mut *tx)
            .await
            .unwrap()
    }

    /// Everything an import must never cause (A28): mail, outbox events, other jobs, orders,
    /// stock movements, invoices, analytics events.
    async fn side_effects(&self) -> [i64; 7] {
        let q = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&self.owner)
                .await
                .unwrap()
        };
        [
            self.count("SELECT count(*) FROM email_messages").await,
            q("SELECT count(*) FROM queue.outbox").await,
            q("SELECT count(*) FROM queue.jobs WHERE kind <> 'data.import'").await,
            self.count("SELECT count(*) FROM orders").await,
            self.count("SELECT count(*) FROM stock_movements").await,
            self.count("SELECT count(*) FROM invoices").await,
            self.count("SELECT count(*) FROM events").await,
        ]
    }
}

const CUSTOMERS: &str = "\u{feff}E-mail;Jméno;Telefon;street;city;postal_code;country
anna@example.com;Anna Nová;+420 777 111 222;Dlouhá 1;Praha;110 00;cz
BORIS@example.com;Boris;;;;;
not-an-email;X;;;;;
anna@example.com;Dup;;;;;
";

fn mapping(pairs: &[(&str, &str)]) -> Mapping {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn customers_import_reports_rows_and_reimports_idempotently(db: PgPool) {
    let c = setup(db).await;
    let before = c.side_effects().await;
    let m = mapping(&[("email", "E-mail"), ("name", "Jméno"), ("phone", "Telefon")]);
    let run = c.analyzed(Kind::Customers, CUSTOMERS, m.clone()).await;
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
    let r = run.report.unwrap();
    assert_eq!((r.rows, r.records, r.invalid_rows, r.new), (4, 2, 2, 2));
    assert_eq!(r.headers[0], "E-mail");
    let codes: Vec<(u64, &str)> = r.errors.iter().map(|e| (e.line, e.code.as_str())).collect();
    assert_eq!(codes, [(4, "invalid_email"), (5, "duplicate")]);
    assert_eq!(r.preview[0]["email"], "anna@example.com");
    assert_eq!(c.count("SELECT count(*) FROM customers").await, 0, "dry run writes nothing");

    let run = c.step(run.id, "apply").await;
    assert_eq!(run.status, RunStatus::Applied, "{:?}", run.error);
    assert_eq!((run.progress.created, run.progress.updated), (2, 0));
    assert_eq!(c.count("SELECT count(*) FROM customers").await, 2);
    assert_eq!(
        c.count("SELECT count(*) FROM customers WHERE email = 'boris@example.com' AND password_hash IS NULL AND locale = 'cs'").await,
        1
    );
    assert_eq!(c.count("SELECT count(*) FROM customer_addresses WHERE is_default AND country = 'CZ'").await, 1);
    // The CSV (personal data) is gone once applied.
    let key = Path::from(format!("data-imports/{}/{}.csv", c.shop.tenant, run.id));
    assert!(c.storage.private.head(&key).await.is_err());

    // Same file again: updates, no duplicates (customers, addresses).
    let again = c.analyzed(Kind::Customers, CUSTOMERS, m).await;
    assert_eq!(again.report.as_ref().unwrap().existing, 2);
    let again = c.step(again.id, "apply").await;
    assert_eq!((again.progress.created, again.progress.updated), (0, 2));
    assert_eq!(c.count("SELECT count(*) FROM customers").await, 2);
    assert_eq!(c.count("SELECT count(*) FROM customer_addresses").await, 1);
    assert_eq!(c.side_effects().await, before, "customer imports have no side effects");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_bad_file_or_mapping_fails_the_run_and_can_be_reanalyzed(db: PgPool) {
    let c = setup(db).await;
    let run = c.analyzed(Kind::Customers, "Jmeno\nAnna\n", Mapping::new()).await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("email"));
    // A new mapping fixes it without a new upload.
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    imports::analyze(&mut tx, "boss", run.id, &AnalyzeInput { mapping: Some(mapping(&[("email", "Jmeno")])) })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    imports::run_step(&c.runtime, &c.storage, c.shop.tenant, run.id, "analyze")
        .await
        .unwrap();
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let run = imports::get(&mut tx, run.id).await.unwrap();
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
    assert_eq!(run.report.unwrap().errors[0].code, "invalid_email");
    // Unknown mapping fields and applying before the dry run are refused.
    let err = imports::analyze(&mut tx, "boss", run.id, &AnalyzeInput { mapping: Some(mapping(&[("password", "x")])) })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a customers field"), "{err}");
}

const ORDERS: &str = "order_number,placed_at,email,currency,total,status,name,street,city,postal_code,country,sku,item_name,quantity,unit_price
A-1,2024-03-01 10:00,anna@example.com,CZK,\"300,00\",Vyřízeno,Anna Nová,Dlouhá 1,Praha,11000,CZ,TEE,Tričko,2,100
A-1,2024-03-01 10:00,anna@example.com,CZK,\"300,00\",Vyřízeno,Anna Nová,Dlouhá 1,Praha,11000,CZ,MUG,Hrnek,1,100
A-2,2024-04-01,guest@example.com,EUR,12.5,,,,,,,,Kniha,1,12.5
A-3,2024-04-01,guest@example.com,EUR,12.5,,,,,,,,Kniha,-1,12.5
";

#[sqlx::test(migrations = "../../migrations")]
async fn historical_orders_are_archived_without_side_effects(db: PgPool) {
    let c = setup(db).await;
    c.import(Kind::Customers, "email,name\nanna@example.com,Anna\n").await;
    let stock_before = c.count("SELECT coalesce(sum(on_hand), 0)::bigint FROM inventory_levels").await;
    let before = c.side_effects().await;

    let run = c.analyzed(Kind::Orders, ORDERS, Mapping::new()).await;
    let r = run.report.clone().unwrap();
    assert_eq!((r.rows, r.records, r.invalid_rows), (4, 2, 1), "{:?}", r.errors);
    assert_eq!(r.counts["lines"], 3);
    assert_eq!(r.counts["linked_to_customer"], 1);
    let run = c.step(run.id, "apply").await;
    assert_eq!(run.progress.created, 2);

    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let a1 = sqlx::query!(
        r#"SELECT placed_at, total_minor, currency, status_label, customer_id, address, lines,
                  jsonb_array_length(lines) AS "n!"
           FROM archived_orders WHERE number = 'A-1'"#
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(a1.placed_at.to_rfc3339(), "2024-03-01T09:00:00+00:00");
    assert_eq!((a1.total_minor, a1.currency.as_str(), a1.n), (30_000, "CZK", 2));
    assert_eq!(a1.status_label.as_deref(), Some("Vyřízeno"));
    assert!(a1.customer_id.is_some(), "linked to the imported customer");
    assert_eq!(a1.address.unwrap()["city"], "Praha");
    assert_eq!(a1.lines[1]["sku"], "MUG");

    // No side effects of any kind (A28), stock untouched.
    assert_eq!(c.side_effects().await, before);
    assert_eq!(
        c.count("SELECT coalesce(sum(on_hand), 0)::bigint FROM inventory_levels").await,
        stock_before
    );

    // Re-import replaces instead of duplicating; the archive list pages newest first.
    let run = c.import(Kind::Orders, ORDERS).await;
    assert_eq!((run.progress.created, run.progress.updated), (0, 2));
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let page = commerce::portability::archived::list(
        &mut tx,
        &commerce::portability::archived::ArchivedOrderFilter {
            q: Some("GUEST@".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, "A-2");
}

#[sqlx::test(migrations = "../../migrations")]
async fn subscribers_without_evidence_never_become_marketable(db: PgPool) {
    let c = setup(db).await;
    // Existing states: one unsubscribed, one suppressed, one who withdrew consent recently.
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO subscribers (tenant_id, email, status, locale, market_id, text_version, source,
                                  unsubscribed_at)
         VALUES ($1, 'gone@example.com', 'unsubscribed', 'cs', $2, 'v1', 'footer', now())",
    )
    .bind(c.shop.tenant)
    .bind(c.shop.cz)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO email_suppressions (tenant_id, email, reason) VALUES ($1, 'bounced@example.com', 'bounce')")
        .bind(c.shop.tenant)
        .execute(&mut *tx)
        .await
        .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("withdrew@example.com".into()),
        ConsentPurpose::EmailMarketing,
        false,
        "v1",
        "unsubscribe",
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = c.side_effects().await;

    let csv = "email,consent_at,consent_source,consent_ip,consent_text_version
yes@example.com,2023-05-01T08:00:00Z,old shop checkout box,192.0.2.7,v3
nothing@example.com,,,,
gone@example.com,2023-05-01,old shop,,
bounced@example.com,2023-05-01,old shop,,
withdrew@example.com,2023-05-01,old shop,,
";
    let run = c.analyzed(Kind::Subscribers, csv, Mapping::new()).await;
    let counts = run.report.clone().unwrap().counts;
    assert_eq!(counts.get("pending_not_marketable"), Some(&2), "{counts:?}");
    assert_eq!(counts.get("kept_unsubscribed"), Some(&1), "{counts:?}");
    let run = c.step(run.id, "apply").await;
    let o = &run.progress.outcomes;
    assert_eq!(o.get("subscribed"), Some(&1), "{o:?}");
    assert_eq!(o.get("pending_not_marketable"), Some(&3), "{o:?}");
    assert_eq!(o.get("kept_unsubscribed"), Some(&1), "{o:?}");

    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let rows = sqlx::query!("SELECT id, email, status, confirmed_at, consent_evidence FROM subscribers ORDER BY email")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    let by = |e: &str| rows.iter().find(|r| r.email == e).unwrap();
    let yes = by("yes@example.com");
    assert_eq!(yes.status, "subscribed");
    assert_eq!(yes.confirmed_at.unwrap().to_rfc3339(), "2023-05-01T08:00:00+00:00");
    let ev = yes.consent_evidence.as_ref().unwrap();
    assert_eq!((ev["source"].as_str(), ev["ip"].as_str(), ev["text_version"].as_str()), (Some("old shop checkout box"), Some("192.0.2.7"), Some("v3")));
    assert_eq!(subscribers::may_receive(&mut tx, yes.id).await.unwrap(), None);
    for e in ["nothing@example.com", "bounced@example.com", "withdrew@example.com"] {
        assert_eq!(by(e).status, "pending", "{e}");
        assert!(subscribers::may_receive(&mut tx, by(e).id).await.unwrap().is_some(), "{e}");
    }
    assert_eq!(by("gone@example.com").status, "unsubscribed");
    // The consent record carries the evidence time; the later withdrawal still wins.
    let grant = sqlx::query_scalar!(
        "SELECT at FROM consent_records WHERE subject_id = 'yes@example.com' AND source = 'import'"
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(grant.to_rfc3339(), "2023-05-01T08:00:00+00:00");
    assert_eq!(
        consent::latest(&mut tx, &Subject::Email("withdrew@example.com".into()), ConsentPurpose::EmailMarketing).await.unwrap(),
        Some(false)
    );
    tx.commit().await.unwrap();
    // Nothing was mailed (no confirmation either); re-import adds no second record.
    assert_eq!(c.side_effects().await, before);
    c.import(Kind::Subscribers, csv).await;
    assert_eq!(c.count("SELECT count(*) FROM consent_records WHERE source = 'import'").await, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenant_export_zips_every_table_without_secrets(db: PgPool) {
    let c = setup(db).await;
    let other = testkit::storefront::shop(&c.runtime, "other").await;
    c.import(Kind::Customers, "email,name\nanna@example.com,Anna\n").await;
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    sqlx::query("UPDATE customers SET password_hash = '$argon2id$v=19$secret'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO subscribers (tenant_id, email, locale, market_id, text_version, source,
                                  confirm_token_hash, confirm_expires_at, request_ip_hash)
         VALUES ($1, 'news@example.com', 'cs', $2, 'v1', 'footer', $3, now() + interval '1 day', $3)",
    )
    .bind(c.shop.tenant)
    .bind(c.shop.cz)
    .bind(vec![7u8; 32])
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = tenant_tx(&c.runtime, other.tenant).await.unwrap();
    sqlx::query("INSERT INTO customers (tenant_id, email, locale) VALUES ($1, 'foreign@example.com', 'cs')")
        .bind(other.tenant)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let e = export::create(&mut tx, "boss").await.unwrap();
    assert!(export::create(&mut tx, "boss").await.is_err(), "one export at a time");
    tx.commit().await.unwrap();
    export::run(&c.runtime, &c.storage, c.shop.tenant, e.id).await.unwrap();
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let done = export::get(&mut tx, e.id).await.unwrap();
    assert_eq!(done.status, export::ExportStatus::Ready);
    let link = export::download(&mut tx, &c.storage, "boss", e.id).await.unwrap();
    assert!(link.url.contains("X-Amz-Expires=300"), "{}", link.url);
    tx.commit().await.unwrap();

    let key = Path::from(format!("exports/{}/{}.zip", c.shop.tenant, e.id));
    let bytes = c.storage.private.get(&key).await.unwrap().bytes().await.unwrap();
    assert_eq!(done.size_bytes, Some(bytes.len() as i64));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    for want in ["customers.jsonl", "orders.jsonl", "products.jsonl", "subscribers.jsonl", "consent_records.jsonl", "audit_log.jsonl", "assets-manifest.jsonl", "README.txt"] {
        assert!(names.iter().any(|n| n == want), "{want} missing from {names:?}");
    }
    for never in ["customer_sessions.jsonl", "order_tokens.jsonl", "data_exports.jsonl", "idempotency_keys.jsonl"] {
        assert!(!names.iter().any(|n| n == never), "{never} exported");
    }
    let mut all = String::new();
    for i in 0..zip.len() {
        zip.by_index(i).unwrap().read_to_string(&mut all).unwrap();
    }
    assert!(all.contains("anna@example.com"));
    assert!(!all.contains("foreign@example.com"), "another tenant's data leaked");
    assert!(!all.contains("argon2id") && !all.contains("password_hash"));
    assert!(!all.contains("confirm_token_hash") && !all.contains("\\\\x"), "bytea leaked");
    let mut customers = String::new();
    zip.by_name("customers.jsonl").unwrap().read_to_string(&mut customers).unwrap();
    let row: serde_json::Value = serde_json::from_str(customers.lines().next().unwrap()).unwrap();
    assert_eq!(row["email"], "anna@example.com");
}

async fn finished_order(c: &Ctx, email: &str) -> Uuid {
    let id = testkit::storefront::raw_order(&c.runtime, &c.shop, c.shop.cz, "CZK", 12_900, 1, "delivered").await;
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET email = $2, notes = 'ring twice', phone = '+420 777 000 000' WHERE id = $1")
        .bind(id)
        .bind(email)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO order_addresses (tenant_id, order_id, kind, name, street, city, postal_code, country)
         VALUES ($1, $2, 'shipping', 'Anna Nová', 'Dlouhá 1', 'Praha', '11000', 'CZ')",
    )
    .bind(c.shop.tenant)
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO withdrawals (tenant_id, order_id, email, locale, channel, status, declaration,
                                  iban, refund_due_at, refunded_at)
         VALUES ($1, $2, $3, 'cs', 'web', 'refunded', 'I, Anna Nová, withdraw', 'CZ6508000000192000145399',
                 now(), now())",
    )
    .bind(c.shop.tenant)
    .bind(id)
    .bind(email)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO order_events (tenant_id, order_id, kind, data, actor)
         VALUES ($1, $2, 'note', '{\"note\": \"Anna called\"}', 'staff')",
    )
    .bind(c.shop.tenant)
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let series: Uuid = sqlx::query_scalar(
        "INSERT INTO invoice_series (tenant_id, kind, year, prefix) VALUES ($1, 'invoice', 2026, 'FV')
         ON CONFLICT (tenant_id, kind, year) DO UPDATE SET prefix = 'FV' RETURNING id",
    )
    .bind(c.shop.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO invoices (tenant_id, series_id, kind, number, order_id, issued_on,
                               taxable_supply_date, due_on, currency, vat_payer, document,
                               total_minor, created_by)
         VALUES ($1, $2, 'invoice', 'FV' || lpad((floor(random() * 1e9))::bigint::text, 9, '0'),
                 $3, current_date, current_date, current_date, 'CZK', true,
                 '{\"customer\": {\"name\": \"Anna Nová\"}}', 12900, 'test')",
    )
    .bind(c.shop.tenant)
    .bind(series)
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[sqlx::test(migrations = "../../migrations")]
async fn access_and_erasure_of_a_data_subject(db: PgPool) {
    let c = setup(db).await;
    c.import(Kind::Customers, "email,name,street,city,postal_code,country\nanna@example.com,Anna,Dlouhá 1,Praha,11000,CZ\n").await;
    c.import(Kind::Orders, "order_number,placed_at,email,currency,total,name\nOLD-1,2022-01-01,anna@example.com,CZK,10,Anna\n").await;
    c.import(Kind::Subscribers, "email,consent_at,consent_source\nanna@example.com,2022-01-01,old shop\n").await;
    let order = finished_order(&c, "anna@example.com").await;
    let open = testkit::storefront::raw_order(&c.runtime, &c.shop, c.shop.cz, "CZK", 100, 1, "processing").await;
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET email = 'anna@example.com' WHERE id = $1").bind(open).execute(&mut *tx).await.unwrap();
    let cid: Uuid = sqlx::query_scalar("SELECT id FROM customers WHERE email = 'anna@example.com'").fetch_one(&mut *tx).await.unwrap();
    consent::record_server(&mut tx, &Subject::Customer(cid), ConsentPurpose::Analytics, true, "v1", "admin", None).await.unwrap();
    tx.commit().await.unwrap();

    // Access: everything in one document.
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let doc = privacy::access(&mut tx, "boss", " Anna@Example.com ").await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(doc["customer"]["email"], "anna@example.com");
    assert!(doc["customer"].get("password_hash").is_none());
    assert_eq!(doc["orders"].as_array().unwrap().len(), 2);
    assert_eq!(doc["orders"][0]["addresses"][0]["city"], "Praha");
    assert_eq!(doc["archived_orders"][0]["number"], "OLD-1");
    assert_eq!(doc["invoices"].as_array().unwrap().len(), 1);
    assert_eq!(doc["newsletter"]["status"], "subscribed");
    assert!(doc["newsletter"].get("confirm_token_hash").is_none());
    assert_eq!(doc["consents"].as_array().unwrap().len(), 2);

    // Erasure: confirmation must match; an open order blocks it.
    let req = |confirm: &str| ErasureRequest { email: "anna@example.com".into(), confirm_email: confirm.into() };
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let err = privacy::erase(&mut tx, "boss", &req("other@example.com")).await.unwrap_err();
    assert!(err.to_string().contains("repeat"), "{err}");
    let err = privacy::erase(&mut tx, "boss", &req("ANNA@example.com")).await.unwrap_err();
    assert!(matches!(err, platform::Error::Conflict { code: "erasure_blocked", .. }), "{err}");
    sqlx::query("UPDATE orders SET status = 'cancelled' WHERE id = $1").bind(open).execute(&mut *tx).await.unwrap();
    let report = privacy::erase(&mut tx, "boss", &req("ANNA@example.com")).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(report.customer_id, Some(cid));
    assert_eq!((report.orders_anonymized, report.archived_orders_anonymized), (2, 1));
    assert_eq!((report.invoices_retained, report.subscribers_deleted), (1, 1));
    assert_eq!(report.consent_records_pseudonymized, 2);

    let n = |sql: &'static str| c.count(sql);
    assert_eq!(n("SELECT count(*) FROM customers").await, 0);
    assert_eq!(n("SELECT count(*) FROM customer_addresses").await, 0);
    assert_eq!(n("SELECT count(*) FROM subscribers").await, 0);
    assert_eq!(n("SELECT count(*) FROM orders WHERE email = 'erased@erased.invalid' AND notes IS NULL AND phone IS NULL AND customer_id IS NULL").await, 2);
    assert_eq!(n("SELECT count(*) FROM order_addresses WHERE name = '[erased]' AND country = 'CZ'").await, 1);
    assert_eq!(n("SELECT count(*) FROM archived_orders WHERE email = 'erased@erased.invalid' AND name IS NULL").await, 1);
    assert_eq!(n("SELECT count(*) FROM consent_records WHERE subject_id LIKE 'erased-%'").await, 2);
    assert_eq!(n("SELECT count(*) FROM consent_records WHERE subject_id IN ('anna@example.com')").await, 0);
    assert_eq!(n("SELECT count(*) FROM withdrawals WHERE email = 'erased@erased.invalid' AND iban IS NULL AND declaration = '[erased]'").await, 1);
    assert_eq!(n("SELECT count(*) FROM order_events WHERE kind = 'note' AND data = '{}'").await, 1);
    // Invoices stay exactly as issued (tax law).
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let doc: serde_json::Value = sqlx::query_scalar("SELECT document FROM invoices WHERE order_id = $1").bind(order).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(doc["customer"]["name"], "Anna Nová");
    // The audit entry names no one.
    let audit: serde_json::Value = sqlx::query_scalar("SELECT diff FROM audit_log WHERE action = 'privacy.erased'").fetch_one(&mut *tx).await.unwrap();
    assert!(!audit.to_string().contains("anna"), "{audit}");
    // Nothing is left to find.
    let doc = privacy::access(&mut tx, "boss", "anna@example.com").await.unwrap();
    assert!(doc["customer"].is_null() && doc["orders"].as_array().unwrap().is_empty(), "{doc}");
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn new_tables_are_tenant_isolated(db: PgPool) {
    let c = setup(db).await;
    c.import(Kind::Orders, "order_number,placed_at,email,currency,total\nA-1,2024-01-01,a@example.com,CZK,1\n").await;
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    export::create(&mut tx, "boss").await.unwrap();
    tx.commit().await.unwrap();

    let other = testkit::storefront::shop(&c.runtime, "iso2").await;
    let mut tx = tenant_tx(&c.runtime, other.tenant).await.unwrap();
    for table in ["data_imports", "archived_orders", "data_exports"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
        let touched = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET tenant_id = tenant_id")))
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(touched, 0, "{table} writable across tenants");
    }
    let err = sqlx::query(
        "INSERT INTO archived_orders (tenant_id, number, placed_at, email, currency, total_minor)
         VALUES ($1, 'X', now(), 'x@example.com', 'CZK', 1)",
    )
    .bind(c.shop.tenant)
    .execute(&mut *tx)
    .await;
    assert!(err.is_err(), "RLS WITH CHECK refuses another tenant's rows");
    // Consent erasure only touches the current tenant.
    tx.rollback().await.unwrap();
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    consent::record_server(&mut tx, &Subject::Email("same@example.com".into()), ConsentPurpose::EmailMarketing, true, "v1", "admin", None).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = tenant_tx(&c.runtime, other.tenant).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT platform.erase_consent_subject('email', 'same@example.com')")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0, "erasure reached another tenant's consent records");
}
