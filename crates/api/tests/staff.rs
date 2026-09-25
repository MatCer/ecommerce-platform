#![allow(clippy::unwrap_used)]
mod common;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;

#[sqlx::test(migrations = "../../migrations")]
async fn staff_list_requires_admin(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let s = state(
        testkit::runtime_pool(&db, 4).await,
        &jwks,
        Duration::from_secs(30),
    );
    let (tenant, _) = testkit::tenant(&s.db, "staff-list").await;
    for (user, role, expected) in [
        ("owner", "owner", StatusCode::OK),
        ("admin", "admin", StatusCode::OK),
        ("staff", "staff", StatusCode::FORBIDDEN),
    ] {
        testkit::staff(&s.db, tenant, user, role).await;
        let (status, body, _) = Call::get("/admin/v1/staff")
            .tenant(tenant)
            .token(&sign(&claims(user)))
            .send(&s)
            .await;
        assert_eq!(status, expected, "{body}");
        if role == "staff" {
            assert_eq!(body["code"], "insufficient_role");
        }
    }
}

async fn setup(db: &PgPool) -> (api::AppState, Jwks, uuid::Uuid, String) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let s = state(
        testkit::runtime_pool(db, 4).await,
        &jwks,
        Duration::from_secs(30),
    );
    let (tenant, _) = testkit::tenant(&s.db, "staff-test").await;
    testkit::staff(&s.db, tenant, "owner", "owner").await;
    (s, jwks, tenant, sign(&claims("owner")))
}
async fn member_id(s: &api::AppState, tenant: uuid::Uuid, user: &str) -> String {
    let mut tx = platform::db::tenant_tx(&s.db, tenant).await.unwrap();
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM staff_members WHERE user_id = $1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    id.to_string()
}
#[sqlx::test(migrations = "../../migrations")]
async fn invitation_calls_auth_and_audits_then_rejects_duplicate(db: PgPool) {
    let (s, jwks, tenant, token) = setup(&db).await;
    let body = json!({"email": "  New@Example.test ", "role": "admin"});
    let (status, member, _) = Call::post("/admin/v1/staff/invitations", body.clone())
        .tenant(tenant)
        .token(&token)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{member}");
    assert_eq!(member["email"], "new@example.test");
    assert_eq!(member["role"], "admin");
    assert_eq!(member["user_id"], "auth:new@example.test");
    let requests = jwks.auth_requests.read().await.clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "/internal/users");
    assert_eq!(requests[1].0, "/internal/users/invite");
    for request in &requests {
        assert_eq!(request.1, format!("Bearer {SERVICE_TOKEN}"));
    }
    assert_eq!(
        requests[0].2,
        json!({"email":"new@example.test", "name":"new@example.test"})
    );
    assert_eq!(
        requests[1].2,
        json!({"email":"new@example.test", "callback_url":ADMIN_ORIGIN})
    );
    assert_eq!(
        member_id(&s, tenant, "auth:new@example.test").await,
        member["id"].as_str().unwrap()
    );
    let mut tx = platform::db::tenant_tx(&s.db, tenant).await.unwrap();
    let audit = commerce::audit::list(&mut tx, None, 10).await.unwrap();
    assert_eq!(audit.items[0].action, "staff.invited");
    assert_eq!(audit.items[0].actor, "owner");
    assert_eq!(audit.items[0].entity, "staff_member");
    assert_eq!(audit.items[0].entity_id.as_deref(), member["id"].as_str());
    assert_eq!(
        audit.items[0].diff,
        json!({"email":"new@example.test", "role":"admin"})
    );
    tx.commit().await.unwrap();
    let (status, body, _) = Call::post("/admin/v1/staff/invitations", body)
        .tenant(tenant)
        .token(&token)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "already_member");
    assert_eq!(jwks.auth_requests.read().await.len(), 3);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invitation_validation_freshness_and_auth_failures(db: PgPool) {
    use std::sync::atomic::Ordering;
    let (s, jwks, tenant, token) = setup(&db).await;
    let input = json!({"email":"new@example.test", "role":"staff"});
    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 901);
    let (status, body, _) = Call::post("/admin/v1/staff/invitations", input.clone())
        .tenant(tenant)
        .token(&sign(&stale))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "reauth_required");
    for email in [
        "bad",
        "@a.b",
        "a@b",
        "a@b.",
        "a b@c.d",
        &format!("{}@a.b", "x".repeat(251)),
    ] {
        let (status, body, _) = Call::post(
            "/admin/v1/staff/invitations",
            json!({"email":email,"role":"staff"}),
        )
        .tenant(tenant)
        .token(&token)
        .send(&s)
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid_email");
    }
    let (status, _, _) = Call::post(
        "/admin/v1/staff/invitations",
        json!({"email":"a@b.c","role":"staff","extra":true}),
    )
    .tenant(tenant)
    .token(&token)
    .send(&s)
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(jwks.auth_requests.read().await.is_empty());
    for stage in [1, 2] {
        jwks.auth_failure.store(stage, Ordering::SeqCst);
        let (status, body, _) = Call::post("/admin/v1/staff/invitations", input.clone())
            .tenant(tenant)
            .token(&token)
            .send(&s)
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let mut tx = platform::db::tenant_tx(&s.db, tenant).await.unwrap();
        assert_eq!(
            commerce::staff::list(&mut tx, "owner").await.unwrap().len(),
            1
        );
        assert!(
            commerce::audit::list(&mut tx, None, 10)
                .await
                .unwrap()
                .items
                .is_empty()
        );
    }

    // Without the auth service configured the API still runs; invitations answer 503.
    let unconfigured = api::AppState {
        auth_service: None,
        ..s.clone()
    };
    let (status, _, _) = Call::post("/admin/v1/staff/invitations", input.clone())
        .tenant(tenant)
        .token(&token)
        .send(&unconfigured)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
#[sqlx::test(migrations = "../../migrations")]
async fn role_changes_last_owner_and_removal(db: PgPool) {
    let (s, jwks, tenant, owner) = setup(&db).await;
    testkit::staff(&s.db, tenant, "admin", "admin").await;
    testkit::staff(&s.db, tenant, "employee", "staff").await;
    let admin = sign(&claims("admin"));
    let owner_path = format!("/admin/v1/staff/{}", member_id(&s, tenant, "owner").await);
    let admin_path = format!("/admin/v1/staff/{}", member_id(&s, tenant, "admin").await);
    let employee_path = format!(
        "/admin/v1/staff/{}",
        member_id(&s, tenant, "employee").await
    );
    for call in [
        Call::post(
            "/admin/v1/staff/invitations",
            json!({"email":"x@y.z","role":"owner"}),
        ),
        Call::patch(&owner_path, json!({"role":"admin"})),
        Call::patch(&admin_path, json!({"role":"owner"})),
        Call::delete(&owner_path),
    ] {
        let (status, body, _) = call.tenant(tenant).token(&admin).send(&s).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "insufficient_role");
    }
    assert!(jwks.auth_requests.read().await.is_empty());
    for call in [
        Call::patch(&owner_path, json!({"role":"admin"})),
        Call::delete(&owner_path),
    ] {
        let (status, body, _) = call.tenant(tenant).token(&owner).send(&s).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "last_owner");
    }
    let (status, body, _) = Call::patch(&employee_path, json!({"role":"admin"}))
        .tenant(tenant)
        .token(&admin)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], "admin");
    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 901);
    for call in [
        Call::patch(&admin_path, json!({"role":"staff"})),
        Call::delete(&admin_path),
    ] {
        let (status, body, _) = call.tenant(tenant).token(&sign(&stale)).send(&s).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "reauth_required");
    }
    assert_eq!(
        Call::patch(&admin_path, json!({"role":"owner"}))
            .tenant(tenant)
            .token(&owner)
            .send(&s)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        Call::patch(&owner_path, json!({"role":"admin"}))
            .tenant(tenant)
            .token(&owner)
            .send(&s)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        Call::delete(&employee_path)
            .tenant(tenant)
            .token(&admin)
            .send(&s)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (status, body, _) = Call::get("/admin/v1/staff")
        .tenant(tenant)
        .token(&sign(&claims("employee")))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "not_a_member");
    let mut tx = platform::db::tenant_tx(&s.db, tenant).await.unwrap();
    let audit = commerce::audit::list(&mut tx, None, 10).await.unwrap();
    assert_eq!(audit.items[0].action, "staff.removed");
    assert_eq!(
        audit.items[0].diff,
        json!({"email":"employee@example.test","role":"admin"})
    );
    assert_eq!(audit.items[1].action, "staff.role_changed");
    assert_eq!(audit.items[1].diff, json!({"from":"owner","to":"admin"}));
}
#[sqlx::test(migrations = "../../migrations")]
async fn staff_cross_tenant_access_is_denied(db: PgPool) {
    let (s, _, tenant_a, owner) = setup(&db).await;
    let (tenant_b, _) = testkit::tenant(&s.db, "other-shop").await;
    testkit::staff(&s.db, tenant_b, "other-owner", "owner").await;
    let target = format!(
        "/admin/v1/staff/{}",
        member_id(&s, tenant_b, "other-owner").await
    );
    for call in [
        Call::get("/admin/v1/staff"),
        Call::patch(&target, json!({"role":"staff"})),
        Call::delete(&target),
    ] {
        let (status, body, _) = call.tenant(tenant_b).token(&owner).send(&s).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "not_a_member");
    }
    for call in [
        Call::patch(&target, json!({"role":"staff"})),
        Call::delete(&target),
    ] {
        let (status, body, _) = call.tenant(tenant_a).token(&owner).send(&s).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn invitations_follow_role_rules_and_staff_cannot_mutate(db: PgPool) {
    let (s, _, tenant, owner) = setup(&db).await;
    testkit::staff(&s.db, tenant, "admin", "admin").await;
    testkit::staff(&s.db, tenant, "staff", "staff").await;
    let target = format!("/admin/v1/staff/{}", member_id(&s, tenant, "admin").await);
    for call in [
        Call::post(
            "/admin/v1/staff/invitations",
            json!({"email":"denied@x.test","role":"staff"}),
        ),
        Call::patch(&target, json!({"role":"staff"})),
        Call::delete(&target),
    ] {
        let (status, body, _) = call
            .tenant(tenant)
            .token(&sign(&claims("staff")))
            .send(&s)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "insufficient_role");
    }
    for (actor, role, email) in [
        ("admin", "staff", "new-staff@x.test"),
        ("owner", "owner", "new-owner@x.test"),
    ] {
        let (status, body, _) = Call::post(
            "/admin/v1/staff/invitations",
            json!({"email":email,"role":role}),
        )
        .tenant(tenant)
        .token(&sign(&claims(actor)))
        .send(&s)
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["role"], role);
    }
    assert_eq!(
        Call::patch(&target, json!({"role":"staff","extra":true}))
            .tenant(tenant)
            .token(&owner)
            .send(&s)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let unknown = format!("/admin/v1/staff/{}", uuid::Uuid::now_v7());
    for call in [
        Call::patch(&unknown, json!({"role":"staff"})),
        Call::delete(&unknown),
    ] {
        assert_eq!(
            call.tenant(tenant).token(&owner).send(&s).await.0,
            StatusCode::NOT_FOUND
        );
    }
    let target = format!(
        "/admin/v1/staff/{}",
        member_id(&s, tenant, "auth:new-owner@x.test").await
    );
    assert_eq!(
        Call::delete(&target)
            .tenant(tenant)
            .token(&owner)
            .send(&s)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}
