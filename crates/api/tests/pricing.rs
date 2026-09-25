//! Pricing, promotions and inventory Admin API through the router (real Postgres as the
//! runtime role, locally signed staff JWTs).
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    owner: String,
    clerk: String,
    tenant: uuid::Uuid,
    market: uuid::Uuid,
    other_tenant: uuid::Uuid,
    runtime: PgPool,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, market) = testkit::tenant(&runtime, "shop").await;
    let (other_tenant, _) = testkit::tenant(&runtime, "other").await;
    testkit::staff(&runtime, tenant, "boss", "owner").await;
    testkit::staff(&runtime, tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other_tenant, "rival", "owner").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        owner: sign(&claims("boss")),
        clerk: sign(&claims("clerk")),
        tenant,
        market,
        other_tenant,
        runtime,
        _jwks: jwks,
    }
}

impl Ctx {
    async fn as_owner(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (status, body, _) = call
            .token(&self.owner)
            .tenant(self.tenant)
            .send(&self.s)
            .await;
        (status, body)
    }

    async fn as_clerk(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (status, body, _) = call
            .token(&self.clerk)
            .tenant(self.tenant)
            .send(&self.s)
            .await;
        (status, body)
    }
}

fn profile() -> Value {
    json!({
        "establishment_country": "CZ",
        "vat_payer": true,
        "vat_id": "CZ12345678",
        "distance_sales_mode": "destination"
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn tax_profile_needs_admin_and_fresh_login(db: PgPool) {
    let c = setup(db).await;
    let (status, _) = c.as_owner(Call::get("/admin/v1/tax-profile")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = c
        .as_clerk(Call::put("/admin/v1/tax-profile", profile()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let mut stale = claims("boss");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::put("/admin/v1/tax-profile", profile())
        .token(&sign(&stale))
        .tenant(c.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "reauth_required");

    let (status, body) = c
        .as_owner(Call::put("/admin/v1/tax-profile", profile()))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["distance_sales_mode"], "destination");
    let mut origin = profile();
    origin["distance_sales_mode"] = json!("origin_threshold");
    let (status, body) = c
        .as_owner(Call::put("/admin/v1/tax-profile", origin.clone()))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "origin_threshold_confirmation_required");
    origin["confirm_origin_threshold"] = json!(true);
    let (status, body) = c.as_owner(Call::put("/admin/v1/tax-profile", origin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["origin_threshold_confirmed_at"].is_string());
    let (status, body) = c.as_clerk(Call::get("/admin/v1/tax-profile")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["vat_id"], "CZ12345678");
}

#[sqlx::test(migrations = "../../migrations")]
async fn price_lists_prices_sales_and_history(db: PgPool) {
    let c = setup(db).await;
    let product = testkit::catalog::product(&c.runtime, c.tenant, "TS", 2).await;
    let (v1, v2) = (product.variants[0].id, product.variants[1].id);

    let list_body =
        json!({ "code": "cz", "name": "Česko", "currency": "CZK", "market_ids": [c.market] });
    let (status, _) = c
        .as_clerk(Call::post("/admin/v1/price-lists", list_body.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, list) = c
        .as_owner(Call::post("/admin/v1/price-lists", list_body.clone()).key("pl-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{list}");
    assert_eq!(list["market_ids"], json!([c.market]));
    let (status, replay) = c
        .as_owner(Call::post("/admin/v1/price-lists", list_body).key("pl-1"))
        .await;
    assert_eq!((status, &replay), (StatusCode::CREATED, &list));
    let list_id = list["id"].as_str().unwrap().to_owned();
    let (status, body) = c
        .as_owner(Call::post(
            "/admin/v1/price-lists",
            json!({ "code": "eu", "name": "EU", "currency": "EUR", "market_ids": [c.market] }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "currency_mismatch");

    let prices = format!("/admin/v1/price-lists/{list_id}/prices");
    let (status, body) = c
        .as_clerk(Call::put(
            &prices,
            json!({ "items": [
                { "variant_id": v1, "amount_minor": 129_000, "compare_at_minor": 149_000 },
                { "variant_id": v2, "amount_minor": 139_000 }
            ] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    let (status, body) = c
        .as_clerk(Call::put(
            &prices,
            json!({ "items": [{ "variant_id": v1, "amount_minor": -1 }] }),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_amount"))
    );
    let (status, page) = c.as_clerk(Call::get(&format!("{prices}?limit=1"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["currency"], "CZK");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["next_cursor"].is_string());

    // A sale scheduled in the future.
    let starts = Utc::now() + chrono::Duration::days(3);
    let sale_body = json!({
        "name": "Podzim",
        "discount": { "type": "percent", "basis_points": 2000 },
        "starts_at": starts,
        "ends_at": starts + chrono::Duration::days(7),
        "targets": { "product_ids": [product.id] }
    });
    let (status, sale) = c
        .as_clerk(Call::post("/admin/v1/sales", sale_body).key("sale-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sale}");

    let history_url = format!(
        "/admin/v1/products/{}/price-history?price_list_id={list_id}",
        product.id
    );
    let (status, history) = c.as_clerk(Call::get(&history_url)).await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let h1 = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["variant_id"] == v1.to_string())
        .unwrap();
    let causes: Vec<&str> = h1["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["cause"].as_str().unwrap())
        .collect();
    assert_eq!(causes, ["base", "sale", "base"]);
    assert_eq!(h1["intervals"][1]["amount_minor"], 103_200);
    assert_eq!(h1["price"]["compare_at_minor"], 149_000);
    assert_eq!(h1["omnibus"]["current_minor"], 129_000);
    assert_eq!(h1["omnibus"]["on_sale"], false);
    // Evaluated at the sale: launched < 30 days ago, reference = lowest since launch.
    let at = (starts + chrono::Duration::hours(1))
        .to_rfc3339()
        .replace('+', "%2B");
    let (_, during) = c
        .as_clerk(Call::get(&format!("{history_url}&at={at}")))
        .await;
    let d1 = &during["items"][0];
    assert_eq!(d1["omnibus"]["on_sale"], true);
    assert_eq!(d1["omnibus"]["reference_minor"], 129_000);
    assert_eq!(d1["omnibus"]["discount_percent"], 20);
    assert_eq!(d1["omnibus"]["claim"], true);

    // Updating and deleting the sale.
    let sale_url = format!("/admin/v1/sales/{}", sale["id"].as_str().unwrap());
    let (status, body) = c
        .as_clerk(Call::put(
            &sale_url,
            json!({ "name": "Podzim 2", "discount": { "type": "percent", "basis_points": 1000 },
                    "starts_at": starts, "targets": { "all": true } }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ends_at"], Value::Null);
    let (status, list) = c.as_clerk(Call::get("/admin/v1/sales")).await;
    assert_eq!(
        (status, list["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
    let (status, _) = c.as_clerk(Call::delete(&sale_url)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, history) = c.as_clerk(Call::get(&history_url)).await;
    assert_eq!(
        history["items"][0]["intervals"].as_array().unwrap().len(),
        1
    );

    let (status, _) = c.as_clerk(Call::delete(&format!("{prices}/{v2}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = c.as_clerk(Call::delete(&format!("{prices}/{v2}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Other tenants see nothing.
    let rival = sign(&claims("rival"));
    for url in [
        format!("/admin/v1/price-lists/{list_id}"),
        history_url.clone(),
        prices.clone(),
    ] {
        let (status, _, _) = Call::get(&url)
            .token(&rival)
            .tenant(c.other_tenant)
            .send(&c.s)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{url}");
    }

    let (_, audit) = c.as_owner(Call::get("/admin/v1/audit-log?limit=100")).await;
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    for want in [
        "price_list.created",
        "variant_prices.upserted",
        "sale.created",
        "sale.updated",
        "sale.deleted",
        "variant_price.deleted",
    ] {
        assert!(actions.contains(&want), "{want} missing from {actions:?}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn coupons_crud(db: PgPool) {
    let c = setup(db).await;
    let body = json!({
        "code": "podzim10",
        "discount": { "type": "fixed", "amount_minor": 10_000 },
        "currency": "CZK",
        "min_subtotal_minor": 100_000,
        "usage_limit": 100,
        "published": true
    });
    let (status, coupon) = c
        .as_clerk(Call::post("/admin/v1/coupons", body.clone()).key("c-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{coupon}");
    assert_eq!(coupon["code"], "PODZIM10");
    let (status, dup) = c
        .as_clerk(Call::post("/admin/v1/coupons", body.clone()))
        .await;
    assert_eq!(
        (status, dup["code"].as_str()),
        (StatusCode::CONFLICT, Some("code_taken"))
    );
    let url = format!("/admin/v1/coupons/{}", coupon["id"].as_str().unwrap());
    // Started and published: the terms are fixed, the end and limits are not.
    let mut changed = body.clone();
    changed["discount"] = json!({ "type": "fixed", "amount_minor": 20_000 });
    let (status, err) = c.as_clerk(Call::put(&url, changed)).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("coupon_started"))
    );
    let mut limits = body.clone();
    limits["usage_limit"] = json!(5);
    limits["ends_at"] = json!(Utc::now() + chrono::Duration::days(1));
    let (status, updated) = c.as_clerk(Call::put(&url, limits)).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["usage_limit"], 5);
    let (status, err) = c.as_clerk(Call::delete(&url)).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("coupon_in_use"))
    );
    let (status, page) = c.as_clerk(Call::get("/admin/v1/coupons")).await;
    assert_eq!(
        (status, page["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );

    let (status, draft) = c
        .as_clerk(Call::post(
            "/admin/v1/coupons",
            json!({ "code": "DOPRAVA", "discount": { "type": "free_shipping" } }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, _) = c
        .as_clerk(Call::delete(&format!(
            "/admin/v1/coupons/{}",
            draft["id"].as_str().unwrap()
        )))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, bad) = c
        .as_clerk(Call::post(
            "/admin/v1/coupons",
            json!({ "code": "X1X", "discount": { "type": "fixed", "amount_minor": 5 } }),
        ))
        .await;
    assert_eq!(
        (status, bad["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("currency_required"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn inventory_levels_and_adjustments(db: PgPool) {
    let c = setup(db).await;
    let product = testkit::catalog::product(&c.runtime, c.tenant, "INV", 2).await;
    let v = product.variants[0].id;
    let adjust = format!("/admin/v1/inventory/{v}/adjustments");

    let (status, first) = c
        .as_clerk(Call::post(&adjust, json!({ "on_hand": 10, "note": "inventura" })).key("adj-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["level"]["on_hand"], 10);
    assert_eq!(first["applied"], true);
    let (status, replay) = c
        .as_clerk(Call::post(&adjust, json!({ "on_hand": 10, "note": "inventura" })).key("adj-1"))
        .await;
    assert_eq!((status, &replay), (StatusCode::CREATED, &first));
    let (status, err) = c
        .as_clerk(Call::post(&adjust, json!({ "delta": 3 })).key("adj-1"))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_conflict"))
    );
    let (status, second) = c
        .as_clerk(Call::post(&adjust, json!({ "delta": -4 })))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(second["level"]["available"], 6);
    let (status, err) = c
        .as_clerk(Call::post(&adjust, json!({ "delta": -7 })))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("below_reserved"))
    );
    let (status, err) = c
        .as_clerk(Call::post(&adjust, json!({ "delta": 1, "on_hand": 1 })))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_adjustment"))
    );
    // i32::MIN must not slip through an abs() overflow.
    let (status, err) = c
        .as_clerk(Call::post(&adjust, json!({ "delta": i32::MIN })))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_quantity"))
    );

    let (status, level) = c
        .as_clerk(Call::put(
            &format!("/admin/v1/inventory/{v}"),
            json!({ "track": true, "allow_backorder": true }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{level}");
    assert_eq!(level["allow_backorder"], true);

    let (status, page) = c
        .as_clerk(Call::get(&format!(
            "/admin/v1/inventory?product_id={}",
            product.id
        )))
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let row = items
        .iter()
        .find(|i| i["variant_id"] == v.to_string())
        .unwrap();
    assert_eq!(
        (row["on_hand"].as_i64(), row["sku"].as_str()),
        (Some(6), Some("INV-1"))
    );

    let (status, moves) = c
        .as_clerk(Call::get(&format!("/admin/v1/inventory/{v}/movements")))
        .await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<(&str, i64)> = moves["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["kind"].as_str().unwrap(), m["quantity"].as_i64().unwrap()))
        .collect();
    assert_eq!(kinds, [("adjust", -4), ("adjust", 10)]);

    let rival = sign(&claims("rival"));
    let (status, _, _) = Call::post(&adjust, json!({ "delta": 1 }))
        .token(&rival)
        .tenant(c.other_tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
