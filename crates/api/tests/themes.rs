//! Themes Admin API + builder callbacks + preview resolution through the router (WP23):
//! roles, fresh auth for publish (A9), builder token separation (A7), archive rejection with
//! reasons (A6), preview tokens (A21), tenant isolation.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::themes::archive::{self, Source};
use commerce::themes::{self, ArtifactKind};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;

mod common;
use common::*;

const DEFAULT: &str = "d0000000000000000000000000000000";
const BUILT: &str = "a0000000000000000000000000000001";

fn tokens(buy: &str) -> Value {
    json!({ "colors": { "buy": buy }, "fonts": { "sans": "system-ui" }, "radius": { "md": "0.5rem" } })
}

fn artifact_files(id: &str) -> Vec<(String, Vec<u8>)> {
    let manifest = json!({
        "schema": 1, "id": id, "kind": "theme",
        "runtime": { "compatibility_date": "2026-09-21", "compatibility_flags": [],
                     "main": "entry.mjs", "modules": ["entry.mjs"] },
        "assets": {}, "tokens": tokens("#123456"),
        "csp": { "script_hashes": [], "style_hashes": [] }
    });
    vec![
        (
            "manifest.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
        ("server/entry.mjs".into(), b"export default {}".to_vec()),
    ]
}

/// The artifact directory as the builder uploads it (`tar -cf - -C <dir> .`).
fn artifact_tar(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        b.append_data(&mut h, format!("./{path}"), bytes.as_slice())
            .unwrap();
    }
    b.into_inner().unwrap()
}

struct Ctx {
    s: api::AppState,
    shop: Shop,
    other: Shop,
    owner: String,
    clerk: String,
    stranger: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other.tenant, "stranger", "owner").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    // The default theme + its source, as `make theme-build` publishes them.
    themes::register_artifact(
        &runtime,
        &s.storage,
        DEFAULT,
        ArtifactKind::Theme,
        None,
        artifact_files(DEFAULT),
    )
    .await
    .unwrap();
    let mut src = Source::default();
    src.files.insert(
        "theme.tokens.json".into(),
        archive::tokens_file(&tokens("#ff8800")),
    );
    src.files
        .insert("src/pages/index.astro".into(), b"<h1>x</h1>".to_vec());
    s.storage
        .private
        .put(
            &Path::from("theme-sources/default/x.tar.gz"),
            PutPayload::from(archive::write(&src)),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE platform.theme_artifacts SET source_key = 'theme-sources/default/x.tar.gz'",
    )
    .execute(&runtime)
    .await
    .unwrap();
    themes::publish_default(&runtime, "platform", DEFAULT)
        .await
        .unwrap();
    Ctx {
        s,
        owner: sign(&claims("boss")),
        clerk: sign(&claims("clerk")),
        stranger: sign(&claims("stranger")),
        shop,
        other,
        _jwks: jwks,
    }
}

impl Ctx {
    fn builder<'a>(&self, call: Call<'a>) -> Call<'a> {
        call.token(BUILDER_TOKEN).tenant(self.shop.tenant)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn fork_build_preview_publish(db: PgPool) {
    let c = setup(db).await;
    let t = c.shop.tenant;

    // Staff may look but not create; owners create.
    let (st, _, _) = Call::post("/admin/v1/themes/revisions/fork", json!({}))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, rev, _) = Call::post("/admin/v1/themes/revisions/fork", json!({}))
        .token(&c.owner)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{rev}");
    assert_eq!(rev["status"], "draft");
    let id = rev["id"].as_str().unwrap().to_owned();

    // Builder: spec, source, building, artifact → checking + preview token, screenshots, ready.
    let uri = |tail: &str| format!("/internal/v1/themes/revisions/{id}/{tail}");
    let (st, spec, _) = c.builder(Call::get(&uri("build"))).send(&c.s).await;
    assert_eq!(st, StatusCode::OK, "{spec}");
    assert_eq!(
        (
            spec["tokens_only"].as_bool(),
            spec["astro_key_hex"].as_str().map(str::len)
        ),
        (Some(false), Some(64))
    );
    // The edge's token is not the builder's (A7); another tenant header finds nothing.
    let (st, _, _) = Call::get(&uri("build"))
        .token(SERVICE_TOKEN)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _, _) = Call::get(&uri("build"))
        .token(BUILDER_TOKEN)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, gz, res) = c.builder(Call::get(&uri("source"))).send_text(&c.s).await;
    assert_eq!(
        (st, res.headers()["content-type"].to_str().unwrap()),
        (StatusCode::OK, "application/gzip")
    );
    assert!(!gz.is_empty());
    let (st, body, _) = c
        .builder(Call::post(
            &uri("status"),
            json!({ "status": "building", "checks": {} }),
        ))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let bad = {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "client/_astro/x.js", "/proc/self/environ")
            .unwrap();
        b.into_inner().unwrap()
    };
    let artifact_uri = uri("artifact");
    let (st, body, _) = c
        .builder(Call::put_raw(&artifact_uri, bad, "application/x-tar"))
        .send(&c.s)
        .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_artifact"))
    );
    let shot = |n: &str| uri(&format!("screenshots/{n}"));
    let (st, check, _) = c
        .builder(Call::put_raw(
            &artifact_uri,
            artifact_tar(&artifact_files(BUILT)),
            "application/x-tar",
        ))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK, "{check}");
    assert_eq!(check["preview_host"], "preview-2--shop.localhost");
    let png = b"\x89PNG\r\n\x1a\nx".to_vec();
    let (home, evil, desktop) = (shot("home-mobile"), shot("..%2Fevil"), shot("home-desktop"));
    let (st, _, _) = c
        .builder(Call::put_raw(&home, png.clone(), "image/png"))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _, _) = c
        .builder(Call::put_raw(&evil, png, "image/png"))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = c
        .builder(Call::put_raw(&desktop, b"GIF89a".to_vec(), "image/png"))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let report =
        json!({ "status": "ready", "checks": { "failures": [], "screenshots": ["home-mobile"] } });
    let (st, _, _) = c
        .builder(Call::post(&uri("status"), report))
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK);

    // Preview: link for staff, resolved by the edge (service token), not by the builder token.
    let detail_uri = format!("/admin/v1/themes/revisions/{id}");
    let (st, detail, _) = Call::get(&detail_uri)
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["screenshots"][0]["name"], "home-mobile");
    assert_eq!(detail["tokens"]["colors"]["buy"], "#123456");
    let preview_uri = format!("/admin/v1/themes/revisions/{id}/preview");
    let (st, link, _) = Call::post(&preview_uri, json!({}))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK, "{link}");
    let url = link["url"].as_str().unwrap();
    assert!(
        url.starts_with("http://preview-2--shop.localhost:8080/?preview_token="),
        "{url}"
    );
    let token = url.split("preview_token=").nth(1).unwrap();
    let resolve =
        format!("/internal/v1/previews/resolve?host=preview-2--shop.localhost:8080&token={token}");
    let (st, site, _) = Call::get(&resolve).token(SERVICE_TOKEN).send(&c.s).await;
    assert_eq!(st, StatusCode::OK, "{site}");
    assert_eq!(site["site"]["theme_artifact"], BUILT);
    let (st, _, _) = Call::get(&resolve).token(BUILDER_TOKEN).send(&c.s).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let foreign =
        format!("/internal/v1/previews/resolve?host=preview-2--other.localhost&token={token}");
    let (st, _, _) = Call::get(&foreign).token(SERVICE_TOKEN).send(&c.s).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Other tenants see nothing.
    let (st, _, _) = Call::get(&detail_uri)
        .token(&c.stranger)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Publish: Admin with a fresh login (A9).
    let publish_uri = format!("/admin/v1/themes/revisions/{id}/publish");
    let mut stale = claims("boss");
    stale["auth_time"] = json!(now() - 3600);
    let stale = sign(&stale);
    let (st, body, _) = Call::post(&publish_uri, json!({}))
        .token(&stale)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (st, _, _) = Call::post(&publish_uri, json!({}))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, body, _) = Call::post(&publish_uri, json!({}))
        .token(&c.owner)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(
        (st, body["status"].as_str(), body["active"].as_bool()),
        (StatusCode::OK, Some("published"), Some(true))
    );
    let (st, list, _) = Call::get("/admin/v1/themes/revisions")
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK);
    let statuses: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["published", "superseded"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn uploads_and_token_edits_are_validated(db: PgPool) {
    let c = setup(db).await;
    let t = c.shop.tenant;
    let evil = {
        // A symlink next to valid files: refused with the reason.
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        b.append_link(&mut h, "src/pages/index.astro", "/etc/passwd")
            .unwrap();
        let tokens = archive::tokens_file(&tokens("#000000"));
        let mut h = tar::Header::new_gnu();
        h.set_size(tokens.len() as u64);
        h.set_mode(0o644);
        b.append_data(&mut h, "theme.tokens.json", tokens.as_slice())
            .unwrap();
        b.into_inner().unwrap().finish().unwrap()
    };
    let (st, body, _) = Call::post_raw(
        "/admin/v1/themes/revisions/upload",
        evil,
        "application/gzip",
    )
    .token(&c.owner)
    .tenant(t)
    .send(&c.s)
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_archive");
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("symbolic links are not allowed"),
        "{body}"
    );

    let (st, body, _) = Call::post(
        "/admin/v1/themes/revisions/tokens",
        json!({ "tokens": tokens("#00ff00") }),
    )
    .token(&c.owner)
    .tenant(t)
    .send(&c.s)
    .await;
    assert_eq!(
        (st, body["change"].as_str()),
        (StatusCode::CREATED, Some("tokens"))
    );
    let (st, body, _) = Call::post(
        "/admin/v1/themes/revisions/tokens",
        json!({ "tokens": { "colors": {"x": "expression(1)"}, "fonts": {}, "radius": {} } }),
    )
    .token(&c.owner)
    .tenant(t)
    .send(&c.s)
    .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_tokens"))
    );
    let (st, body, _) = Call::post(
        "/admin/v1/themes/revisions/tokens",
        json!({ "tokens": tokens("#00ff00"), "css": "x" }),
    )
    .token(&c.owner)
    .tenant(t)
    .send(&c.s)
    .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn ai_runs_are_admin_only_and_tenant_scoped(db: PgPool) {
    let c = setup(db).await;
    let t = c.shop.tenant;
    fn start(token: &str, t: uuid::Uuid) -> Call<'_> {
        Call::post(
            "/admin/v1/themes/ai-runs",
            json!({ "prompt": "Add a free-shipping bar" }),
        )
        .token(token)
        .tenant(t)
    }
    let (st, _, _) = start(&c.clerk, t).send(&c.s).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, run, _) = start(&c.owner, t).send(&c.s).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["status"], "queued");
    let id = run["id"].as_str().unwrap().to_owned();
    let (st, body, _) = start(&c.owner, t).send(&c.s).await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("ai_run_in_progress"))
    );
    let (st, body, _) = Call::post(
        "/admin/v1/themes/ai-runs",
        json!({ "prompt": "x", "model": "claude-opus-5-5" }),
    )
    .token(&c.owner)
    .tenant(t)
    .send(&c.s)
    .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body"))
    );

    // Staff may follow the run; the list names the provider.
    let (st, list, _) = Call::get("/admin/v1/themes/ai-runs")
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["provider"], "fake");
    assert_eq!(list["items"][0]["id"], id.as_str());
    let (st, detail, _) = Call::get(&format!("/admin/v1/themes/ai-runs/{id}"))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["limits"]["max_turns"], 25);

    // Another tenant sees nothing, even with its own owner.
    let (st, _, _) = Call::get(&format!("/admin/v1/themes/ai-runs/{id}"))
        .token(&c.stranger)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = Call::post(&format!("/admin/v1/themes/ai-runs/{id}/cancel"), json!({}))
        .token(&c.stranger)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Only succeeded runs are accepted; staff cannot cancel; owners can.
    let (st, body, _) = Call::post(&format!("/admin/v1/themes/ai-runs/{id}/accept"), json!({}))
        .token(&c.owner)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(
        (st, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_transition"))
    );
    let (st, _, _) = Call::post(&format!("/admin/v1/themes/ai-runs/{id}/cancel"), json!({}))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, body, _) = Call::post(&format!("/admin/v1/themes/ai-runs/{id}/cancel"), json!({}))
        .token(&c.owner)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!(
        (st, body["status"].as_str()),
        (StatusCode::OK, Some("cancelled"))
    );

    // The revision diff view: the default revision has no parent (empty diff).
    let base = run["base_revision_id"].as_str().unwrap();
    let (st, diff, _) = Call::get(&format!("/admin/v1/themes/revisions/{base}/diff"))
        .token(&c.clerk)
        .tenant(t)
        .send(&c.s)
        .await;
    assert_eq!((st, diff["diff"].as_str()), (StatusCode::OK, Some("")));
    let (st, _, _) = Call::get(&format!("/admin/v1/themes/revisions/{base}/diff"))
        .token(&c.stranger)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
