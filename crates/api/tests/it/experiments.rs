#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::common::*;

const ENROL: &str = "/internal/v1/experiments/enrol";
const SUBJECT: &str = "0123456789abcdef0123456789abcdef";

async fn start(runtime: &PgPool, tenant: Uuid) -> Uuid {
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
    sqlx::query("UPDATE experiments SET status = 'running' WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
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
