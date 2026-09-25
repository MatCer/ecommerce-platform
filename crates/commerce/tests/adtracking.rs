//! Ad-platform forwarders (§11.3, A20, A21): config with sealed credentials, consent gating at
//! capture and at send time, withdrawal, vendor payloads, retries/dead, pause/resume, refunds
//! and tenant isolation.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::routing::post;
use commerce::adtracking::{
    self, AdTracking, Credentials, DeliveryQuery, Outcome, Platform, PlatformUpdate, Settings,
    vendors::Endpoints,
};
use commerce::analytics::clean_event;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject, new_anon_id};
use commerce::storefront::PublicUrls;
use platform::crypto::SecretBox;
use platform::db::tenant_tx;
use platform::http::SafeClient;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::{Shop, raw_order, shop};
use uuid::Uuid;

type Got = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct Vendor {
    status: Arc<Mutex<u16>>,
    got: Got,
    /// The OAuth token endpoint answers after this many milliseconds.
    token_delay_ms: Arc<Mutex<u64>>,
}

/// One server for every vendor path; records `(path?query, JSON body)`.
async fn vendor() -> (String, Vendor) {
    let v = Vendor {
        status: Arc::new(Mutex::new(200)),
        got: Arc::default(),
        token_delay_ms: Arc::default(),
    };
    async fn any(State(v): State<Vendor>, uri: OriginalUri, body: String) -> (StatusCode, String) {
        let path = uri.0.to_string();
        if path.ends_with("/google/token") {
            let delay = *v.token_delay_ms.lock().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            return (
                StatusCode::OK,
                json!({ "access_token": "ya29.test", "expires_in": 3600 }).to_string(),
            );
        }
        v.got
            .lock()
            .unwrap()
            .push((path, serde_json::from_str(&body).unwrap_or(Value::Null)));
        (
            StatusCode::from_u16(*v.status.lock().unwrap()).unwrap(),
            "{}".into(),
        )
    }
    let app = Router::new().fallback(post(any)).with_state(v.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://localhost:{}/ads",
        listener.local_addr().unwrap().port()
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, v)
}

fn ads(base: &str) -> AdTracking {
    AdTracking::new(
        SecretBox::new(&[7; 32]),
        SafeClient::new(["localhost".to_owned()]).unwrap(),
        Endpoints {
            base: Some(base.to_owned()),
        },
        PublicUrls::default(),
    )
}

fn grant(ads: bool) -> ConsentChoice {
    ConsentChoice {
        purposes: Purposes {
            ads: Some(ads),
            ..Purposes::default()
        },
        text_version: "v1".into(),
        source: Source::Banner,
    }
}

/// Enables all four platforms for the CZ market (and Meta/GA4 for SK).
async fn configure(runtime: &PgPool, a: &AdTracking, s: &Shop) {
    let mut tx = tenant_tx(runtime, s.tenant).await.unwrap();
    let all =
        |markets: Vec<Uuid>, settings: Settings, credentials: Option<Credentials>| PlatformUpdate {
            enabled: Some(true),
            market_ids: Some(markets),
            settings: Some(settings),
            credentials,
            ..PlatformUpdate::default()
        };
    let both = vec![s.cz, s.sk];
    for (p, u) in [
        (
            Platform::Meta,
            all(
                both.clone(),
                Settings {
                    pixel_id: Some("1234567890".into()),
                    ..Settings::default()
                },
                Some(Credentials {
                    access_token: Some("EAAmeta-token".into()),
                    ..Credentials::default()
                }),
            ),
        ),
        (
            Platform::Ga4,
            all(
                both.clone(),
                Settings {
                    measurement_id: Some("G-TEST123".into()),
                    ..Settings::default()
                },
                Some(Credentials {
                    api_secret: Some("ga4-secret".into()),
                    ..Credentials::default()
                }),
            ),
        ),
        (
            Platform::GoogleAds,
            all(
                vec![s.cz],
                Settings {
                    customer_id: Some("123-456-7890".into()),
                    conversion_action_id: Some("555".into()),
                    ..Settings::default()
                },
                Some(Credentials {
                    client_id: Some("cid".into()),
                    client_secret: Some("csecret".into()),
                    refresh_token: Some("1//refresh".into()),
                    ..Credentials::default()
                }),
            ),
        ),
        (
            Platform::Sklik,
            all(
                both,
                Settings {
                    sem_id: Some("sem-s2s".into()),
                    ..Settings::default()
                },
                None,
            ),
        ),
    ] {
        adtracking::update(&mut tx, a, "staff", p, &u)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn consent_for(runtime: &PgPool, tenant: Uuid, anon: &str, ads: bool) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    consent::record(&mut tx, &Subject::Anon(anon.to_owned()), &grant(ads), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn deliveries(runtime: &PgPool, tenant: Uuid) -> Vec<(Uuid, String, String, String)> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let rows = sqlx::query_as(
        "SELECT id, platform, event_name, status FROM ad_deliveries ORDER BY platform, created_at",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rows
}

async fn purchase(runtime: &PgPool, s: &Shop, market: Uuid, currency: &str, anon: &str) -> Uuid {
    let order = raw_order(runtime, s, market, currency, 25_800, 2, "confirmed").await;
    let mut tx = tenant_tx(runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET phone = '606 666 666' WHERE id = $1")
        .bind(order)
        .execute(&mut *tx)
        .await
        .unwrap();
    adtracking::capture_purchase(&mut tx, order, anon, Some("Mozilla/5.0 test"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    order
}

#[sqlx::test(migrations = "../../migrations")]
async fn config_is_validated_sealed_and_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let s = shop(&runtime, "adcfg").await;
    let other = shop(&runtime, "adother").await;
    let a = ads("http://localhost:1/ads");
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    // Enabling needs complete settings and credentials.
    let err = adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Meta,
        &PlatformUpdate {
            enabled: Some(true),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        platform::Error::Validation {
            code: "incomplete_configuration",
            ..
        }
    ));
    // Partial Google credentials are refused.
    let err = adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::GoogleAds,
        &PlatformUpdate {
            credentials: Some(Credentials {
                client_id: Some("x".into()),
                ..Credentials::default()
            }),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        platform::Error::Validation {
            code: "incomplete_credentials",
            ..
        }
    ));
    tx.commit().await.unwrap();
    configure(&runtime, &a, &s).await;

    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let list = adtracking::list(&mut tx).await.unwrap();
    let meta = list
        .items
        .iter()
        .find(|p| p.platform == Platform::Meta)
        .unwrap();
    assert!(meta.enabled && meta.complete && meta.has_credentials);
    assert_eq!(meta.credentials_hint.as_deref(), Some("oken"));
    let listed = serde_json::to_string(&list).unwrap();
    assert!(!listed.contains("EAAmeta-token") && !listed.contains("ga4-secret"));
    let sealed: Vec<u8> = sqlx::query_scalar(
        "SELECT credentials_ciphertext FROM ad_platforms WHERE platform = 'meta'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(
        !String::from_utf8_lossy(&sealed).contains("EAAmeta"),
        "encrypted at rest"
    );
    // Rotating one Google secret keeps the others.
    let g = adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::GoogleAds,
        &PlatformUpdate {
            credentials: Some(Credentials {
                refresh_token: Some("1//rotated-wxyz".into()),
                ..Credentials::default()
            }),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap();
    assert!(g.enabled && g.complete);
    assert_eq!(g.credentials_hint.as_deref(), Some("wxyz"));
    assert_eq!(g.settings.customer_id.as_deref(), Some("1234567890"));
    // Unknown market of another tenant.
    let err = adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Meta,
        &PlatformUpdate {
            market_ids: Some(vec![other.cz]),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        platform::Error::Validation {
            code: "invalid_markets",
            ..
        }
    ));
    tx.commit().await.unwrap();

    // Cross-tenant: the other tenant sees nothing of this one.
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    let theirs = adtracking::list(&mut tx).await.unwrap();
    assert!(
        theirs
            .items
            .iter()
            .all(|p| !p.enabled && !p.has_credentials)
    );
    let n: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM ad_platforms) + (SELECT count(*) FROM ad_deliveries)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(n, 0);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_consented_subjects_are_captured(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let s = shop(&runtime, "adcap").await;
    let a = ads("http://localhost:1/ads");
    configure(&runtime, &a, &s).await;
    let (yes, no, unknown) = (new_anon_id(), new_anon_id(), new_anon_id());
    consent_for(&runtime, s.tenant, &yes, true).await;
    consent_for(&runtime, s.tenant, &no, false).await;
    let events: Vec<_> = [
        json!({ "type": "page_view", "template": "home" }),
        json!({ "type": "view_item", "template": "product", "product_id": s.product }),
        json!({ "type": "web_vital", "name": "LCP", "value": 1.0 }),
    ]
    .iter()
    .filter_map(clean_event)
    .collect();
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let now = chrono::Utc::now();
    for (who, expected) in [(&no, 0), (&unknown, 0), (&yes, 4)] {
        let n =
            adtracking::capture_events(&mut tx, s.cz, Some(who.as_str()), &events, Some("UA"), now)
                .await
                .unwrap();
        assert_eq!(n, expected, "page_view + view_item to Meta and GA4 only");
    }
    let props: Value = sqlx::query_scalar(
        "SELECT props FROM ad_deliveries WHERE event_name = 'view_item' LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        props["path"], "/p/tee-cs",
        "the product page from the catalog"
    );
    assert_eq!(
        props["skus"].as_array().unwrap().len(),
        2,
        "the product's variant SKUs"
    );
    tx.commit().await.unwrap();

    // Purchases: all four platforms on CZK; the EUR market has no Google Ads and no Sklik.
    purchase(&runtime, &s, s.cz, "CZK", &yes).await;
    purchase(&runtime, &s, s.sk, "EUR", &yes).await;
    purchase(&runtime, &s, s.cz, "CZK", &no).await;
    let got = deliveries(&runtime, s.tenant).await;
    let purchases: Vec<&str> = got
        .iter()
        .filter(|d| d.2 == "purchase")
        .map(|d| d.1.as_str())
        .collect();
    assert_eq!(
        purchases,
        ["ga4", "ga4", "google_ads", "meta", "meta", "sklik"]
    );
    // Captured rows hold no email, phone or IP.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let dump: String =
        sqlx::query_scalar("SELECT string_agg(row_to_json(d)::text, ' ') FROM ad_deliveries d")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(!dump.contains("buyer@example.com") && !dump.contains("606 666"));
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn withdrawal_cancels_and_send_time_consent_is_rechecked(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let s = shop(&runtime, "adwd").await;
    let (base, v) = vendor().await;
    let a = ads(&base);
    configure(&runtime, &a, &s).await;
    let anon = new_anon_id();
    consent_for(&runtime, s.tenant, &anon, true).await;
    purchase(&runtime, &s, s.cz, "CZK", &anon).await;
    let open = deliveries(&runtime, s.tenant).await;
    assert_eq!(open.len(), 4);
    assert!(open.iter().all(|d| d.3 == "pending"));

    consent_for(&runtime, s.tenant, &anon, false).await;
    let after = deliveries(&runtime, s.tenant).await;
    assert!(after.iter().all(|d| d.3 == "cancelled"), "{after:?}");
    for d in &after {
        assert_eq!(
            adtracking::deliver(&runtime, &a, s.tenant, d.0, 1, false)
                .await
                .unwrap(),
            Outcome::Stale
        );
    }
    assert!(v.got.lock().unwrap().is_empty(), "nothing was sent");

    // A refusal on the customer account (another device) also stops a purchase at send time.
    let anon2 = new_anon_id();
    consent_for(&runtime, s.tenant, &anon2, true).await;
    let order = purchase(&runtime, &s, s.cz, "CZK", &anon2).await;
    let customer = Uuid::now_v7();
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE ad_deliveries SET customer_id = $2 WHERE order_id = $1")
        .bind(order)
        .bind(customer)
        .execute(&mut *tx)
        .await
        .unwrap();
    // Recorded directly, as another device would (`insert` would cancel right away).
    sqlx::query(
        "INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose, granted,
                                      text_version, source)
         VALUES ($1, 'customer', $2, 'ads', false, 'v1', 'preferences')",
    )
    .bind(s.tenant)
    .bind(customer.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM ad_deliveries WHERE order_id = $1 AND platform = 'meta'",
    )
    .bind(order)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, id, 1, false)
            .await
            .unwrap(),
        Outcome::Cancelled
    );
    assert!(v.got.lock().unwrap().is_empty());
    // And recording that refusal through `consent::record` cancels the rest.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    consent::record(&mut tx, &Subject::Customer(customer), &grant(false), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let rest = deliveries(&runtime, s.tenant).await;
    assert!(rest.iter().all(|d| d.3 == "cancelled"), "{rest:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn vendors_get_hashed_payloads_with_retries_and_dead(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let s = shop(&runtime, "adsend").await;
    let (base, v) = vendor().await;
    let a = ads(&base);
    configure(&runtime, &a, &s).await;
    let anon = new_anon_id();
    consent_for(&runtime, s.tenant, &anon, true).await;
    purchase(&runtime, &s, s.cz, "CZK", &anon).await;
    let open = deliveries(&runtime, s.tenant).await;
    for d in &open {
        assert_eq!(
            adtracking::deliver(&runtime, &a, s.tenant, d.0, 1, false)
                .await
                .unwrap(),
            Outcome::Succeeded,
            "{d:?}"
        );
    }
    let got = v.got.lock().unwrap().clone();
    let by = |p: &str| {
        got.iter()
            .find(|(path, _)| path.contains(p))
            .unwrap()
            .1
            .clone()
    };
    let meta = by("/meta/v26.0/1234567890/events");
    let em = |s: &str| commerce::adtracking::normalize::sha256_hex(s);
    assert_eq!(
        meta["data"][0]["user_data"]["em"][0],
        em("buyer@example.com")
    );
    assert_eq!(meta["data"][0]["user_data"]["ph"][0], em("420606666666"));
    assert_eq!(
        meta["data"][0]["user_data"]["client_user_agent"],
        "Mozilla/5.0 test"
    );
    assert_eq!(
        meta["data"][0]["event_source_url"],
        "https://adsend.localhost/"
    );
    let ga4 = got
        .iter()
        .find(|(p, _)| p.contains("/ga4/mp/collect"))
        .unwrap();
    assert!(
        ga4.0
            .contains("measurement_id=G-TEST123&api_secret=ga4-secret")
    );
    let google = by("/google/v1/events:ingest");
    assert_eq!(
        google["events"][0]["userData"]["userIdentifiers"][1]["phoneNumber"],
        em("+420606666666")
    );
    let sklik = by("/sklik/rtgconv");
    assert_eq!(sklik["event_data"]["sem_id"], "sem-s2s");
    // The same event id goes to every platform (vendor-side dedupe).
    assert_eq!(meta["data"][0]["event_id"], sklik["event_id"]);
    // Finished: the user agent is dropped.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let ua: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ad_deliveries WHERE user_agent IS NOT NULL")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(ua, 0);
    tx.commit().await.unwrap();

    // Failure → retry → dead (on the last attempt); a 400 is dead at once.
    let anon2 = new_anon_id();
    consent_for(&runtime, s.tenant, &anon2, true).await;
    let order = purchase(&runtime, &s, s.cz, "CZK", &anon2).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let ids: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, platform FROM ad_deliveries WHERE order_id = $1 ORDER BY platform",
    )
    .bind(order)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let meta_id = ids.iter().find(|d| d.1 == "meta").unwrap().0;
    let ga4_id = ids.iter().find(|d| d.1 == "ga4").unwrap().0;
    *v.status.lock().unwrap() = 503;
    assert!(matches!(
        adtracking::deliver(&runtime, &a, s.tenant, meta_id, 1, false)
            .await
            .unwrap(),
        Outcome::Retry(_)
    ));
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, meta_id, 2, true)
            .await
            .unwrap(),
        Outcome::Dead
    );
    *v.status.lock().unwrap() = 400;
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, ga4_id, 1, false)
            .await
            .unwrap(),
        Outcome::Dead
    );
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let log = adtracking::deliveries(
        &mut tx,
        &DeliveryQuery {
            platform: None,
            status: Some("dead".into()),
            cursor: None,
            limit: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(log.items.len(), 2);
    let meta_log = log
        .items
        .iter()
        .find(|d| d.platform == Platform::Meta)
        .unwrap();
    assert_eq!((meta_log.attempts, meta_log.response_code), (2, Some(503)));
    let text = serde_json::to_string(&log).unwrap();
    assert!(
        !text.contains("buyer@") && !text.contains("secret"),
        "{text}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn pause_holds_and_resume_requeues_and_refunds_follow_purchases(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let s = shop(&runtime, "adpause").await;
    let (base, v) = vendor().await;
    let a = ads(&base);
    configure(&runtime, &a, &s).await;
    let anon = new_anon_id();
    consent_for(&runtime, s.tenant, &anon, true).await;
    let order = purchase(&runtime, &s, s.cz, "CZK", &anon).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Meta,
        &PlatformUpdate {
            paused: Some(true),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap();
    let meta: Uuid = sqlx::query_scalar("SELECT id FROM ad_deliveries WHERE platform = 'meta'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, meta, 1, false)
            .await
            .unwrap(),
        Outcome::Paused
    );
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Meta,
        &PlatformUpdate {
            paused: Some(false),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.jobs WHERE kind = 'adtracking.deliver' AND payload->>'delivery_id' = $1",
    )
    .bind(meta.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(jobs, 2, "the first job and the one queued on resume");
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, meta, 1, false)
            .await
            .unwrap(),
        Outcome::Succeeded
    );

    // A refund goes to GA4 (the only platform that takes refunds), once per outbox event.
    for _ in 0..2 {
        let n = adtracking::capture_refund(
            &runtime,
            s.tenant,
            42,
            &json!({ "order_id": order, "amount_minor": 12_900 }),
        )
        .await
        .unwrap();
        assert!(n <= 1);
    }
    let refunds: Vec<_> = deliveries(&runtime, s.tenant)
        .await
        .into_iter()
        .filter(|d| d.2 == "refund")
        .collect();
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].1, "ga4");
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, refunds[0].0, 1, false)
            .await
            .unwrap(),
        Outcome::Succeeded
    );
    let got = v.got.lock().unwrap().clone();
    let refund = got.iter().rev().find(|(p, _)| p.contains("/ga4/")).unwrap();
    assert_eq!(refund.1["events"][0]["name"], "refund");
    assert_eq!(refund.1["events"][0]["params"]["value"], 129.0);

    // Test mode: Seznam has no test channel, nothing is sent.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Sklik,
        &PlatformUpdate {
            test_mode: Some(true),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap();
    let sklik: Uuid = sqlx::query_scalar("SELECT id FROM ad_deliveries WHERE platform = 'sklik'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = v.got.lock().unwrap().len();
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, sklik, 1, false)
            .await
            .unwrap(),
        Outcome::Skipped
    );
    assert_eq!(v.got.lock().unwrap().len(), before);

    // Retention purge runs as the platform function (finished rows only).
    let purged: i64 = sqlx::query_scalar("SELECT platform.purge_ad_deliveries()")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(purged, 0, "nothing is 90 days old");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_withdrawal_during_preparation_or_a_removed_market_stops_the_send(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = shop(&runtime, "adrace").await;
    let (base, v) = vendor().await;
    let a = ads(&base);
    configure(&runtime, &a, &s).await;
    // Other tenants' deliveries are invisible.
    let other = shop(&runtime, "adrace2").await;
    let anon = new_anon_id();
    consent_for(&runtime, s.tenant, &anon, true).await;
    let order = purchase(&runtime, &s, s.cz, "CZK", &anon).await;
    let id_of = |platform: &'static str| {
        let runtime = runtime.clone();
        let tenant = s.tenant;
        async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let id: Uuid = sqlx::query_scalar(
                "SELECT id FROM ad_deliveries WHERE order_id = $1 AND platform = $2",
            )
            .bind(order)
            .bind(platform)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            id
        }
    };
    let google = id_of("google_ads").await;
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM ad_deliveries")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(seen, 0);

    // The OAuth exchange is slow; consent is withdrawn meanwhile: nothing is sent.
    *v.token_delay_ms.lock().unwrap() = 700;
    let (r, a2, t) = (runtime.clone(), a.clone(), s.tenant);
    let sending =
        tokio::spawn(async move { adtracking::deliver(&r, &a2, t, google, 1, false).await });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    consent_for(&runtime, s.tenant, &anon, false).await;
    // Cancelled by the withdrawal itself (the job then finds it finished) or at the claim.
    let outcome = sending.await.unwrap().unwrap();
    assert!(
        matches!(outcome, Outcome::Stale | Outcome::Cancelled),
        "{outcome:?}"
    );
    assert!(
        v.got.lock().unwrap().is_empty(),
        "nothing reached the vendor"
    );

    // A market removed from a platform stops its queued deliveries.
    let anon2 = new_anon_id();
    consent_for(&runtime, s.tenant, &anon2, true).await;
    let order2 = purchase(&runtime, &s, s.cz, "CZK", &anon2).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    adtracking::update(
        &mut tx,
        &a,
        "staff",
        Platform::Meta,
        &PlatformUpdate {
            market_ids: Some(vec![s.sk]),
            ..PlatformUpdate::default()
        },
    )
    .await
    .unwrap();
    let meta: Uuid = sqlx::query_scalar(
        "SELECT id FROM ad_deliveries WHERE order_id = $1 AND platform = 'meta'",
    )
    .bind(order2)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        adtracking::deliver(&runtime, &a, s.tenant, meta, 1, false)
            .await
            .unwrap(),
        Outcome::Skipped
    );
    assert!(v.got.lock().unwrap().is_empty());

    // A delivery whose job gave up is closed, not left open.
    let ga4 = {
        let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
        let id: Uuid = sqlx::query_scalar(
            "SELECT id FROM ad_deliveries WHERE order_id = $1 AND platform = 'ga4'",
        )
        .bind(order2)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    };
    assert!(
        adtracking::give_up(&runtime, s.tenant, ga4, "gave up")
            .await
            .unwrap()
    );
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let (status, ua): (String, Option<String>) =
        sqlx::query_as("SELECT status, user_agent FROM ad_deliveries WHERE id = $1")
            .bind(ga4)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((status.as_str(), ua), ("dead", None));
}
