//! Secrets of the theme pipeline, derived from one platform secret (`THEME_SECRET`) with
//! domain separation:
//! - preview tokens (A21): HMAC-SHA256 bound to tenant + revision + expiry (at most 1 h);
//! - the per-tenant `ASTRO_KEY` (follow-up WP6): Astro encrypts server-island props with it and
//!   embeds it in the tenant's own bundle, so a fixed key per tenant makes builds reproducible
//!   without sharing a key between tenants.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// A21: preview links live at most one hour.
pub const PREVIEW_TTL_SECS: i64 = 3600;

/// Not `Debug`: holds the secret.
#[derive(Clone)]
pub struct ThemeKeys {
    secret: Vec<u8>,
}

impl ThemeKeys {
    /// `secret` must be at least 32 bytes (checked by the config loader).
    pub fn new(secret: &[u8]) -> Self {
        Self {
            secret: secret.to_vec(),
        }
    }

    fn mac(&self, label: &str) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
        mac.update(label.as_bytes());
        mac.update(b"\0");
        mac
    }

    fn preview_mac(&self, tenant: Uuid, revision: Uuid, expires: i64) -> Hmac<Sha256> {
        let mut mac = self.mac("preview-token:v1");
        mac.update(tenant.as_bytes());
        mac.update(revision.as_bytes());
        mac.update(&expires.to_be_bytes());
        mac
    }

    /// `<revision uuid>.<expires unix>.<hex HMAC>`; the tenant is bound by the MAC and comes
    /// from the preview host on verification.
    pub fn preview_token(&self, tenant: Uuid, revision: Uuid, expires: i64) -> String {
        let tag = self
            .preview_mac(tenant, revision, expires)
            .finalize()
            .into_bytes();
        format!("{}.{expires}.{}", revision.simple(), hex::encode(tag))
    }

    /// The revision a token grants, if it is authentic for `tenant`, not expired and not
    /// valid for longer than [`PREVIEW_TTL_SECS`] (constant-time MAC comparison).
    pub fn verify_preview(&self, tenant: Uuid, token: &str, now: i64) -> Option<Uuid> {
        if token.len() > 128 {
            return None;
        }
        let mut parts = token.split('.');
        let (rev, exp, tag) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || rev.len() != 32 || tag.len() != 64 {
            return None;
        }
        let revision = Uuid::try_parse(rev).ok()?;
        let expires: i64 = exp.parse().ok()?;
        if expires <= now || expires > now + PREVIEW_TTL_SECS {
            return None;
        }
        let tag = hex::decode(tag).ok()?;
        self.preview_mac(tenant, revision, expires)
            .verify_slice(&tag)
            .ok()?;
        Some(revision)
    }

    /// The tenant's 256-bit `ASTRO_KEY` (raw bytes; Astro wants it base64-encoded).
    pub fn astro_key(&self, tenant: Uuid) -> [u8; 32] {
        let mut mac = self.mac("astro-key:v1");
        mac.update(tenant.as_bytes());
        mac.finalize().into_bytes().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn preview_tokens_are_bound() {
        let keys = ThemeKeys::new(&[7u8; 32]);
        let (t1, t2) = (Uuid::now_v7(), Uuid::now_v7());
        let (r1, r2) = (Uuid::now_v7(), Uuid::now_v7());
        let token = keys.preview_token(t1, r1, NOW + 600);
        assert_eq!(keys.verify_preview(t1, &token, NOW), Some(r1));
        // Another tenant, expired, too long-lived, tampered, another key.
        assert_eq!(keys.verify_preview(t2, &token, NOW), None);
        assert_eq!(keys.verify_preview(t1, &token, NOW + 600), None);
        let long = keys.preview_token(t1, r1, NOW + PREVIEW_TTL_SECS + 1);
        assert_eq!(keys.verify_preview(t1, &long, NOW), None);
        let swapped = token.replacen(&r1.simple().to_string(), &r2.simple().to_string(), 1);
        assert_eq!(keys.verify_preview(t1, &swapped, NOW), None);
        let later = token.replacen(&(NOW + 600).to_string(), &(NOW + 900).to_string(), 1);
        assert_eq!(keys.verify_preview(t1, &later, NOW), None);
        assert_eq!(
            ThemeKeys::new(&[8u8; 32]).verify_preview(t1, &token, NOW),
            None
        );
        for junk in ["", "a.b.c", "x", &format!("{token}.x")] {
            assert_eq!(keys.verify_preview(t1, junk, NOW), None, "{junk}");
        }
    }

    #[test]
    fn astro_keys_are_stable_per_tenant() {
        let keys = ThemeKeys::new(&[7u8; 32]);
        let (t1, t2) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(keys.astro_key(t1), keys.astro_key(t1));
        assert_ne!(keys.astro_key(t1), keys.astro_key(t2));
        assert_ne!(keys.astro_key(t1), ThemeKeys::new(&[8u8; 32]).astro_key(t1));
    }
}
