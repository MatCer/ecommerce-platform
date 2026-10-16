use std::time::Duration;

use axum::http::StatusCode;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject};
use commerce::content::LegalType;
use commerce::content::legal::{self, InstallInput};
use commerce::experiments;
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::common::*;

const ENROL: &str = "/internal/v1/experiments/enrol";
const SUBJECT: &str = "0123456789abcdef0123456789abcdef";

async fn draft(runtime: &PgPool, tenant: Uuid) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO experiments (tenant_id, name, version, arms)
           VALUES ($1, 'homepage', 1,
                   '[{"key":"control","weight":50,"config":{}},{"key":"b","weight":50,"config":{}}]')
           RETURNING id"#,
    )
    .bind(tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn start(runtime: &PgPool, tenant: Uuid) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let installed = legal::install(
        &mut tx,
        "test",
        &InstallInput {
            locales: vec![],
            types: vec![LegalType::Cookies],
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE pages SET status = 'published', published_at = now() WHERE id = $1")
        .bind(installed.created[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let id = draft(runtime, tenant).await;
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    experiments::start(&mut tx, "test", id).await.unwrap();
    tx.commit().await.unwrap();
    id
}

fn as_owner<'a>(call: Call<'a>, token: &'a str, tenant: Uuid) -> Call<'a> {
    call.token(token).tenant(tenant)
}

#[sqlx::test(migrations = "../../migrations")]
async fn staff_start_an_experiment_once_the_published_cookie_page_covers_it(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-start").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (owner, clerk) = (sign(&claims("boss")), sign(&claims("clerk")));
    let id = draft(&runtime, shop.tenant).await;
    let uri = format!("/admin/v1/experiments/{id}/start");
    let admin = |call| as_owner(call, &owner, shop.tenant);
    let post_start = || admin(Call::post_raw(&uri, "", "application/json"));

    let (status, problem, _) = post_start().send(&s).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("cookie_policy_outdated")
        ),
        "no cookie page"
    );

    let (status, installed, _) = admin(Call::post(
        "/admin/v1/legal/templates/install",
        json!({"types": ["cookies"]}),
    ))
    .send(&s)
    .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    let page_uri = format!(
        "/admin/v1/pages/{}",
        installed["created"][0].as_str().unwrap()
    );
    let (_, page, _) = admin(Call::get(&page_uri)).send(&s).await;
    assert_eq!(page["legal_template_version"], consent::TEXT_VERSION);
    let (status, _, _) = post_start().send(&s).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "draft page");

    let mut input = json!({
        "kind": page["kind"], "legal_type": page["legal_type"], "status": "published",
        "published_at": null, "image_asset_id": null, "translations": page["translations"],
    });
    let (status, published, _) = admin(Call::put(&page_uri, input.clone())).send(&s).await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["legal_template_version"], consent::TEXT_VERSION);

    input["translations"][0]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "rich_text", "html": "<p>Doplněno.</p>"}));
    input["still_covers_experiments"] = json!(true);
    let (_, confirmed, _) = admin(Call::put(&page_uri, input.clone())).send(&s).await;
    assert_eq!(confirmed["legal_template_version"], consent::TEXT_VERSION);

    let (status, _, _) = Call::post_raw(&uri, "", "application/json")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff role");

    let (status, body, _) = post_start().send(&s).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, problem, _) = post_start().send(&s).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::CONFLICT, Some("experiment_not_draft"))
    );
    let missing = format!("/admin/v1/experiments/{}/start", Uuid::now_v7());
    let (status, _, _) = admin(Call::post_raw(&missing, "", "application/json"))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    input["translations"][1]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "rich_text", "html": "<p>Doplnené.</p>"}));
    input["still_covers_experiments"] = json!(false);
    let (_, edited, _) = admin(Call::put(&page_uri, input)).send(&s).await;
    assert_eq!(edited["legal_template_version"], json!(null));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_edge_enrols_consenting_visitors_with_its_service_token(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-api").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let body = json!({"tenant_id": shop.tenant, "subject": SUBJECT});

    let (status, _, _) = Call::post(ENROL, body.clone()).send(&s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = Call::post(ENROL, body.clone())
        .token("wrong-token")
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for bad in [
        json!({"tenant_id": shop.tenant, "subject": "not-hex"}),
        json!({"tenant_id": shop.tenant, "subject": SUBJECT, "arm": "b"}),
        json!({"subject": SUBJECT}),
    ] {
        let (status, problem, _) = Call::post(ENROL, bad.clone())
            .token(SERVICE_TOKEN)
            .send(&s)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {problem}");
    }

    let (status, body_none, _) = Call::post(ENROL, body.clone())
        .token(SERVICE_TOKEN)
        .send(&s)
        .await;
    assert_eq!(
        (status, body_none),
        (StatusCode::OK, json!({"assignment": null})),
        "no experiment"
    );

    let id = start(&runtime, shop.tenant).await;
    let (status, body_none, _) = Call::post(ENROL, body.clone())
        .token(SERVICE_TOKEN)
        .send(&s)
        .await;
    assert_eq!(
        (status, body_none),
        (StatusCode::OK, json!({"assignment": null})),
        "no consent"
    );

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    consent::record(
        &mut tx,
        &Subject::Anon(SUBJECT.into()),
        &ConsentChoice {
            purposes: Purposes {
                analytics: Some(true),
                ..Purposes::default()
            },
            text_version: consent::TEXT_VERSION.into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (status, enrolled, _) = Call::post(ENROL, body.clone())
        .token(SERVICE_TOKEN)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let arm = enrolled["assignment"]["arm"].as_str().unwrap().to_owned();
    assert!(arm == "control" || arm == "b", "{arm}");
    assert_eq!(
        enrolled,
        json!({"assignment": {"experiment_id": id, "version": 1, "arm": arm}})
    );
    let (_, again, _) = Call::post(ENROL, body).token(SERVICE_TOKEN).send(&s).await;
    assert_eq!(again, enrolled);
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_names_the_running_experiment(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-res").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let uri = "/internal/v1/resolve?host=exp-res.localhost";

    let (status, body, _) = Call::get(uri).token(SERVICE_TOKEN).send(&s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["experiment"], json!(null));

    let id = start(&runtime, shop.tenant).await;
    let (_, body, _) = Call::get(uri).token(SERVICE_TOKEN).send(&s).await;
    assert_eq!(body["experiment"], json!({"id": id, "version": 1}));
}

#[sqlx::test(migrations = "../../migrations")]
async fn enrolment_is_rate_limited_per_tenant_and_client_ip(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-rate").await;
    let mut s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    s.rate_limit = std::sync::Arc::new(api::rate_limit::StorefrontLimiter::new(1, 2));
    let enrol = |ip: &'static str| {
        Call::post(ENROL, json!({"tenant_id": shop.tenant, "subject": SUBJECT}))
            .token(SERVICE_TOKEN)
            .header("x-client-ip", ip)
    };

    for _ in 0..2 {
        let (status, _, _) = enrol("203.0.113.7").send(&s).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, problem, res) = enrol("203.0.113.7").send(&s).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert_eq!(res.headers()["retry-after"], "1");
    let (status, _, _) = enrol("198.51.100.1").send(&s).await;
    assert_eq!(status, StatusCode::OK, "another client IP");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_recorded_choice_tells_the_edge_whether_its_analytics_grant_covers_experiments(
    db: PgPool,
) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-flag").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let mut seen = vec![];
    for purposes in [
        json!({"analytics": true}),
        json!({"personalization": true}),
        json!({"analytics": false}),
    ] {
        for version in [consent::TEXT_VERSION, "2026-09-25"] {
            let (status, body, res) = Call::post(
                "/storefront/v1/consent",
                json!({"purposes": purposes, "text_version": version}),
            )
            .header("x-storefront-token", shop.token.clone())
            .header("x-market", shop.cz.to_string())
            .header("x-consent-subject", SUBJECT)
            .send(&s)
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            seen.push(
                res.headers()
                    .get("x-consent-experiments")
                    .map(|v| v.to_str().unwrap().to_owned()),
            );
        }
    }
    assert_eq!(
        seen,
        [Some("1".into()), None, None, None, None, None,],
        "analytics granted under the experiments wording, then the old wording; a later choice \
         without analytics keeps the old analytics record; withdrawal"
    );
}
