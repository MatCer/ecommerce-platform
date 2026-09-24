//! S3-compatible object storage (MinIO locally, R2 in prod) via `object_store`.

use std::sync::Arc;

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;

use crate::config::S3Config;

/// The platform's two buckets (spec D18).
#[derive(Clone)]
pub struct Storage {
    /// Public-read: re-encoded media only (spec A21).
    pub public: Arc<dyn ObjectStore>,
    /// Everything else (invoices, labels, exports, theme sources/bundles), served through
    /// short-lived presigned URLs after authorization (spec A21).
    pub private: Arc<dyn ObjectStore>,
}

impl Storage {
    pub fn s3(cfg: &S3Config) -> Result<Self, object_store::Error> {
        Ok(Self {
            public: s3(cfg, &cfg.bucket_public)?,
            private: s3(cfg, &cfg.bucket_private)?,
        })
    }

    /// Both buckets reachable with the configured credentials.
    pub async fn ping(&self) -> Result<(), object_store::Error> {
        ping(self.public.as_ref()).await?;
        ping(self.private.as_ref()).await
    }
}

/// One store per bucket. Path-style requests, so MinIO works without wildcard DNS.
pub fn s3(cfg: &S3Config, bucket: &str) -> Result<Arc<dyn ObjectStore>, object_store::Error> {
    let store = AmazonS3Builder::new()
        .with_endpoint(cfg.endpoint.as_str().trim_end_matches('/'))
        .with_allow_http(cfg.endpoint.scheme() == "http")
        .with_region(&cfg.region)
        .with_access_key_id(&cfg.access_key_id)
        .with_secret_access_key(&cfg.secret_access_key)
        .with_bucket_name(bucket)
        .with_virtual_hosted_style_request(false)
        .build()?;
    Ok(Arc::new(store))
}

/// Lists a prefix that normally does not exist. This proves the endpoint is reachable, the
/// credentials are accepted and the bucket exists (a missing bucket is an error, unlike `HEAD`
/// on a missing key, which cannot tell "no key" from "no bucket").
pub async fn ping(store: &dyn ObjectStore) -> Result<(), object_store::Error> {
    store
        .list_with_delimiter(Some(&Path::from("__readyz")))
        .await
        .map(|_| ())
}
