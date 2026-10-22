use std::time::Duration;

use axum::http::StatusCode;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject};
use commerce::content::LegalType;
use commerce::content::legal::{self, InstallInput};
use commerce::experiments;
use platform::db::tenant_tx;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::common::*;

const ENROL: &str = "/internal/v1/experiments/enrol";
const SUBJECT: &str = "0123456789abcdef0123456789abcdef";

fn two_empty_arms() -> serde_json::Value {
    json!([
        {"key": "control", "weight": 50, "config": {}},
        {"key": "b", "weight": 50, "config": {}},
    ])
}

async fn draft(runtime: &PgPool, tenant: Uuid) -> Uuid {
    draft_with(runtime, tenant, two_empty_arms()).await
}

async fn draft_with(runtime: &PgPool, tenant: Uuid, arms: serde_json::Value) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO experiments (tenant_id, name, version, arms)
         VALUES ($1, 'homepage', 1, $2) RETURNING id",
    )
    .bind(tenant)
    .bind(arms)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn start(runtime: &PgPool, tenant: Uuid) -> Uuid {
    start_with(runtime, tenant, two_empty_arms()).await
}

async fn start_with(runtime: &PgPool, tenant: Uuid, arms: serde_json::Value) -> Uuid {
    publish_cookie_page(runtime, tenant).await;
    let id = draft_with(runtime, tenant, arms).await;
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    experiments::start(&mut tx, "test", id).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn publish_cookie_page(runtime: &PgPool, tenant: Uuid) {
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
    assert_eq!(page["legal_template_version"], legal::TEMPLATE_VERSION);
    let (status, _, _) = post_start().send(&s).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "draft page");

    let mut input = json!({
        "kind": page["kind"], "legal_type": page["legal_type"], "status": "published",
        "published_at": null, "image_asset_id": null, "translations": page["translations"],
    });
    let (status, published, _) = admin(Call::put(&page_uri, input.clone())).send(&s).await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["legal_template_version"], legal::TEMPLATE_VERSION);

    input["translations"][0]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "rich_text", "html": "<p>Doplněno.</p>"}));
    input["still_covers_experiments"] = json!(true);
    let (_, confirmed, _) = admin(Call::put(&page_uri, input.clone())).send(&s).await;
    assert_eq!(confirmed["legal_template_version"], legal::TEMPLATE_VERSION);

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
    assert_eq!(
        body["experiment"],
        json!({"id": id, "version": 1, "name": "homepage", "theme_arm": null})
    );
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

async fn replace_running(
    runtime: &PgPool,
    tenant: Uuid,
    version: i32,
    b: serde_json::Value,
) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "UPDATE experiments SET status = 'stopped', stop_reason = 'manual'
         WHERE status = 'running'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO experiments (tenant_id, name, version, arms, status, started_at)
         VALUES ($1, 'homepage', $2, $3, 'running', now()) RETURNING id",
    )
    .bind(tenant)
    .bind(version)
    .bind(json!([
        {"key": "control", "weight": 1, "config": {}},
        {"key": "b", "weight": 1, "config": b},
    ]))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_edges_arm_overrides_the_home_hero_in_the_page_model(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-hero").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let id = start_with(
        &runtime,
        shop.tenant,
        json!([
            {"key": "control", "weight": 1, "config": {}},
            {"key": "b", "weight": 1, "config": {"hero": {
                "title": {"cs": "Nová kolekce je tady", "sk": "Nová kolekcia je tu"},
                "subtitle": {"cs": "Doprava zdarma nad 1 500 Kč"},
                "cta": {"cs": "Prohlédnout kolekci"},
            }}},
        ]),
    )
    .await;
    let home = async |market: Uuid, experiment: Option<String>| {
        let mut call = Call::get("/storefront/v1/pages/home")
            .header("x-storefront-token", shop.token.clone())
            .header("x-market", market.to_string());
        if let Some(e) = experiment {
            call = call.header("x-experiment", e);
        }
        let (status, body, _) = call.send(&s).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["hero"].clone()
    };

    let control = home(shop.cz, None).await;
    let b = home(shop.cz, Some(format!("{id}:1:b"))).await;
    assert_eq!(b["title"], "Nová kolekce je tady");
    assert_eq!(b["subtitle"], "Doprava zdarma nad 1 500 Kč");
    assert_eq!(b["cta"]["label"], "Prohlédnout kolekci");
    assert_eq!(b["cta"]["href"], control["cta"]["href"]);
    assert_eq!(b["image"], control["image"]);

    let sk_control = home(shop.sk, None).await;
    let sk_b = home(shop.sk, Some(format!("{id}:1:b"))).await;
    assert_eq!(sk_b["title"], "Nová kolekcia je tu");
    assert_eq!(
        sk_b["subtitle"], "Doprava zdarma nad 1 500 Kč",
        "a field without the locale falls back like other i18n texts"
    );

    let other = Uuid::now_v7();
    for (header, why) in [
        (format!("{id}:1:control"), "the control arm"),
        (format!("{id}:2:b"), "another version"),
        (format!("{other}:1:b"), "another experiment"),
        (format!("{id}:1:nope"), "no such arm"),
        ("garbage".to_owned(), "a malformed header"),
    ] {
        assert_eq!(
            home(shop.cz, Some(header)).await,
            control,
            "{why} renders control"
        );
    }
    assert_eq!(
        home(shop.sk, Some(format!("{id}:1:control"))).await,
        sk_control
    );

    let title_only = replace_running(
        &runtime,
        shop.tenant,
        2,
        json!({"hero": {"title": {"cs": "Jen titulek"}}}),
    )
    .await;
    let page = home(shop.cz, Some(format!("{title_only}:2:b"))).await;
    assert_eq!(page["title"], "Jen titulek");
    assert_eq!(page["subtitle"], control["subtitle"]);
    assert_eq!(page["cta"], control["cta"]);
    assert_eq!(
        home(shop.cz, Some(format!("{id}:1:b"))).await,
        control,
        "a stopped experiment renders control"
    );

    for (version, config, why) in [
        (
            3,
            json!({"tokens": {"color": "red"}}),
            "a config key the page model does not know",
        ),
        (
            4,
            json!({"hero": {"title": {"cs": "x".repeat(201)}}}),
            "a title over 200 characters",
        ),
        (5, json!({"hero": {"title": {"cs": "  "}}}), "a blank title"),
    ] {
        let id = replace_running(&runtime, shop.tenant, version, config).await;
        assert_eq!(
            home(shop.cz, Some(format!("{id}:{version}:b"))).await,
            control,
            "{why} renders control"
        );
    }
}

const EXPERIMENTS: &str = "/admin/v1/experiments";

fn keys(v: &serde_json::Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn admins_create_read_stop_and_promote_an_experiment(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-admin").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let owner = sign(&claims("boss"));
    let admin = |call| as_owner(call, &owner, shop.tenant);
    let input = json!({
        "name": "homepage",
        "change": {"kind": "hero", "hero": {"title": {"cs": "Nová kolekce", "sk": "Nová kolekcia"}}},
    });

    let (status, created, res) = admin(Call::post(EXPERIMENTS, input.clone()).key("exp-1"))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(res.headers().get("idempotent-replayed").is_none());
    assert_eq!(
        keys(&created),
        [
            "change",
            "created_at",
            "id",
            "name",
            "promoted_at",
            "started_at",
            "status",
            "stop_reason",
            "stopped_at",
            "version"
        ]
    );
    assert_eq!(
        (
            &created["name"],
            &created["version"],
            &created["status"],
            &created["change"]
        ),
        (
            &json!("homepage"),
            &json!(1),
            &json!("draft"),
            &json!({"kind": "hero", "hero": {"title": {"cs": "Nová kolekce", "sk": "Nová kolekcia"}}})
        )
    );
    let (status, replayed, res) = admin(Call::post(EXPERIMENTS, input).key("exp-1"))
        .send(&s)
        .await;
    assert_eq!((status, &replayed), (StatusCode::CREATED, &created));
    assert_eq!(res.headers()["idempotent-replayed"], "true");

    let (status, list, _) = admin(Call::get(EXPERIMENTS)).send(&s).await;
    assert_eq!(
        (status, list),
        (StatusCode::OK, json!({"items": [created]}))
    );
    let id = created["id"].as_str().unwrap();
    let one = format!("{EXPERIMENTS}/{id}");
    let (status, detail, _) = admin(Call::get(&one)).send(&s).await;
    assert_eq!(
        (status, detail),
        (
            StatusCode::OK,
            json!({"experiment": created, "readout": null})
        )
    );

    publish_cookie_page(&runtime, shop.tenant).await;
    let [start, stop, promote] = ["start", "stop", "promote"].map(|a| format!("{one}/{a}"));
    let post = |uri| Call::post_raw(uri, "", "application/json");
    let (status, _, _) = admin(post(&start)).send(&s).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, detail, _) = admin(Call::get(&one)).send(&s).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["experiment"]["status"], "running");
    let readout = &detail["readout"];
    assert_eq!(
        keys(readout),
        [
            "as_of",
            "control",
            "decides_from",
            "final",
            "min_converters",
            "state",
            "variant"
        ]
    );
    assert_eq!(
        readout["state"],
        json!({"state": "collecting", "need": "time"})
    );
    let control = &readout["control"];
    assert_eq!(
        keys(control),
        [
            "assigned",
            "conversion_rate",
            "converters",
            "expected_loss",
            "p_best",
            "subjects"
        ]
    );
    assert_eq!(
        [
            &control["assigned"],
            &control["subjects"],
            &control["converters"],
            &control["conversion_rate"]
        ],
        [&json!(0), &json!(0), &json!(0), &json!(null)]
    );

    for _ in 0..2 {
        let (status, body, _) = admin(post(&stop)).send(&s).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    let mut stale = claims("boss");
    stale["auth_time"] = json!(now() - 20 * 60);
    let stale = sign(&stale);
    let (status, problem, _) = post(&promote)
        .token(&stale)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (status, promoted, _) = admin(post(&promote)).send(&s).await;
    assert_eq!(status, StatusCode::OK, "{promoted}");
    assert_eq!(
        (&promoted["status"], &promoted["stop_reason"]),
        (&json!("stopped"), &json!("manual"))
    );
    assert!(promoted["promoted_at"].is_string(), "{promoted}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_admins_of_the_tenant_reach_its_experiments(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-roles").await;
    let (other, _) = testkit::tenant(&runtime, "exp-other").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other, "rival", "owner").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (clerk, rival) = (sign(&claims("clerk")), sign(&claims("rival")));
    let id = start(&runtime, shop.tenant).await;
    let one = format!("{EXPERIMENTS}/{id}");
    let routes = [
        ("POST", EXPERIMENTS.to_owned()),
        ("GET", EXPERIMENTS.to_owned()),
        ("GET", one.clone()),
        ("POST", format!("{one}/start")),
        ("POST", format!("{one}/stop")),
        ("POST", format!("{one}/promote")),
    ];
    let body = json!({"name": "other", "change": {"kind": "hero", "hero": {"title": {"cs": "X"}}}});
    fn call<'a>(method: &str, uri: &'a str, body: &Value) -> Call<'a> {
        match method {
            "GET" => Call::get(uri),
            _ => Call::post(uri, body.clone()),
        }
    }

    for (method, uri) in &routes {
        let (status, problem, _) = call(method, uri, &body)
            .token(&clerk)
            .tenant(shop.tenant)
            .send(&s)
            .await;
        assert_eq!(
            (status, problem["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("insufficient_role")),
            "{method} {uri}"
        );
    }
    for (method, uri) in &routes[2..] {
        let (status, _, _) = call(method, uri, &body)
            .token(&rival)
            .tenant(other)
            .send(&s)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (_, list, _) = Call::get(EXPERIMENTS)
        .token(&rival)
        .tenant(other)
        .send(&s)
        .await;
    assert_eq!(list, json!({"items": []}));
}
