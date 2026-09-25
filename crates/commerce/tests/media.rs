//! Media upload, verification, processing and deletion with Postgres (runtime role) and
//! in-memory object storage.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;

use commerce::media::{self, AssetStatus, NewUpload, Processed};
use image::{ImageEncoder, Rgb, RgbImage};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::storage::Storage;
use sqlx::PgPool;
use uuid::Uuid;

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
    let mut buf = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
        .write_image(&img, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    buf.into_inner()
}

fn upload(size: usize) -> NewUpload {
    NewUpload {
        filename: Some("photo.jpg".into()),
        content_type: "image/jpeg".into(),
        size: size as u64,
    }
}

async fn put(storage: &Storage, tenant: Uuid, id: Uuid, bytes: Vec<u8>) {
    storage
        .private
        .put(
            &Path::from(format!("uploads/{tenant}/{id}")),
            PutPayload::from(bytes),
        )
        .await
        .unwrap();
}

async fn jobs(owner: &PgPool, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM queue.jobs WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(owner)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn upload_verify_process_and_delete(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let photo = jpeg(400, 200);

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let up = media::create_upload(&mut tx, &storage, "u1", &upload(photo.len()))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let id = up.asset.id;
    assert_eq!(up.asset.status, AssetStatus::Pending);
    let url = object_store::signer::Url::parse(&up.upload.url).unwrap();
    assert_eq!(url.host_str(), Some("s3.test"));
    assert_eq!(url.path(), format!("/private/uploads/{tenant}/{id}"));
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["X-Amz-Expires"], "900");
    assert_eq!(query["X-Amz-SignedHeaders"], "content-type;host");
    assert_eq!(up.upload.headers["content-type"], "image/jpeg");

    // Nothing uploaded yet.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let err = media::complete(&mut tx, &storage, "u1", id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "upload_missing");
    tx.rollback().await.unwrap();

    // A non-image is refused and removed; the asset stays pending.
    put(&storage, tenant, id, b"%PDF-1.7 not an image".to_vec()).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let err = media::complete(&mut tx, &storage, "u1", id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unsupported_type");
    tx.rollback().await.unwrap();
    assert!(
        storage
            .private
            .head(&Path::from(format!("uploads/{tenant}/{id}")))
            .await
            .is_err()
    );

    // A different size than declared is refused too.
    put(&storage, tenant, id, jpeg(10, 10)).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let err = media::complete(&mut tx, &storage, "u1", id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "size_mismatch");
    tx.rollback().await.unwrap();

    put(&storage, tenant, id, photo.clone()).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let done = media::complete(&mut tx, &storage, "u1", id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(done.status, AssetStatus::Processing);
    assert_eq!((done.width, done.height), (Some(400), Some(200)));
    assert_eq!(done.mime.as_deref(), Some("image/jpeg"));
    assert_eq!(done.sha256.as_deref().map(str::len), Some(64));
    // Completing twice is harmless and does not queue a second job.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    media::complete(&mut tx, &storage, "u1", id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(jobs(&db, media::PROCESS_JOB).await.len(), 1);

    assert_eq!(
        media::process(&runtime, &storage, tenant, id)
            .await
            .unwrap(),
        Processed::Ready
    );
    assert_eq!(
        media::process(&runtime, &storage, tenant, id)
            .await
            .unwrap(),
        Processed::Skipped
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let ready = media::get(&mut tx, &storage, id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(ready.status, AssetStatus::Ready);
    // 160, 320 and the original 400 px, three formats each.
    assert_eq!(ready.variants.len(), 3 * 3);
    let v = ready
        .variants
        .iter()
        .find(|v| v.width == 320 && v.format == "avif")
        .unwrap();
    assert_eq!(v.height, 160);
    assert!(v.key.starts_with(&format!("media/{tenant}/")) && v.key.ends_with(".avif"));
    assert_eq!(v.url, format!("http://media.test/{}", v.key));
    for v in &ready.variants {
        let got = storage
            .public
            .get(&Path::from(v.key.as_str()))
            .await
            .unwrap();
        let ct = got
            .attributes
            .get(&object_store::Attribute::ContentType)
            .unwrap()
            .to_string();
        assert!(ct.starts_with("image/"), "{ct}");
        assert_eq!(got.bytes().await.unwrap().len() as u64, v.bytes);
    }
    let events: Vec<String> =
        sqlx::query_scalar("SELECT type FROM queue.outbox WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(&db)
            .await
            .unwrap();
    assert!(events.contains(&"asset.ready".to_owned()));

    // Another tenant sees nothing.
    let (other, _) = testkit::tenant(&runtime, "other").await;
    let mut tx = tenant_tx(&runtime, other).await.unwrap();
    assert_eq!(
        media::get(&mut tx, &storage, id).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        media::complete(&mut tx, &storage, "u2", id)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        media::delete(&mut tx, &storage, "u2", id)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    tx.rollback().await.unwrap();

    // Delete: a purge job removes the original and the variants.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    media::delete(&mut tx, &storage, "u1", id).await.unwrap();
    tx.commit().await.unwrap();
    let purge = jobs(&db, media::PURGE_JOB).await.pop().unwrap();
    let list = |k: &str| -> Vec<String> { serde_json::from_value(purge[k].clone()).unwrap() };
    assert_eq!(list("public").len(), 9);
    media::purge(&storage, &list("private"), &list("public"))
        .await
        .unwrap();
    assert!(
        storage
            .public
            .head(&Path::from(v.key.as_str()))
            .await
            .is_err()
    );
    // Purging again (job retry) is fine.
    media::purge(&storage, &list("private"), &list("public"))
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn undecodable_originals_fail_and_used_assets_cannot_be_deleted(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;

    // Valid PNG header, corrupted pixel data: verification passes, decoding fails.
    let img = RgbImage::from_fn(400, 300, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 7]));
    let mut broken = Vec::new();
    image::codecs::png::PngEncoder::new(&mut broken)
        .write_image(&img, 400, 300, image::ExtendedColorType::Rgb8)
        .unwrap();
    let idat = broken.windows(4).position(|w| w == b"IDAT").unwrap();
    broken[idat + 10] ^= 0xFF;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let png = NewUpload {
        content_type: "image/png".into(),
        ..upload(broken.len())
    };
    let up = media::create_upload(&mut tx, &storage, "u1", &png)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    put(&storage, tenant, up.asset.id, broken).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        media::process(&runtime, &storage, tenant, up.asset.id)
            .await
            .unwrap(),
        Processed::Failed
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let failed = media::get(&mut tx, &storage, up.asset.id).await.unwrap();
    assert_eq!(failed.status, AssetStatus::Failed);
    assert!(failed.error.is_some());
    // `complete` on a failed asset runs processing again from the kept original.
    let retried = media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    assert_eq!(
        (retried.status, retried.error),
        (AssetStatus::Processing, None)
    );
    tx.commit().await.unwrap();
    assert_eq!(jobs(&db, media::PROCESS_JOB).await.len(), 2);
    assert_eq!(
        media::process(&runtime, &storage, tenant, up.asset.id)
            .await
            .unwrap(),
        Processed::Failed
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();

    // Referenced by a product: 409.
    sqlx::query(
        "WITH p AS (INSERT INTO products (tenant_id) VALUES ($1) RETURNING id)
         INSERT INTO product_media (tenant_id, product_id, asset_id, position) SELECT $1, id, $2, 0 FROM p",
    )
    .bind(tenant)
    .bind(up.asset.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let err = media::delete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "asset_in_use");

    let invalid = NewUpload {
        content_type: "image/svg+xml".into(),
        ..upload(10)
    };
    assert_eq!(invalid.validate().unwrap_err().code(), "unsupported_type");
    assert_eq!(
        upload(21 * 1024 * 1024).validate().unwrap_err().code(),
        "file_too_large"
    );
    assert_eq!(upload(0).validate().unwrap_err().code(), "file_too_large");
}

/// Uploads `bytes` as a new asset and completes it.
async fn completed(runtime: &PgPool, storage: &Storage, tenant: Uuid, bytes: Vec<u8>) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let up = media::create_upload(&mut tx, storage, "u1", &upload(bytes.len()))
        .await
        .unwrap();
    put(storage, tenant, up.asset.id, bytes).await;
    media::complete(&mut tx, storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    up.asset.id
}

#[sqlx::test(migrations = "../../migrations")]
async fn uploads_after_complete_cannot_change_what_is_processed(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let id = completed(&runtime, &storage, tenant, jpeg(200, 100)).await;

    // The verified bytes moved to a server-only key; the upload key is free again.
    let original = Path::from(format!("originals/{tenant}/{id}"));
    assert!(storage.private.head(&original).await.is_ok());
    // A second PUT with the still-valid upload URL is simply ignored.
    put(&storage, tenant, id, jpeg(300, 300)).await;
    assert_eq!(
        media::process(&runtime, &storage, tenant, id)
            .await
            .unwrap(),
        Processed::Ready
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let asset = media::get(&mut tx, &storage, id).await.unwrap();
    assert_eq!(asset.variants.iter().map(|v| v.width).max(), Some(200));
    tx.rollback().await.unwrap();

    // An original that no longer matches the verified digest is refused.
    let tampered = completed(&runtime, &storage, tenant, jpeg(64, 64)).await;
    storage
        .private
        .put(
            &Path::from(format!("originals/{tenant}/{tampered}")),
            PutPayload::from(jpeg(65, 64)),
        )
        .await
        .unwrap();
    assert_eq!(
        media::process(&runtime, &storage, tenant, tampered)
            .await
            .unwrap(),
        Processed::Failed
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_an_asset_keeps_an_identical_one_intact(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let photo = jpeg(120, 80);
    let a = completed(&runtime, &storage, tenant, photo.clone()).await;
    let b = completed(&runtime, &storage, tenant, photo).await;
    for id in [a, b] {
        media::process(&runtime, &storage, tenant, id)
            .await
            .unwrap();
    }
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let kept = media::get(&mut tx, &storage, b).await.unwrap();
    media::delete(&mut tx, &storage, "u1", a).await.unwrap();
    tx.commit().await.unwrap();
    let purge: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM queue.jobs WHERE kind = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(media::PURGE_JOB)
    .fetch_one(&db)
    .await
    .unwrap();
    let keys = |k: &str| -> Vec<String> { serde_json::from_value(purge[k].clone()).unwrap() };
    media::purge(&storage, &keys("private"), &keys("public"))
        .await
        .unwrap();
    for v in &kept.variants {
        assert!(
            storage
                .public
                .head(&Path::from(v.key.as_str()))
                .await
                .is_ok(),
            "{} deleted with the other asset",
            v.key
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_rolled_back_complete_can_be_retried(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let photo = jpeg(40, 20);
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let up = media::create_upload(&mut tx, &storage, "u1", &upload(photo.len()))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    put(&storage, tenant, up.asset.id, photo).await;

    // E.g. the database fails after verification: nothing is lost.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let done = media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(done.status, AssetStatus::Processing);
}
