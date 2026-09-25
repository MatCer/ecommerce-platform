//! Email marketing through the router (WP18): the newsletter flow on the storefront API
//! (sign-up, confirmation read vs. confirm, preferences, one-click unsubscribe, signed clicks),
//! the admin API (roles, fresh sign-in for exports, segments, campaigns, email log without
//! sensitive bodies, suppressions, branding and texts) and the SES/SNS bounce endpoint.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    runtime: PgPool,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "news").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        shop,
        runtime,
        _jwks: jwks,
    }
}

fn sf<'a>(call: Call<'a>, shop: &Shop) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
        .header("x-client-ip", "203.0.113.7")
}

async fn latest_body(c: &Ctx, to: &str, template: &str) -> String {
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let (html, text): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT html, body_text FROM email_messages WHERE to_email = $1 AND template = $2
         ORDER BY id DESC LIMIT 1",
    )
    .bind(to)
    .bind(template)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    format!("{}\n{}", html.unwrap_or_default(), text.unwrap_or_default())
}

fn param(body: &str, name: &str) -> String {
    let at = body.find(&format!("{name}=")).unwrap() + name.len() + 1;
    body[at..at + 64].to_owned()
}

#[sqlx::test(migrations = "../../migrations")]
async fn newsletter_flow_on_the_storefront_api(db: PgPool) {
    let c = setup(db).await;
    let email = "jana@example.com";
    let (status, body, _) = sf(
        Call::post(
            "/storefront/v1/newsletter/subscribe",
            json!({ "email": email }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "accepted");
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/newsletter/subscribe",
            json!({ "email": "nope" }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let mail = latest_body(&c, email, "newsletter_confirm").await;
    assert!(mail.contains("http://checkout.news.localhost:8080/newsletter/confirm?token="));
    let token = param(&mail, "token");
    let confirm_uri = format!("/storefront/v1/newsletter/confirmation?token={token}");
    let (status, body, res) = sf(Call::get(&confirm_uri), &c.shop).send(&c.s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], "j***@example.com");
    assert_eq!(res.headers()["cache-control"], "no-store");
    let (status, body, _) = sf(
        Call::post(
            "/storefront/v1/newsletter/confirmation",
            json!({ "token": token }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("subscribed"))
    );
    let (status, _, _) = sf(Call::get(&confirm_uri), &c.shop).send(&c.s).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "used tokens are gone");

    // A campaign to the (single) subscriber, sent by the batch job.
    let boss = sign(&claims("boss"));
    let (status, campaign, _) = Call::post(
        "/admin/v1/campaigns",
        json!({
            "name": "Září",
            "content": {"cs": {"subject": "Novinky", "blocks": [
                {"type": "heading", "text": "Ahoj"},
                {"type": "button", "label": "Do obchodu", "href": "/"},
                {"type": "personalized_products", "title": "Pro vás", "limit": 2}
            ]}}
        }),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::CREATED, "{campaign}");
    let id = campaign["id"].as_str().unwrap().to_owned();
    let (status, _, _) = Call::post(&format!("/admin/v1/campaigns/{id}/schedule"), json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    commerce::marketing::campaigns::run_batch(
        &c.runtime,
        &c.s.public_urls,
        c.shop.tenant,
        id.parse().unwrap(),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let mail = latest_body(&c, email, "campaign").await;
    assert!(mail.contains("Tričko") || mail.contains("Pro vás"));
    let t = param(&mail, "?t");

    let (status, body, _) = sf(
        Call::get(&format!("/storefront/v1/newsletter/preferences?t={t}")),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("subscribed"))
    );
    // Clicks: only signed targets.
    let (status, _, _) = sf(
        Call::get(&format!(
            "/storefront/v1/newsletter/click?t={t}&u=https%3A%2F%2Fevil.example%2F&s=00"
        )),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let start = mail.find("/_p/newsletter/click?").unwrap();
    let end = start + mail[start..].find('"').unwrap();
    let query = mail[start + "/_p/newsletter/click".len()..end].replace("&amp;", "&");
    let (status, body, _) = sf(
        Call::get(&format!("/storefront/v1/newsletter/click{query}")),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["url"]
            .as_str()
            .unwrap()
            .starts_with("http://news.localhost:8080/")
    );

    // One-click unsubscribe; repeating it is harmless.
    for _ in 0..2 {
        let (status, body, _) = sf(
            Call::post(
                "/storefront/v1/newsletter/unsubscribe",
                json!({ "token": t }),
            ),
            &c.shop,
        )
        .send(&c.s)
        .await;
        assert_eq!(
            (status, body["status"].as_str()),
            (StatusCode::OK, Some("unsubscribed"))
        );
    }
    // Resubscribing starts a fresh double opt-in.
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/newsletter/resubscribe",
            json!({ "token": t }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let again = latest_body(&c, email, "newsletter_confirm").await;
    assert_ne!(param(&again, "token"), token);

    // The campaign stats show it all.
    let (_, campaign, _) = Call::get(&format!("/admin/v1/campaigns/{id}"))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(campaign["status"], "sent");
    assert_eq!(campaign["stats"]["sent"], 1);
    assert_eq!(campaign["stats"]["clicked"], 1);
    assert_eq!(campaign["stats"]["unsubscribed"], 1);

    // Another shop's token cannot use this shop's links.
    let other = testkit::storefront::shop(&c.runtime, "news2").await;
    let (status, _, _) = sf(
        Call::get(&format!("/storefront/v1/newsletter/preferences?t={t}")),
        &other,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_marketing_and_email_settings(db: PgPool) {
    let c = setup(db).await;
    let boss = sign(&claims("boss"));
    let clerk = sign(&claims("clerk"));
    for e in ["a@example.com", "b@example.com"] {
        sf(
            Call::post("/storefront/v1/newsletter/subscribe", json!({ "email": e })),
            &c.shop,
        )
        .send(&c.s)
        .await;
    }
    let (status, page, _) = Call::get("/admin/v1/subscribers?status=pending&q=A%40")
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["email"], "a@example.com");

    // Export: owner/admin with a fresh sign-in only.
    let (status, _, _) = Call::get("/admin/v1/subscribers/export")
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut stale = claims("boss");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::get("/admin/v1/subscribers/export")
        .tenant(c.shop.tenant)
        .token(&sign(&stale))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (status, csv, res) = Call::get("/admin/v1/subscribers/export")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send_text(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    assert!(csv.starts_with("email,status,") && csv.contains("\"b@example.com\",\"pending\""));

    // Segments: preview and injection attempts in values.
    let (status, preview, _) = Call::post(
        "/admin/v1/segments/preview",
        json!({"conditions": [{"field": "purchased_brand", "brands": ["x' OR 1=1 --"]}]}),
    )
    .tenant(c.shop.tenant)
    .token(&clerk)
    .send(&c.s)
    .await;
    assert_eq!(
        (status, preview["count"].as_i64()),
        (StatusCode::OK, Some(0))
    );
    let (status, _, _) = Call::post(
        "/admin/v1/segments/preview",
        json!({"conditions": [{"field": "raw_sql", "sql": "true"}]}),
    )
    .tenant(c.shop.tenant)
    .token(&clerk)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, seg, _) = Call::post(
        "/admin/v1/segments",
        json!({"name": "Čeština", "rules": {"match": "all", "conditions": [{"field": "locale", "locales": ["cs"]}]}}),
    )
    .tenant(c.shop.tenant)
    .token(&clerk)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::CREATED, "{seg}");

    // Email log: sensitive confirmation mail shows no body.
    let (status, log, _) = Call::get("/admin/v1/emails?to=a%40example")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let msg = log["items"][0]["id"].as_str().unwrap().to_owned();
    let (_, detail, _) = Call::get(&format!("/admin/v1/emails/{msg}"))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(detail["sensitive"], true);
    assert_eq!(detail["html"], Value::Null);
    let (status, _, _) = Call::get("/admin/v1/emails")
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Suppressions: add, list, remove (audited).
    let (status, _, _) = Call::post(
        "/admin/v1/email-suppressions",
        json!({"email": "Blocked@Example.com", "note": "requested by phone"}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list, _) = Call::get("/admin/v1/email-suppressions")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(list["items"][0]["email"], "blocked@example.com");
    let (status, _, _) = Call::delete("/admin/v1/email-suppressions?email=blocked%40example.com")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Texts: a tenant subject, then the next confirmation mail uses it.
    let (status, text, _) = Call::put(
        "/admin/v1/email-templates/newsletter_confirm/cs",
        json!({"subject": "Potvrďte odběr u {shop}!", "intro": null}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text["default_subject"].as_str().unwrap().contains("{shop}"));
    let (status, _, _) = Call::put("/admin/v1/email-templates/nope/cs", json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    sf(
        Call::post(
            "/storefront/v1/newsletter/subscribe",
            json!({ "email": "c@example.com" }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let subject: String =
        sqlx::query_scalar("SELECT subject FROM email_messages WHERE to_email = 'c@example.com'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(subject.starts_with("Potvrďte odběr u ") && subject.ends_with('!'));
    tx.commit().await.unwrap();
    let (status, _, _) = Call::put(
        "/admin/v1/email-branding",
        json!({"logo_asset_id": uuid::Uuid::now_v7()}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown asset");
    let (status, _, _) = Call::put("/admin/v1/email-branding", json!({"logo_asset_id": null}))
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ses_bounces_need_the_secret_and_suppress(db: PgPool) {
    let c = setup(db).await;
    sf(
        Call::post(
            "/storefront/v1/newsletter/subscribe",
            json!({ "email": "gone@example.com" }),
        ),
        &c.shop,
    )
    .send(&c.s)
    .await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let message: uuid::Uuid = sqlx::query_scalar("SELECT id FROM email_messages")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let ses = json!({
        "notificationType": "Bounce",
        "bounce": {"bounceType": "Permanent", "bouncedRecipients": [{"emailAddress": "gone@example.com"}]},
        "mail": {"commonHeaders": {"messageId": format!("<{message}@mail.test>")}}
    });
    let sns = json!({
        "Type": "Notification",
        "MessageId": "sns-1",
        "TopicArn": "arn:aws:sns:eu-central-1:1:ses",
        "Message": ses.to_string()
    })
    .to_string();
    let basic = |password: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("ses:{password}"))
        )
    };
    let (status, _, _) = Call::post_raw("/webhooks/ses", sns.clone(), "text/plain")
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = Call::post_raw("/webhooks/ses", sns.clone(), "text/plain")
        .header("authorization", basic("wrong-secret-0123456789abcdef0123"))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = Call::post_raw("/webhooks/ses", sns.clone(), "text/plain")
        .header("authorization", basic(MAIL_EVENTS_SECRET))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["applied"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let boss = sign(&claims("boss"));
    let (_, list, _) = Call::get("/admin/v1/email-suppressions")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(list["items"][0]["email"], "gone@example.com");
    assert_eq!(list["items"][0]["reason"], "bounce");
    // Subscription confirmations are logged, never followed.
    let confirm = json!({"Type": "SubscriptionConfirmation", "MessageId": "x", "TopicArn": "t",
                         "SubscribeURL": "https://sns.eu-central-1.amazonaws.com/?Action=ConfirmSubscription"})
        .to_string();
    let (status, body, _) = Call::post_raw("/webhooks/ses", confirm, "text/plain")
        .header("authorization", basic(MAIL_EVENTS_SECRET))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["applied"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    // Not configured: 404.
    let off = api::AppState {
        mail_events: None,
        ..c.s.clone()
    };
    let (status, _, _) = Call::post_raw("/webhooks/ses", sns, "text/plain")
        .header("authorization", basic(MAIL_EVENTS_SECRET))
        .send(&off)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
