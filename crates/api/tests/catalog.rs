//! Catalog and media Admin API through the router (real Postgres as the runtime role,
//! in-memory object storage, locally signed staff JWTs).
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use image::ImageEncoder;
use object_store::{ObjectStoreExt, PutPayload, path::Path};
use serde_json::{Value, json};
use sqlx::PgPool;

mod common;
use common::*;

fn category(name: &str, slug: &str, parent: Option<&str>) -> Value {
    json!({
        "parent_id": parent,
        "translations": [{ "locale": "cs", "name": name, "slug": slug }]
    })
}

fn option(code: &str, values: &[&str]) -> Value {
    json!({
        "code": code,
        "name_i18n": { "cs": code, "en": code },
        "values": values.iter().map(|v| json!({ "code": v, "name_i18n": { "cs": v } })).collect::<Vec<_>>()
    })
}

fn product(category_id: &str) -> Value {
    let variants: Vec<Value> = [("red", "s"), ("red", "m"), ("blue", "s"), ("blue", "m")]
        .iter()
        .map(|(c, s)| {
            json!({
                "sku": format!("TS-{c}-{s}").to_uppercase(),
                "option_values": { "color": c, "size": s },
                "weight_g": 180
            })
        })
        .collect();
    json!({
        "status": "active",
        "brand": "Basic",
        "gpsr": {
            "manufacturer": { "name": "Výrobce s.r.o.", "address": "Praha", "email": "info@vyrobce.cz",
                              "url": null, "phone": null },
            "warnings": { "cs": "Nevhodné pro děti do 3 let." }
        },
        "unit_measure": "pcs",
        "unit_quantity": 1.0,
        "translations": [
            { "locale": "cs", "name": "Tričko Basic", "slug": "tricko-basic", "description_html": "<p>Bavlna<script>x()</script></p>" },
            { "locale": "sk", "name": "Tričko Basic", "slug": "tricko-basic" },
            { "locale": "en", "name": "Basic T-shirt", "slug": "basic-t-shirt" }
        ],
        "options": [option("color", &["red", "blue"]), option("size", &["s", "m"])],
        "variants": variants,
        "category_ids": [category_id],
        "tax_categories": { "SK": "reduced" }
    })
}

struct Ctx {
    s: api::AppState,
    token: String,
    tenant: uuid::Uuid,
    other_tenant: uuid::Uuid,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let (other_tenant, _) = testkit::tenant(&runtime, "other").await;
    // The lowest role may edit the catalog.
    testkit::staff(&runtime, tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other_tenant, "rival", "owner").await;
    Ctx {
        s: state(runtime, &jwks, Duration::from_secs(30)),
        token: sign(&claims("clerk")),
        tenant,
        other_tenant,
        _jwks: jwks,
    }
}

impl Ctx {
    async fn call(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (status, body, _) = call
            .token(&self.token)
            .tenant(self.tenant)
            .send(&self.s)
            .await;
        (status, body)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn staff_manage_categories_and_products(db: PgPool) {
    let c = setup(db).await;

    let (status, root) = c
        .call(
            Call::post(
                "/admin/v1/categories",
                category("Oblečení", "obleceni", None),
            )
            .key("cat-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{root}");
    let root_id = root["id"].as_str().unwrap().to_owned();
    let (status, replayed) = c
        .call(
            Call::post(
                "/admin/v1/categories",
                category("Oblečení", "obleceni", None),
            )
            .key("cat-1"),
        )
        .await;
    assert_eq!(
        (status, &replayed["id"]),
        (StatusCode::CREATED, &root["id"])
    );
    let (_, tees) = c
        .call(Call::post(
            "/admin/v1/categories",
            category("Trička", "tricka", Some(&root_id)),
        ))
        .await;
    let tees_id = tees["id"].as_str().unwrap().to_owned();
    let (_, mugs) = c
        .call(Call::post(
            "/admin/v1/categories",
            category("Hrnky", "hrnky", None),
        ))
        .await;

    let (status, tree) = c.call(Call::get("/admin/v1/categories")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tree["items"].as_array().unwrap().len(), 2);
    assert_eq!(tree["items"][0]["children"][0]["id"], tees["id"]);
    assert_eq!(tree["items"][0]["translations"][0]["slug"], "obleceni");

    let uri = format!("/admin/v1/categories/{}/move", mugs["id"].as_str().unwrap());
    let (status, moved) = c
        .call(Call::post(
            &uri,
            json!({ "parent_id": root_id, "position": 0 }),
        ))
        .await;
    assert_eq!((status, &moved["position"]), (StatusCode::OK, &json!(0)));
    let uri = format!("/admin/v1/categories/{root_id}/move");
    let (status, err) = c
        .call(Call::post(
            &uri,
            json!({ "parent_id": tees_id, "position": 0 }),
        ))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("category_cycle"))
    );

    // Product with 2 options / 4 variants / 3 locales.
    let (status, p) = c
        .call(Call::post("/admin/v1/products", product(&tees_id)).key("p-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    let id = p["id"].as_str().unwrap().to_owned();
    assert_eq!(p["variants"].as_array().unwrap().len(), 4);
    assert_eq!(p["variants"][0]["is_default"], true);
    assert_eq!(p["translations"].as_array().unwrap().len(), 3);
    assert!(
        !p["translations"][0]["description_html"]
            .as_str()
            .unwrap()
            .contains("script")
    );
    assert_eq!(p["gpsr"]["manufacturer"]["email"], "info@vyrobce.cz");
    let (status, err) = c
        .call(Call::post("/admin/v1/products", product(&tees_id)).key("p-1"))
        .await;
    assert_eq!(
        (status, &err["id"]),
        (StatusCode::CREATED, &p["id"]),
        "replayed"
    );
    let (status, err) = c
        .call(Call::post("/admin/v1/products", product(&tees_id)))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("slug_taken"))
    );
    let mut same_skus = product(&tees_id);
    for t in same_skus["translations"].as_array_mut().unwrap() {
        t["slug"] = json!("other-slug");
    }
    let (status, err) = c.call(Call::post("/admin/v1/products", same_skus)).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("sku_taken"))
    );

    let (status, got) = c.call(Call::get(&format!("/admin/v1/products/{id}"))).await;
    assert_eq!((status, &got), (StatusCode::OK, &p));

    for (query, want) in [
        ("?q=ts-blue", 1),
        ("?q=t-shirt", 1),
        ("?q=nothing", 0),
        ("?status=draft", 0),
        (&format!("?category_id={tees_id}") as &str, 1),
        (&format!("?category_id={root_id}") as &str, 0),
    ] {
        let (status, page) = c
            .call(Call::get(&format!("/admin/v1/products{query}")))
            .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(page["items"].as_array().unwrap().len(), want, "{query}");
    }
    let (status, err) = c.call(Call::get("/admin/v1/products?limit=abc")).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
    let (status, err) = c.call(Call::get("/admin/v1/products?limit=500")).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_limit"))
    );

    // Replace: drop a variant, keep ids of the others, archive.
    let mut doc = product(&tees_id);
    doc["status"] = json!("archived");
    let kept: Vec<Value> = p["variants"].as_array().unwrap()[..3]
        .iter()
        .map(|v| json!({ "id": v["id"], "sku": v["sku"], "option_values": v["option_values"] }))
        .collect();
    doc["variants"] = json!(kept);
    let (status, replaced) = c
        .call(Call::put(&format!("/admin/v1/products/{id}"), doc))
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["status"], "archived");
    assert_eq!(replaced["variants"].as_array().unwrap().len(), 3);
    assert_eq!(replaced["variants"][2]["id"], p["variants"][2]["id"]);

    let mut bad = product(&tees_id);
    bad["tenant_id"] = json!("x");
    let (status, err) = c
        .call(Call::put(&format!("/admin/v1/products/{id}"), bad))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body"))
    );
    let (status, _) = c.call(Call::get("/admin/v1/products/not-a-uuid")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Tenant B's owner cannot see or touch tenant A's product, even by id.
    let rival = sign(&claims("rival"));
    for call in [
        Call::get(&format!("/admin/v1/products/{id}") as &str),
        Call::delete(&format!("/admin/v1/products/{id}") as &str),
        Call::put(
            &format!("/admin/v1/products/{id}") as &str,
            product(&tees_id),
        ),
    ] {
        let (status, _, _) = call.token(&rival).tenant(c.other_tenant).send(&c.s).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (_, page, _) = Call::get("/admin/v1/products")
        .token(&rival)
        .tenant(c.other_tenant)
        .send(&c.s)
        .await;
    assert_eq!(page["items"], json!([]));
    // Tenant A's category id is unknown in tenant B.
    let (status, err, _) = Call::post("/admin/v1/products", product(&tees_id))
        .token(&rival)
        .tenant(c.other_tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_reference"))
    );
    // And the rival cannot act in tenant A at all.
    let (status, _, _) = Call::get("/admin/v1/products")
        .token(&rival)
        .tenant(c.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = c
        .call(Call::delete(&format!("/admin/v1/products/{id}")))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = c.call(Call::get(&format!("/admin/v1/products/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Staff cannot read the audit log, but every mutation is in it.
    let mut tx = platform::db::tenant_tx(&c.s.db, c.tenant).await.unwrap();
    let actions: Vec<String> = commerce::audit::list(&mut tx, None, 100)
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|e| e.actor == "clerk")
        .map(|e| e.action)
        .collect();
    for a in [
        "category.created",
        "category.moved",
        "product.created",
        "product.updated",
        "product.deleted",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn parameters_and_tax_categories(db: PgPool) {
    let c = setup(db).await;
    let input = json!({ "key": "material", "name_i18n": { "cs": "Materiál" }, "kind": "text", "filterable": true });
    let (status, p) = c
        .call(Call::post("/admin/v1/parameters", input.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{p}");
    let (status, err) = c.call(Call::post("/admin/v1/parameters", input)).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("key_taken"))
    );
    let uri = format!("/admin/v1/parameters/{}", p["id"].as_str().unwrap());
    let (status, updated) = c
        .call(Call::put(&uri, json!({ "key": "material", "name_i18n": { "cs": "Materiál", "en": "Material" }, "kind": "text" })))
        .await;
    assert_eq!(
        (status, &updated["filterable"]),
        (StatusCode::OK, &json!(false))
    );
    let (_, list) = c.call(Call::get("/admin/v1/parameters")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, _) = c.call(Call::delete(&uri)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, tax) = c
        .call(Call::get(
            "/admin/v1/tax-categories?country=SK&at=2026-09-25",
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let rates: Vec<(String, String)> = tax["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["code"].as_str().unwrap().into(),
                t["rate"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        rates,
        vec![
            ("reduced".into(), "19".into()),
            ("second_reduced".into(), "5".into()),
            ("standard".into(), "23".into())
        ]
    );
    let (status, _) = c
        .call(Call::get("/admin/v1/tax-categories?country=sk"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../../migrations")]
async fn presigned_upload_flow(db: PgPool) {
    let c = setup(db).await;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &[90u8; 3 * 200 * 100],
            200,
            100,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

    let body = json!({ "filename": "a.png", "content_type": "image/png", "size": png.len() });
    let (status, up) = c
        .call(Call::post("/admin/v1/assets/uploads", body).key("up-1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{up}");
    assert_eq!(up["asset"]["status"], "pending");
    assert_eq!(up["upload"]["method"], "PUT");
    assert!(
        up["upload"]["url"]
            .as_str()
            .unwrap()
            .starts_with("http://s3.test/private/uploads/")
    );
    let id = up["asset"]["id"].as_str().unwrap().to_owned();

    let (status, err) = c
        .call(Call::post(
            "/admin/v1/assets/uploads",
            json!({ "content_type": "image/svg+xml", "size": 10 }),
        ))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unsupported_type"))
    );

    let complete = format!("/admin/v1/assets/{id}/complete");
    let (status, err) = c.call(Call::post(&complete, json!(null))).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("upload_missing"))
    );

    // What the client's PUT would store.
    let key = Path::from(format!("uploads/{}/{id}", c.tenant));
    c.s.storage
        .private
        .put(&key, PutPayload::from(png))
        .await
        .unwrap();
    let (status, asset) = c.call(Call::post(&complete, json!(null))).await;
    assert_eq!(status, StatusCode::OK, "{asset}");
    assert_eq!(
        (asset["status"].as_str(), asset["width"].as_i64()),
        (Some("processing"), Some(200))
    );

    let asset_id = uuid::Uuid::parse_str(&id).unwrap();
    commerce::media::process(&c.s.db, &c.s.storage, c.tenant, asset_id)
        .await
        .unwrap();
    let (status, ready) = c.call(Call::get(&format!("/admin/v1/assets/{id}"))).await;
    assert_eq!(
        (status, ready["status"].as_str()),
        (StatusCode::OK, Some("ready"))
    );
    assert_eq!(ready["variants"].as_array().unwrap().len(), 6);
    assert!(
        ready["variants"][0]["url"]
            .as_str()
            .unwrap()
            .starts_with("http://media.test/media/")
    );

    let (_, page) = c.call(Call::get("/admin/v1/assets?status=ready")).await;
    assert_eq!(page["items"][0]["id"].as_str(), Some(id.as_str()));

    let (status, _, _) = Call::get(&format!("/admin/v1/assets/{id}"))
        .token(&sign(&claims("rival")))
        .tenant(c.other_tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = c
        .call(Call::delete(&format!("/admin/v1/assets/{id}")))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
