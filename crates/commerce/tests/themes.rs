//! WP23 theme revisions against a real Postgres as the runtime role: fork → builder callbacks
//! → preview → publish → rollback, token edits, uploads, tenant isolation, maintenance.
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use commerce::tenancy;
use commerce::themes::archive::{self, Source};
use commerce::themes::{self, StatusUpdate, ThemeKeys, TokensInput};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::storage::Storage;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const DEFAULT: &str = "d0000000000000000000000000000000";

fn keys() -> ThemeKeys {
    ThemeKeys::new(&[42u8; 32])
}

fn tokens(buy: &str) -> Value {
    json!({ "colors": { "buy": buy }, "fonts": { "sans": "system-ui" }, "radius": { "md": "0.5rem" } })
}

fn default_source() -> Source {
    let mut s = Source::default();
    s.files.insert(
        "theme.tokens.json".into(),
        archive::tokens_file(&tokens("#ff8800")),
    );
    s.files.insert(
        "src/pages/index.astro".into(),
        b"<h1>Default</h1>\n".to_vec(),
    );
    s.files.insert("package.json".into(), b"{}\n".to_vec());
    s
}

/// A minimal artifact that satisfies `check_artifact` (the edge verifies the real content
/// address; these ids are arbitrary).
fn artifact(id: &str, buy: &str) -> Vec<(String, Vec<u8>)> {
    let manifest = json!({
        "schema": 1, "id": id, "kind": "theme",
        "runtime": { "compatibility_date": "2026-09-21", "compatibility_flags": [],
                     "main": "entry.mjs", "modules": ["entry.mjs"] },
        "assets": { "/_astro/app.js": { "sha256": "00", "size": 2 } },
        "tokens": tokens(buy),
        "csp": { "script_hashes": [], "style_hashes": [] }
    });
    vec![
        (
            "manifest.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
        ("server/entry.mjs".into(), b"export default {}".to_vec()),
        ("client/_astro/app.js".into(), b"1;".to_vec()),
    ]
}

/// The default artifact + its source, published for every tenant (what `make theme-build` does).
async fn publish_default(runtime: &PgPool, storage: &Storage) {
    themes::register_artifact(
        runtime,
        storage,
        DEFAULT,
        themes::ArtifactKind::Theme,
        None,
        artifact(DEFAULT, "#ff8800"),
    )
    .await
    .unwrap();
    let key = "theme-sources/default/abc.tar.gz";
    storage
        .private
        .put(
            &Path::from(key),
            PutPayload::from(archive::write(&default_source())),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE platform.theme_artifacts SET source_key = $2 WHERE id = $1")
        .bind(DEFAULT)
        .bind(key)
        .execute(runtime)
        .await
        .unwrap();
    themes::publish_default(runtime, "platform", DEFAULT)
        .await
        .unwrap();
}

/// What the builder does for a revision: building → artifact → checking → ready.
async fn build(
    runtime: &PgPool,
    storage: &Storage,
    shop: &Shop,
    id: Uuid,
    artifact_id: &str,
    buy: &str,
) -> themes::CheckSpec {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let spec = themes::build_spec(&mut tx, &keys(), id).await.unwrap();
    assert_eq!(spec.astro_key_hex.len(), 64);
    themes::builder_status(
        &mut tx,
        id,
        &StatusUpdate {
            status: "building".into(),
            checks: json!({}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let check = themes::attach_artifact(
        runtime,
        storage,
        &keys(),
        shop.tenant,
        id,
        artifact_id,
        artifact(artifact_id, buy),
        Utc::now(),
    )
    .await
    .unwrap();
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    for name in ["home-mobile", "product-desktop"] {
        themes::store_screenshot(&mut tx, storage, id, name, b"\x89PNG\r\n\x1a\nfake")
            .await
            .unwrap();
    }
    themes::builder_status(
        &mut tx,
        id,
        &StatusUpdate { status: "ready".into(), checks: json!({ "failures": [], "screenshots": ["home-mobile", "product-desktop", "../x"] }) },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    check
}

async fn active_artifact(runtime: &PgPool, host: &str) -> Option<String> {
    tenancy::resolve_host(runtime, host)
        .await
        .unwrap()
        .unwrap()
        .theme_artifact
}

#[sqlx::test(migrations = "../../migrations")]
async fn fork_build_preview_publish_rollback(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let storage = testkit::memory_storage();
    let a = testkit::storefront::shop(&runtime, "alpha").await;
    let b = testkit::storefront::shop(&runtime, "beta").await;
    publish_default(&runtime, &storage).await;
    assert_eq!(
        active_artifact(&runtime, "alpha.localhost")
            .await
            .as_deref(),
        Some(DEFAULT)
    );

    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let fork = themes::fork(&mut tx, &storage, "owner").await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (fork.number, fork.status.as_str(), fork.change.as_str()),
        (2, "draft", "fork")
    );
    assert!(fork.has_source && fork.parent_id.is_some());

    // The builder: preview host + pages come from the tenant's own catalog.
    let built = "a0000000000000000000000000000001";
    let check = build(&runtime, &storage, &a, fork.id, built, "#ff8800").await;
    assert_eq!(check.preview_host, "preview-2--alpha.localhost");
    assert_eq!(
        check.pages,
        vec!["/".to_owned(), "/c/trika".into(), format!("/p/{}", a.slug)]
    );

    // Preview: token-bound to tenant + revision (+ number in the host).
    let now = Utc::now();
    let site = themes::resolve_preview(
        &runtime,
        &keys(),
        &check.preview_host,
        &check.preview_token,
        now,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(site.site.theme_artifact.as_deref(), Some(built));
    assert_eq!(site.site.tenant_id, a.tenant);
    assert!(site.site.retained_artifacts.is_empty());
    for (host, token) in [
        ("preview-1--alpha.localhost", check.preview_token.as_str()),
        ("preview-2--beta.localhost", check.preview_token.as_str()),
        ("preview-2--alpha.localhost", "0.0.0"),
        ("alpha.localhost", check.preview_token.as_str()),
    ] {
        assert!(
            themes::resolve_preview(&runtime, &keys(), host, token, now)
                .await
                .unwrap()
                .is_none(),
            "{host}"
        );
    }
    // Expired.
    let later = now + chrono::Duration::seconds(themes::PREVIEW_TTL_SECS + 1);
    assert!(
        themes::resolve_preview(
            &runtime,
            &keys(),
            &check.preview_host,
            &check.preview_token,
            later
        )
        .await
        .unwrap()
        .is_none()
    );

    // Detail: failures, tokens, only allowlisted screenshots (presigned).
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let detail = themes::detail(&mut tx, &storage, fork.id).await.unwrap();
    assert_eq!(detail.revision.status, "ready");
    assert_eq!(detail.tokens.unwrap()["colors"]["buy"], "#ff8800");
    assert_eq!(
        detail
            .screenshots
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["home-mobile", "product-desktop"]
    );
    assert!(detail.screenshots[0].url.contains("theme-shots"));
    let link = themes::preview_link(
        &mut tx,
        &keys(),
        &commerce::storefront::PublicUrls {
            scheme: "http".into(),
            port: Some(8080),
        },
        fork.id,
        now,
    )
    .await
    .unwrap();
    assert!(
        link.url
            .starts_with("http://preview-2--alpha.localhost:8080/?preview_token="),
        "{}",
        link.url
    );

    // Publish: atomic pointer switch; the default revision is superseded.
    let published = themes::publish(&mut tx, "owner", fork.id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (published.status.as_str(), published.active),
        ("published", true)
    );
    assert_eq!(
        active_artifact(&runtime, "alpha.localhost")
            .await
            .as_deref(),
        Some(built)
    );
    assert_eq!(
        active_artifact(&runtime, "beta.localhost").await.as_deref(),
        Some(DEFAULT)
    );

    // A custom tenant no longer follows new defaults (A30); beta does.
    let next_default = "d0000000000000000000000000000002";
    themes::register_artifact(
        &runtime,
        &storage,
        next_default,
        themes::ArtifactKind::Theme,
        None,
        artifact(next_default, "#000000"),
    )
    .await
    .unwrap();
    let changed = themes::publish_default(&runtime, "platform", next_default)
        .await
        .unwrap();
    assert_eq!(changed, vec![b.tenant]);
    assert_eq!(
        active_artifact(&runtime, "alpha.localhost")
            .await
            .as_deref(),
        Some(built)
    );

    // Rollback to revision #1 (superseded default): alpha follows the default again.
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let list = themes::list(&mut tx).await.unwrap();
    let first = list.iter().find(|r| r.number == 1).unwrap();
    assert_eq!(first.status, "superseded");
    let rolled = themes::publish(&mut tx, "owner", first.id).await.unwrap();
    assert_eq!(rolled.status, "published");
    let list = themes::list(&mut tx).await.unwrap();
    assert_eq!(
        list.iter().find(|r| r.id == fork.id).unwrap().status,
        "superseded"
    );
    assert_eq!(list.iter().filter(|r| r.active).count(), 1);
    tx.commit().await.unwrap();
    assert_eq!(
        active_artifact(&runtime, "alpha.localhost")
            .await
            .as_deref(),
        Some(DEFAULT)
    );
    // The rolled-back custom artifact stays retained for old pages (A22).
    let resolved = tenancy::resolve_host(&runtime, "alpha.localhost")
        .await
        .unwrap()
        .unwrap();
    assert!(resolved.retained_artifacts.contains(&built.to_owned()));

    // Tenant isolation: beta can neither see nor publish alpha's revision.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    assert_eq!(
        themes::publish(&mut tx, "x", fork.id)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert!(
        themes::list(&mut tx)
            .await
            .unwrap()
            .iter()
            .all(|r| r.id != fork.id)
    );
    assert_eq!(
        themes::detail(&mut tx, &storage, fork.id)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn token_edits_uploads_and_limits(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let storage = testkit::memory_storage();
    let a = testkit::storefront::shop(&runtime, "alpha").await;

    // No default source yet: a clear conflict.
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    assert_eq!(
        themes::fork(&mut tx, &storage, "o")
            .await
            .unwrap_err()
            .code(),
        "default_source_missing"
    );
    tx.rollback().await.unwrap();
    publish_default(&runtime, &storage).await;

    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let edit = themes::edit_tokens(
        &mut tx,
        &storage,
        "o",
        &TokensInput {
            base_revision_id: None,
            tokens: tokens("#00aa00"),
        },
    )
    .await
    .unwrap();
    assert_eq!(edit.change, "tokens");
    let spec = themes::build_spec(&mut tx, &keys(), edit.id).await.unwrap();
    assert!(spec.tokens_only);
    // Only theme.tokens.json differs from the default source.
    let src = archive::read(
        &themes::source_archive(&mut tx, &storage, edit.id)
            .await
            .unwrap(),
    )
    .unwrap();
    let mut expected = default_source();
    expected.files.insert(
        "theme.tokens.json".into(),
        archive::tokens_file(&tokens("#00aa00")),
    );
    assert_eq!(src, expected);
    let bad = themes::edit_tokens(
        &mut tx,
        &storage,
        "o",
        &TokensInput {
            base_revision_id: None,
            tokens: json!({"colors": {"buy": "url(//evil)"}, "fonts": {}, "radius": {}}),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(bad.code(), "invalid_tokens");

    // Uploads: validated at the boundary with reasons.
    let mut evil = default_source();
    evil.files.insert("node_modules/x.js".into(), b"x".to_vec());
    let err = themes::upload(&mut tx, &storage, "o", &archive::write(&evil))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_archive");
    assert!(err.to_string().contains("node_modules/x.js"), "{err}");
    let up = themes::upload(&mut tx, &storage, "o", &archive::write(&default_source()))
        .await
        .unwrap();
    assert_eq!(up.change, "upload");

    // At most 3 builds in flight per tenant.
    let _third = themes::fork(&mut tx, &storage, "o").await.unwrap();
    assert_eq!(
        themes::fork(&mut tx, &storage, "o")
            .await
            .unwrap_err()
            .code(),
        "builds_in_progress"
    );

    // Nothing unbuilt can be published or previewed; the status machine is enforced.
    assert_eq!(
        themes::publish(&mut tx, "o", up.id)
            .await
            .unwrap_err()
            .code(),
        "not_publishable"
    );
    let urls = commerce::storefront::PublicUrls::default();
    assert_eq!(
        themes::preview_link(&mut tx, &keys(), &urls, up.id, Utc::now())
            .await
            .unwrap_err()
            .code(),
        "not_built"
    );
    let ready = StatusUpdate {
        status: "ready".into(),
        checks: json!({}),
    };
    assert_eq!(
        themes::builder_status(&mut tx, up.id, &ready)
            .await
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    let failed = StatusUpdate {
        status: "failed".into(),
        checks: json!({ "failures": ["lint: foreign fetch"] }),
    };
    let f = themes::builder_status(&mut tx, up.id, &failed)
        .await
        .unwrap();
    assert_eq!(f.failures, vec!["lint: foreign fetch".to_owned()]);
    assert_eq!(
        themes::publish(&mut tx, "o", up.id)
            .await
            .unwrap_err()
            .code(),
        "not_publishable"
    );
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn maintenance_expires_builds_and_collects_artifacts(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let storage = testkit::memory_storage();
    let a = testkit::storefront::shop(&runtime, "alpha").await;
    publish_default(&runtime, &storage).await;

    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let kept = themes::fork(&mut tx, &storage, "o").await.unwrap();
    let dropped = themes::fork(&mut tx, &storage, "o").await.unwrap();
    let stuck = themes::fork(&mut tx, &storage, "o").await.unwrap();
    tx.commit().await.unwrap();
    let (keep_id, drop_id) = (
        "a0000000000000000000000000000001",
        "a0000000000000000000000000000002",
    );
    build(&runtime, &storage, &a, kept.id, keep_id, "#111111").await;
    build(&runtime, &storage, &a, dropped.id, drop_id, "#222222").await;
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    themes::publish(&mut tx, "o", kept.id).await.unwrap();
    tx.commit().await.unwrap();
    // `dropped` failed long ago; `stuck` has not moved for an hour; everything is old.
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    sqlx::query("UPDATE theme_revisions SET status = 'failed', status_changed_at = now() - interval '30 days' WHERE id = $1")
        .bind(dropped.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE theme_revisions SET status_changed_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(stuck.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE platform.theme_artifacts SET created_at = now() - interval '30 days'")
        .execute(&db)
        .await
        .unwrap();

    let report = themes::maintenance(&runtime, &storage).await.unwrap();
    assert_eq!(report.expired_builds, 1);
    assert_eq!(report.released_revisions, 1);
    assert_eq!(report.deleted_artifacts, vec![drop_id.to_owned()]);
    // Referenced (published, superseded default, channel) artifacts and their files stay.
    for id in [DEFAULT, keep_id] {
        assert!(themes::artifact_exists(&runtime, id).await.unwrap(), "{id}");
        storage
            .private
            .head(&themes::object_key(id, "server/entry.mjs"))
            .await
            .unwrap();
    }
    assert!(!themes::artifact_exists(&runtime, drop_id).await.unwrap());
    assert!(
        storage
            .private
            .head(&themes::object_key(drop_id, "manifest.json"))
            .await
            .is_err()
    );
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let list = themes::list(&mut tx).await.unwrap();
    let st = |id: Uuid| list.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!(st(stuck.id).status, "failed");
    assert!(st(stuck.id).failures[0].contains("did not finish"));
    assert_eq!(st(dropped.id).artifact_id, None);
    // Idempotent.
    tx.commit().await.unwrap();
    assert_eq!(
        themes::maintenance(&runtime, &storage).await.unwrap(),
        Default::default()
    );
}
