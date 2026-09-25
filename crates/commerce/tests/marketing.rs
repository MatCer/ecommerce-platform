//! Email marketing against Postgres (runtime role): double opt-in and consent evidence (A20),
//! segments compiled to parameterized SQL (incl. injection attempts), idempotent throttled
//! campaign batches with send-time re-checks (A12, A14, A20), personalized product blocks,
//! signed click links, one-click unsubscribe, bounce ingestion and tenant isolation.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use commerce::consent::{self, ConsentChoice, ConsentPurpose, Purposes, Source, Subject};
use commerce::inventory::{self, Adjustment};
use commerce::marketing::campaigns::{self, CampaignInput, EmailBlock, LocaleContent};
use commerce::marketing::deliverability::{self, Applied};
use commerce::marketing::segments::{self, Condition, Match, Rules, SegmentInput};
use commerce::marketing::subscribers::{self, Status};
use commerce::notifications::{self, Step};
use commerce::storefront::{self, Context, PublicUrls};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::catalog::{self, ACTOR};
use testkit::smtp::{FakeSmtp, Mode};
use testkit::storefront::{Shop, shop};
use uuid::Uuid;

fn urls() -> PublicUrls {
    PublicUrls {
        scheme: "http".into(),
        port: None,
    }
}

async fn ctx(runtime: &PgPool, shop: &Shop, locale: &str) -> Context {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let c = storefront::context(&mut tx, &urls(), shop.cz, Some(locale), Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    c
}

/// The token in the newest confirmation mail to `email`.
async fn confirm_token(runtime: &PgPool, tenant: Uuid, email: &str) -> String {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let body: String = sqlx::query_scalar(
        "SELECT body_text FROM email_messages WHERE to_email = $1 AND template = 'newsletter_confirm'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(email)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let at = body.find("token=").unwrap() + 6;
    body[at..at + 64].to_owned()
}

/// Subscribes and confirms `email` on the CZ market.
async fn subscribed(runtime: &PgPool, shop: &Shop, email: &str) -> Uuid {
    let c = ctx(runtime, shop, "cs").await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    subscribers::subscribe(&mut tx, &c, email, None, "form")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let token = confirm_token(runtime, shop.tenant, email).await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let s = subscribers::confirm(&mut tx, &token, Some(&[1; 32]))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    s.id
}

async fn count(runtime: &PgPool, tenant: Uuid, sql: &'static str) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let n: i64 = sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    n
}

async fn setup(db: &PgPool, slug: &str) -> (PgPool, Shop) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let s = shop(&runtime, slug).await;
    (runtime, s)
}

#[sqlx::test(migrations = "../../migrations")]
async fn double_opt_in_records_evidence_and_never_enumerates(db: PgPool) {
    let (runtime, s) = setup(&db, "nl").await;
    let c = ctx(&runtime, &s, "cs").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    subscribers::subscribe(&mut tx, &c, " Jana@Example.com ", Some(&[9; 32]), "form")
        .await
        .unwrap();
    // A repeat within the cooldown is accepted but sends nothing new.
    subscribers::subscribe(&mut tx, &c, "jana@example.com", Some(&[9; 32]), "form")
        .await
        .unwrap();
    assert!(
        subscribers::subscribe(&mut tx, &c, "not-an-email", None, "form")
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &runtime,
            s.tenant,
            "SELECT count(*) FROM email_messages WHERE template = 'newsletter_confirm'"
        )
        .await,
        1
    );
    let token = confirm_token(&runtime, s.tenant, "jana@example.com").await;

    // GET view does not change anything; confirming does, once.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let view = subscribers::confirmation(&mut tx, &token).await.unwrap();
    assert_eq!(view.email, "j***@example.com");
    let sub = subscribers::confirm(&mut tx, &token, Some(&[2; 32]))
        .await
        .unwrap();
    assert_eq!(sub.status, Status::Subscribed);
    assert!(sub.confirmed_at.is_some());
    assert!(matches!(
        subscribers::confirm(&mut tx, &token, None).await,
        Err(platform::Error::NotFound)
    ));
    assert!(matches!(
        subscribers::confirmation(&mut tx, &"0".repeat(64)).await,
        Err(platform::Error::NotFound)
    ));
    let granted = consent::current(
        &mut tx,
        &Subject::Email("jana@example.com".into()),
        ConsentPurpose::EmailMarketing,
    )
    .await
    .unwrap();
    assert!(granted, "the confirmation is recorded as consent evidence");
    let (source, ip): (String, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT source, ip_hash FROM consent_records WHERE subject_id = 'jana@example.com'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!((source.as_str(), ip), ("double_opt_in", Some(vec![2; 32])));
    // Subscribed: a new sign-up is accepted silently, no mail.
    subscribers::subscribe(&mut tx, &c, "jana@example.com", None, "form")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &runtime,
            s.tenant,
            "SELECT count(*) FROM email_messages WHERE template = 'newsletter_confirm'"
        )
        .await,
        1
    );

    // Expired tokens do not confirm.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    subscribers::subscribe(&mut tx, &c, "late@example.com", None, "form")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let late = confirm_token(&runtime, s.tenant, "late@example.com").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE subscribers SET confirm_expires_at = now() - interval '1 minute' WHERE email = 'late@example.com'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(subscribers::confirm(&mut tx, &late, None).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn signups_per_ip_are_capped(db: PgPool) {
    let (runtime, s) = setup(&db, "cap").await;
    let c = ctx(&runtime, &s, "cs").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    // Every request counts, also repeats of one address.
    for i in 0..subscribers::MAX_REQUESTS_PER_IP_HOUR {
        let email = if i % 2 == 0 {
            format!("u{i}@example.com")
        } else {
            "same@example.com".to_owned()
        };
        subscribers::subscribe(&mut tx, &c, &email, Some(&[5; 32]), "form")
            .await
            .unwrap();
    }
    let r =
        subscribers::subscribe(&mut tx, &c, "one-more@example.com", Some(&[5; 32]), "form").await;
    assert!(matches!(r, Err(platform::Error::TooManyRequests { .. })));
    // Another IP is unaffected.
    subscribers::subscribe(&mut tx, &c, "other@example.com", Some(&[6; 32]), "form")
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn withdrawing_consent_anywhere_unsubscribes(db: PgPool) {
    let (runtime, s) = setup(&db, "wd").await;
    let a = subscribed(&runtime, &s, "anna@example.com").await;
    let b = subscribed(&runtime, &s, "bert@example.com").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    // Bert has a verified customer account: the link is made on confirmation / verification.
    let customer: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale, email_verified_at)
         VALUES ($1, 'bert@example.com', 'cs', now()) RETURNING id",
    )
    .bind(s.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        subscribers::link_customer(&mut tx, customer).await.unwrap(),
        1
    );
    assert_eq!(subscribers::may_receive(&mut tx, b).await.unwrap(), None);
    // The customer turns email marketing off on the preferences page (customer subject).
    consent::record(
        &mut tx,
        &Subject::Customer(customer),
        &ConsentChoice {
            purposes: Purposes {
                email_marketing: Some(false),
                ..Purposes::default()
            },
            text_version: "2026-09-25".into(),
            source: Source::Preferences,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        subscribers::get(&mut tx, b).await.unwrap().status,
        Status::Unsubscribed
    );
    assert_eq!(
        subscribers::may_receive(&mut tx, b).await.unwrap(),
        Some("not_subscribed")
    );
    // Staff unsubscribe Anna on request (audited).
    let anna = subscribers::admin_unsubscribe(&mut tx, "staff-1", a)
        .await
        .unwrap();
    assert_eq!(anna.status, Status::Unsubscribed);
    assert!(
        !consent::current(
            &mut tx,
            &Subject::Email("anna@example.com".into()),
            ConsentPurpose::EmailMarketing
        )
        .await
        .unwrap()
    );
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'subscriber.unsubscribe'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(audit, 1);
}

/// A sellable product of `brand` in the shop's category.
async fn product(runtime: &PgPool, s: &Shop, sku: &str, brand: &str) -> (Uuid, Uuid) {
    let mut input = catalog::product_input(sku, 1);
    input.category_ids = vec![s.category];
    input.brand = Some(brand.into());
    let p = catalog::create(runtime, s.tenant, &input).await;
    let v = p.variants[0].id;
    testkit::pricing::set_prices(runtime, s.tenant, s.czk, &[(v, 49_900)]).await;
    let mut tx = tenant_tx(runtime, s.tenant).await.unwrap();
    inventory::adjust(
        &mut tx,
        ACTOR,
        v,
        &format!("init-{v}"),
        &Adjustment {
            delta: None,
            on_hand: Some(20),
            note: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (p.id, v)
}

#[sqlx::test(migrations = "../../migrations")]
async fn segments_filter_on_purchases_engagement_and_consented_affinity(db: PgPool) {
    let (runtime, s) = setup(&db, "seg").await;
    let buyer = subscribed(&runtime, &s, "buyer@example.com").await;
    let _other = subscribed(&runtime, &s, "other@example.com").await;
    let (p, v) = product(&runtime, &s, "SEG", "Evil' OR '1'='1").await;
    // One placed order of buyer@ (testkit orders are placed by buyer@example.com).
    testkit::storefront::raw_order(&runtime, &s, s.cz, "CZK", 49_900, 1, "confirmed").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE order_lines SET product_id = $1, variant_id = $2")
        .bind(p)
        .bind(v)
        .execute(&mut *tx)
        .await
        .unwrap();
    let now = Utc::now();
    let run = async |tx: &mut platform::db::TenantTx, rules: Rules| {
        segments::preview(tx, &rules, now).await.unwrap()
    };
    let all = run(&mut tx, Rules::default()).await;
    assert_eq!(all.count, 2);
    let brand = run(
        &mut tx,
        Rules {
            match_: Match::All,
            conditions: vec![Condition::PurchasedBrand {
                brands: vec!["Evil' OR '1'='1".into()],
            }],
        },
    )
    .await;
    assert_eq!(brand.count, 1, "the value is compared, never executed");
    assert_eq!(brand.sample[0].id, buyer);
    let injected = run(
        &mut tx,
        Rules {
            match_: Match::All,
            conditions: vec![Condition::PurchasedBrand {
                brands: vec!["x'); DELETE FROM subscribers; --".into()],
            }],
        },
    )
    .await;
    assert_eq!(injected.count, 0);
    let spent = run(
        &mut tx,
        Rules {
            match_: Match::All,
            conditions: vec![
                Condition::TotalSpent {
                    currency: "czk".into(),
                    min_minor: Some(40_000),
                    max_minor: None,
                },
                Condition::OrderCount {
                    min: Some(1),
                    max: Some(1),
                },
                Condition::PurchasedCategory {
                    category_ids: vec![s.category],
                },
                Condition::LastOrder {
                    after: Some(now - Duration::days(1)),
                    before: None,
                },
            ],
        },
    )
    .await;
    assert_eq!(spent.count, 1);
    let any = run(
        &mut tx,
        Rules {
            match_: Match::Any,
            conditions: vec![
                Condition::OrderCount {
                    min: Some(5),
                    max: None,
                },
                Condition::Locale {
                    locales: vec!["cs".into()],
                },
            ],
        },
    )
    .await;
    assert_eq!(any.count, 2);
    let engaged = run(
        &mut tx,
        Rules {
            match_: Match::All,
            conditions: vec![Condition::Engaged { days: 30 }],
        },
    )
    .await;
    assert_eq!(engaged.count, 0);

    // Affinity only with a current `personalization` consent of the linked customer.
    let customer: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale, email_verified_at)
         VALUES ($1, 'buyer@example.com', 'cs', now()) RETURNING id",
    )
    .bind(s.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    subscribers::link_customer(&mut tx, customer).await.unwrap();
    sqlx::query(
        "INSERT INTO customer_affinity (tenant_id, customer_id, dim, key, score)
         VALUES ($1, $2, 'brand', 'Merino', 3.0)",
    )
    .bind(s.tenant)
    .bind(customer)
    .execute(&mut *tx)
    .await
    .unwrap();
    let affinity = Rules {
        match_: Match::All,
        conditions: vec![Condition::Affinity {
            dim: segments::AffinityDim::Brand,
            keys: vec!["Merino".into()],
        }],
    };
    assert_eq!(
        run(&mut tx, affinity.clone()).await.count,
        0,
        "no consent, no affinity"
    );
    let choose = |granted: bool| ConsentChoice {
        purposes: Purposes {
            personalization: Some(granted),
            ..Purposes::default()
        },
        text_version: "2026-09-25".into(),
        source: Source::Preferences,
    };
    consent::record(&mut tx, &Subject::Customer(customer), &choose(true), None)
        .await
        .unwrap();
    assert_eq!(run(&mut tx, affinity.clone()).await.count, 1);
    consent::record(&mut tx, &Subject::Customer(customer), &choose(false), None)
        .await
        .unwrap();
    assert_eq!(
        run(&mut tx, affinity).await.count,
        0,
        "withdrawn: gone at once"
    );

    // CRUD: names are unique, rules are validated before storing.
    let seg = segments::create(
        &mut tx,
        "staff",
        &SegmentInput {
            name: "Kupující".into(),
            rules: Rules::default(),
        },
    )
    .await
    .unwrap();
    assert!(
        segments::create(
            &mut tx,
            "staff",
            &SegmentInput {
                name: "Kupující".into(),
                rules: Rules::default()
            }
        )
        .await
        .is_err()
    );
    let bad: Rules =
        serde_json::from_value(json!({"conditions": [{"field": "locale", "locales": ["c'"]}]}))
            .unwrap();
    assert!(
        segments::update(
            &mut tx,
            "staff",
            seg.id,
            &SegmentInput {
                name: "x".into(),
                rules: bad
            }
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();

    // Another tenant sees none of these subscribers.
    let other = shop(&runtime, "seg2").await;
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    assert_eq!(
        segments::preview(&mut tx, &Rules::default(), now)
            .await
            .unwrap()
            .count,
        0
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM subscribers")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let hit = sqlx::query("UPDATE subscribers SET status = 'unsubscribed' WHERE id = $1")
        .bind(buyer)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(hit.rows_affected(), 0, "cross-tenant writes do nothing");
    let insert =
        sqlx::query("INSERT INTO segments (tenant_id, name, rules) VALUES ($1, 'x', '{}')")
            .bind(s.tenant)
            .execute(&mut *tx)
            .await;
    assert!(
        insert.is_err(),
        "RLS WITH CHECK refuses another tenant's rows"
    );
}

fn content(blocks: Vec<EmailBlock>) -> BTreeMap<String, LocaleContent> {
    let mut c = BTreeMap::new();
    c.insert(
        "cs".to_owned(),
        LocaleContent {
            subject: "Podzimní novinky".into(),
            preheader: "To nejlepší".into(),
            blocks,
        },
    );
    c
}

#[sqlx::test(migrations = "../../migrations")]
async fn campaigns_send_once_per_subscriber_with_rechecks_and_tracking(db: PgPool) {
    let (runtime, s) = setup(&db, "cmp").await;
    let (p1, _) = product(&runtime, &s, "MER", "Merino").await;
    let _ = product(&runtime, &s, "LIN", "Len").await;
    let a = subscribed(&runtime, &s, "anna@example.com").await;
    let b = subscribed(&runtime, &s, "bert@example.com").await;
    {
        let c = ctx(&runtime, &s, "cs").await;
        let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
        subscribers::subscribe(&mut tx, &c, "pending@example.com", None, "form")
            .await
            .unwrap();
        tx.commit().await.unwrap();
    };
    // Bert withdraws on the address subject after subscribing (e.g. another device).
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    // Anna is a customer with personalization consent and a brand affinity.
    let customer: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale, email_verified_at)
         VALUES ($1, 'anna@example.com', 'cs', now()) RETURNING id",
    )
    .bind(s.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    subscribers::link_customer(&mut tx, customer).await.unwrap();
    sqlx::query(
        "INSERT INTO customer_affinity (tenant_id, customer_id, dim, key, score)
         VALUES ($1, $2, 'brand', 'Merino', 5.0)",
    )
    .bind(s.tenant)
    .bind(customer)
    .execute(&mut *tx)
    .await
    .unwrap();
    consent::record(
        &mut tx,
        &Subject::Customer(customer),
        &ConsentChoice {
            purposes: Purposes {
                personalization: Some(true),
                ..Purposes::default()
            },
            text_version: "2026-09-25".into(),
            source: Source::Preferences,
        },
        None,
    )
    .await
    .unwrap();
    let campaign = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: "Podzim".into(),
            segment_id: None,
            content: content(vec![
                EmailBlock::Heading {
                    text: "Novinky".into(),
                },
                EmailBlock::Text {
                    html: "<p>Mrkněte na <a href=\"/c/tricka\">trička</a></p>".into(),
                },
                EmailBlock::Button {
                    label: "Do obchodu".into(),
                    href: "/".into(),
                },
                EmailBlock::PersonalizedProducts {
                    title: "Pro vás".into(),
                    limit: 2,
                },
            ]),
        },
    )
    .await
    .unwrap();
    // Preview for Anna uses her affinity (Merino first); a test send goes to staff only.
    let preview = campaigns::preview(&mut tx, &urls(), campaign.id, Some(a), None, Utc::now())
        .await
        .unwrap();
    assert!(preview.html.contains("Pro vás"));
    let pos = |html: &str, name: &str| html.find(name).unwrap();
    assert!(
        pos(&preview.html, "Product MER") < pos(&preview.html, "Product LIN"),
        "Anna's affinity ranks Merino first"
    );
    // Without personal signals: the fallback (no sales yet: newest first).
    let public = campaigns::preview(&mut tx, &urls(), campaign.id, None, Some("cs"), Utc::now())
        .await
        .unwrap();
    assert!(pos(&public.html, "Product LIN") < pos(&public.html, "Product MER"));
    tx.commit().await.unwrap();
    testkit::staff(&runtime, s.tenant, "boss", "owner").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    assert_eq!(
        campaigns::test_send(
            &mut tx,
            &urls(),
            "staff",
            campaign.id,
            &["Boss@Example.test".into()],
            None,
            Utc::now()
        )
        .await
        .unwrap(),
        1
    );
    // Anyone else (a customer, an unsubscribed address) is refused: no way around consent.
    let refused = campaigns::test_send(
        &mut tx,
        &urls(),
        "staff",
        campaign.id,
        &["boss@example.test".into(), "bert@example.com".into()],
        None,
        Utc::now(),
    )
    .await;
    assert!(matches!(
        refused,
        Err(platform::Error::Validation {
            code: "not_staff",
            ..
        })
    ));
    let scheduled = campaigns::schedule(&mut tx, "staff", campaign.id, None, Utc::now())
        .await
        .unwrap();
    assert_eq!(scheduled.status, campaigns::CampaignStatus::Scheduled);
    assert!(
        campaigns::update(
            &mut tx,
            "staff",
            campaign.id,
            &CampaignInput {
                name: "x".into(),
                segment_id: None,
                content: content(vec![EmailBlock::Heading { text: "x".into() }]),
            }
        )
        .await
        .is_err(),
        "scheduled campaigns are frozen"
    );
    // Bert withdraws before the batch runs.
    consent::record_server(
        &mut tx,
        &Subject::Email("bert@example.com".into()),
        ConsentPurpose::EmailMarketing,
        false,
        "2026-09-25",
        "unsubscribe",
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let batch = campaigns::run_batch(&runtime, &urls(), s.tenant, campaign.id, Utc::now())
        .await
        .unwrap();
    assert_eq!(
        (batch.sent, batch.skipped, batch.done),
        (1, 0, true),
        "Bert is unsubscribed"
    );
    // Repeating the job changes nothing (A12).
    let again = campaigns::run_batch(&runtime, &urls(), s.tenant, campaign.id, Utc::now())
        .await
        .unwrap();
    assert_eq!((again.sent, again.skipped), (0, 0));

    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let got = campaigns::get(&mut tx, campaign.id).await.unwrap();
    assert_eq!(got.status, campaigns::CampaignStatus::Sent);
    assert_eq!((got.stats.recipients, got.stats.sent), (1, 1));
    let (html, list_unsub, sub_id): (String, String, Uuid) = sqlx::query_as(
        "SELECT html, list_unsubscribe, subscriber_id FROM email_messages WHERE template = 'campaign'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(sub_id, a);
    assert!(list_unsub.starts_with("http://checkout.cmp.localhost/_p/newsletter/unsubscribe?t="));
    assert!(
        html.contains("http://checkout.cmp.localhost/_p/newsletter/click?t="),
        "tracked links"
    );
    assert!(
        html.contains("u=http%3A%2F%2Fcmp.localhost%2Fc%2Ftricka"),
        "rich text links are absolute and tracked"
    );
    let token = &list_unsub[list_unsub.find("t=").unwrap() + 2..];
    // Clicks: the signed target redirects and is counted; a tampered one is refused.
    let start = html.find("/_p/newsletter/click?").unwrap();
    let end = start + html[start..].find('"').unwrap();
    let link = html[start..end].replace("&amp;", "&");
    let q: BTreeMap<String, String> = reqwest::Url::parse(&format!("http://x{link}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let target = campaigns::click(&mut tx, &q["t"], &q["u"], &q["s"])
        .await
        .unwrap();
    assert_eq!(target, q["u"]);
    assert!(
        campaigns::click(&mut tx, &q["t"], "https://evil.example/", &q["s"])
            .await
            .is_err()
    );
    assert!(
        campaigns::click(&mut tx, &q["t"], &q["u"], "00")
            .await
            .is_err()
    );
    assert_eq!(
        campaigns::get(&mut tx, campaign.id)
            .await
            .unwrap()
            .stats
            .clicked,
        1
    );
    // Personalized products link to Anna's affinity (Merino) product first.
    let _ = p1;
    assert!(
        html.find("%2Fp%2Fmer-cs").unwrap() < html.find("%2Fp%2Flin-cs").unwrap(),
        "personalized block"
    );
    tx.commit().await.unwrap();

    // A20 at send time: Anna withdraws after the message was queued: SMTP is never reached.
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let message: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template = 'campaign'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    // One-click unsubscribe with the recipient's token.
    let prefs = campaigns::unsubscribe(&mut tx, token, None).await.unwrap();
    assert_eq!(prefs.status, Status::Unsubscribed);
    tx.commit().await.unwrap();
    let step = notifications::deliver(&runtime, &smtp.mailer(), s.tenant, message)
        .await
        .unwrap();
    assert_eq!(step, Step::Done);
    assert!(smtp.received().is_empty());
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT status, last_error FROM email_messages WHERE id = $1")
            .bind(message)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), error.as_deref()),
        ("failed", Some("not_subscribed"))
    );
    let stats = campaigns::get(&mut tx, campaign.id).await.unwrap().stats;
    assert_eq!(stats.unsubscribed, 1);
    // A second campaign skips Anna and Bert: nobody is subscribed any more.
    let second = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: "Zima".into(),
            segment_id: None,
            content: content(vec![EmailBlock::Heading {
                text: "Zima".into(),
            }]),
        },
    )
    .await
    .unwrap();
    campaigns::schedule(&mut tx, "staff", second.id, None, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let batch = campaigns::run_batch(&runtime, &urls(), s.tenant, second.id, Utc::now())
        .await
        .unwrap();
    assert_eq!((batch.sent, batch.done), (0, true));
    let _ = b;
}

#[sqlx::test(migrations = "../../migrations")]
async fn batches_respect_the_tenant_rate_window(db: PgPool) {
    let (runtime, s) = setup(&db, "thr").await;
    subscribed(&runtime, &s, "a@example.com").await;
    let now = Utc::now();
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO email_settings (tenant_id, throttle_window_start, throttle_window_count)
         VALUES ($1, $2, $3)",
    )
    .bind(s.tenant)
    .bind(now - Duration::seconds(10))
    .bind(campaigns::RATE_PER_MINUTE)
    .execute(&mut *tx)
    .await
    .unwrap();
    let c = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: "T".into(),
            segment_id: None,
            content: content(vec![EmailBlock::Heading { text: "T".into() }]),
        },
    )
    .await
    .unwrap();
    campaigns::schedule(&mut tx, "staff", c.id, None, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let b = campaigns::run_batch(&runtime, &urls(), s.tenant, c.id, now)
        .await
        .unwrap();
    assert_eq!((b.sent, b.done), (0, false), "window full: deferred");
    let wait: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.jobs WHERE kind = 'marketing.campaign_batch' AND run_at >= $1",
    )
    .bind(now + Duration::seconds(49))
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(wait, 1);
    // After the window the batch goes out.
    let later = now + Duration::seconds(61);
    let b = campaigns::run_batch(&runtime, &urls(), s.tenant, c.id, later)
        .await
        .unwrap();
    assert_eq!((b.sent, b.done), (1, true));
}

#[sqlx::test(migrations = "../../migrations")]
async fn bounces_and_complaints_suppress_only_the_messages_recipient(db: PgPool) {
    let (runtime, s) = setup(&db, "bnc").await;
    let a = subscribed(&runtime, &s, "anna@example.com").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let c = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: "B".into(),
            segment_id: None,
            content: content(vec![EmailBlock::Heading { text: "B".into() }]),
        },
    )
    .await
    .unwrap();
    campaigns::schedule(&mut tx, "staff", c.id, None, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    campaigns::run_batch(&runtime, &urls(), s.tenant, c.id, Utc::now())
        .await
        .unwrap();
    let message: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template = 'campaign'")
            .fetch_one(&db)
            .await
            .unwrap();
    let event = |kind: &str, to: &str| {
        let body = if kind == "Bounce" {
            json!({"notificationType": "Bounce", "bounce": {"bounceType": "Permanent",
                   "bouncedRecipients": [{"emailAddress": to}]},
                   "mail": {"commonHeaders": {"messageId": format!("<{message}@mail.test>")}}})
        } else {
            json!({"notificationType": "Complaint", "complaint": {
                   "complainedRecipients": [{"emailAddress": to}]},
                   "mail": {"commonHeaders": {"messageId": format!("<{message}@mail.test>")}}})
        };
        deliverability::parse_ses(&body.to_string()).unwrap()
    };
    // A notification naming another address changes nothing.
    assert_eq!(
        deliverability::apply(&runtime, &event("Bounce", "victim@example.com"))
            .await
            .unwrap(),
        Applied::Ignored("recipient does not match the message")
    );
    assert_eq!(
        deliverability::apply(&runtime, &event("Bounce", "Anna@Example.com"))
            .await
            .unwrap(),
        Applied::Suppressed(1)
    );
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    assert!(
        notifications::is_suppressed(
            &mut tx,
            "anna@example.com",
            platform::mail::Stream::Transactional
        )
        .await
        .unwrap()
    );
    assert_eq!(
        subscribers::get(&mut tx, a).await.unwrap().status,
        Status::Bounced
    );
    assert_eq!(
        campaigns::get(&mut tx, c.id).await.unwrap().stats.bounced,
        1
    );
    tx.commit().await.unwrap();
    // A complaint afterwards keeps the (harder) bounce suppression and records a withdrawal.
    deliverability::apply(&runtime, &event("Complaint", "anna@example.com"))
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let reason: String = sqlx::query_scalar("SELECT reason FROM email_suppressions")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(reason, "bounce");
    assert_eq!(
        subscribers::get(&mut tx, a).await.unwrap().status,
        Status::Complained
    );
    assert!(
        !consent::current(
            &mut tx,
            &Subject::Email("anna@example.com".into()),
            ConsentPurpose::EmailMarketing
        )
        .await
        .unwrap()
    );
}

/// Enqueues campaign messages for the given subscribed addresses and returns (campaign,
/// message ids).
async fn sent_campaign(runtime: &PgPool, s: &Shop, blocks: Vec<EmailBlock>) -> (Uuid, Vec<Uuid>) {
    let mut tx = tenant_tx(runtime, s.tenant).await.unwrap();
    let c = campaigns::create(
        &mut tx,
        "staff",
        &CampaignInput {
            name: format!("C {}", Uuid::now_v7()),
            segment_id: None,
            content: content(blocks),
        },
    )
    .await
    .unwrap();
    campaigns::schedule(&mut tx, "staff", c.id, None, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    campaigns::run_batch(runtime, &urls(), s.tenant, c.id, Utc::now())
        .await
        .unwrap();
    let mut tx = tenant_tx(runtime, s.tenant).await.unwrap();
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT message_id FROM campaign_sends WHERE campaign_id = $1 AND status = 'sent' ORDER BY id",
    )
    .bind(c.id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (c.id, ids)
}

async fn message_state(runtime: &PgPool, tenant: Uuid, id: Uuid) -> (String, Option<String>) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let r = sqlx::query_as("SELECT status, last_error FROM email_messages WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    r
}

#[sqlx::test(migrations = "../../migrations")]
async fn cancellation_and_withdrawn_personalization_stop_queued_mail(db: PgPool) {
    let (runtime, s) = setup(&db, "stop").await;
    let _ = product(&runtime, &s, "MER", "Merino").await;
    subscribed(&runtime, &s, "anna@example.com").await;
    let smtp = FakeSmtp::start(Mode::Accept).await;

    // Cancelled while sending, after a batch queued its messages: nothing reaches SMTP. (The
    // rate window has room for one message, so the campaign is still `sending`.)
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO email_settings (tenant_id, throttle_window_start, throttle_window_count)
         VALUES ($1, now(), $2)",
    )
    .bind(s.tenant)
    .bind(campaigns::RATE_PER_MINUTE - 1)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (campaign, ids) =
        sent_campaign(&runtime, &s, vec![EmailBlock::Heading { text: "A".into() }]).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    campaigns::cancel(&mut tx, "staff", campaign).await.unwrap();
    tx.commit().await.unwrap();
    notifications::deliver(&runtime, &smtp.mailer(), s.tenant, ids[0])
        .await
        .unwrap();
    assert_eq!(
        message_state(&runtime, s.tenant, ids[0]).await,
        ("failed".into(), Some("campaign_cancelled".into()))
    );
    // A fresh rate window for the rest of the test.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE email_settings SET throttle_window_count = 0")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Anna becomes a customer granting personalization, with an affinity: her message is
    // personalized; withdrawing personalization before SMTP stops it (A20).
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let customer: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale, email_verified_at)
         VALUES ($1, 'anna@example.com', 'cs', now()) RETURNING id",
    )
    .bind(s.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    subscribers::link_customer(&mut tx, customer).await.unwrap();
    sqlx::query(
        "INSERT INTO customer_affinity (tenant_id, customer_id, dim, key, score)
         VALUES ($1, $2, 'brand', 'Merino', 5.0)",
    )
    .bind(s.tenant)
    .bind(customer)
    .execute(&mut *tx)
    .await
    .unwrap();
    let choose = |granted: bool| ConsentChoice {
        purposes: Purposes {
            personalization: Some(granted),
            ..Purposes::default()
        },
        text_version: "2026-09-25".into(),
        source: Source::Preferences,
    };
    consent::record(&mut tx, &Subject::Customer(customer), &choose(true), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let personal = vec![EmailBlock::PersonalizedProducts {
        title: "Pro vás".into(),
        limit: 2,
    }];
    let (_, ids) = sent_campaign(&runtime, &s, personal.clone()).await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    consent::record(&mut tx, &Subject::Customer(customer), &choose(false), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    notifications::deliver(&runtime, &smtp.mailer(), s.tenant, ids[0])
        .await
        .unwrap();
    assert_eq!(
        message_state(&runtime, s.tenant, ids[0]).await,
        ("failed".into(), Some("personalization_withdrawn".into()))
    );
    assert!(smtp.received().is_empty());
    // Without personalization the next campaign is rendered from the fallback and goes out.
    let (_, ids) = sent_campaign(&runtime, &s, personal).await;
    notifications::deliver(&runtime, &smtp.mailer(), s.tenant, ids[0])
        .await
        .unwrap();
    assert_eq!(
        message_state(&runtime, s.tenant, ids[0]).await.0,
        "accepted"
    );
    assert_eq!(smtp.received().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_batches_send_once_and_share_the_tenant_rate(db: PgPool) {
    let (runtime, s) = setup(&db, "conc").await;
    // More subscribers than one minute's rate, subscribed with consent (bulk fixture).
    let n = campaigns::RATE_PER_MINUTE + 20;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    sqlx::query(
        "WITH s AS (
             INSERT INTO subscribers (tenant_id, email, status, locale, market_id, text_version,
                                      source, confirmed_at)
             SELECT $1, 'bulk' || g || '@example.com', 'subscribed', 'cs', $2, 'v1', 'form', now()
             FROM generate_series(1, $3) g RETURNING email)
         INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose, granted,
                                      text_version, source)
         SELECT $1, 'email', email, 'email_marketing', true, 'v1', 'double_opt_in' FROM s",
    )
    .bind(s.tenant)
    .bind(s.cz)
    .bind(n)
    .execute(&mut *tx)
    .await
    .unwrap();
    let mut ids = Vec::new();
    for name in ["One", "Two"] {
        let c = campaigns::create(
            &mut tx,
            "staff",
            &CampaignInput {
                name: name.into(),
                segment_id: None,
                content: content(vec![EmailBlock::Heading { text: name.into() }]),
            },
        )
        .await
        .unwrap();
        campaigns::schedule(&mut tx, "staff", c.id, None, Utc::now())
            .await
            .unwrap();
        ids.push(c.id);
    }
    tx.commit().await.unwrap();
    let now = Utc::now();
    // The same campaign's job twice at once plus another campaign of the tenant.
    let u = urls();
    let (a, b, c) = tokio::join!(
        campaigns::run_batch(&runtime, &u, s.tenant, ids[0], now),
        campaigns::run_batch(&runtime, &u, s.tenant, ids[0], now),
        campaigns::run_batch(&runtime, &u, s.tenant, ids[1], now),
    );
    let total = a.unwrap().sent + b.unwrap().sent + c.unwrap().sent;
    assert_eq!(
        total,
        usize::try_from(campaigns::RATE_PER_MINUTE).unwrap(),
        "one minute's rate across the tenant's campaigns"
    );
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let dupes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT campaign_id, subscriber_id FROM campaign_sends
                               GROUP BY 1, 2 HAVING count(*) > 1) d",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(dupes, 0);
    let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM email_messages")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(messages, i64::from(campaigns::RATE_PER_MINUTE));
    tx.commit().await.unwrap();
    // The next window finishes both, still once per subscriber and campaign.
    for _ in 0..3 {
        for id in &ids {
            campaigns::run_batch(
                &runtime,
                &urls(),
                s.tenant,
                *id,
                now + Duration::seconds(61),
            )
            .await
            .unwrap();
        }
    }
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    let per_campaign: Vec<i64> = sqlx::query_scalar(
        "SELECT count(*) FROM campaign_sends GROUP BY campaign_id ORDER BY campaign_id",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert!(
        per_campaign.iter().all(|c| *c <= i64::from(n)),
        "{per_campaign:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_marketing_table_is_tenant_isolated(db: PgPool) {
    let (runtime, s) = setup(&db, "iso").await;
    subscribed(&runtime, &s, "anna@example.com").await;
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    segments::create(
        &mut tx,
        "staff",
        &SegmentInput {
            name: "Vše".into(),
            rules: Rules::default(),
        },
    )
    .await
    .unwrap();
    notifications::admin::set_text(
        &mut tx,
        "staff",
        "newsletter_confirm",
        "cs",
        &serde_json::from_value(json!({"subject": "Ahoj {shop}"})).unwrap(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_, _) = sent_campaign(&runtime, &s, vec![EmailBlock::Heading { text: "X".into() }]).await;

    let other = shop(&runtime, "iso2").await;
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    for table in [
        "subscribers",
        "segments",
        "campaigns",
        "campaign_sends",
        "email_settings",
        "email_template_texts",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
        let touched = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = tenant_id"
        )))
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(touched, 0, "{table} writable across tenants");
        let deleted = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(deleted, 0, "{table} deletable across tenants");
    }
    // Inserting rows for the first tenant is refused by the policies' WITH CHECK.
    for insert in [
        "INSERT INTO email_settings (tenant_id) VALUES ($1)",
        "INSERT INTO email_template_texts (tenant_id, template, locale, subject)
         VALUES ($1, 'magic_link', 'cs', 'x')",
        "INSERT INTO segments (tenant_id, name, rules) VALUES ($1, 'x', '{}')",
    ] {
        let mut sp = tenant_tx(&runtime, other.tenant).await.unwrap();
        let r = sqlx::query(insert).bind(s.tenant).execute(&mut *sp).await;
        assert!(r.is_err(), "{insert}");
        drop(sp);
    }
    tx.commit().await.unwrap();
    // The first tenant still has all of its rows.
    let mut tx = tenant_tx(&runtime, s.tenant).await.unwrap();
    for table in [
        "subscribers",
        "segments",
        "campaigns",
        "campaign_sends",
        "email_settings",
        "email_template_texts",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(n > 0, "{table}");
    }
}
