//! Encryption at rest for provider credentials (spec §14, A21), e.g. a tenant's Fio API token.
//!
//! AES-256-GCM (aws-lc-rs) with a random 96-bit nonce per message and caller-supplied
//! associated data that binds a ciphertext to its row (tenant and record id), so a value
//! copied into another row does not decrypt. Layout: `version (1) || nonce (12) || ciphertext
//! || tag (16)`; the version byte leaves room for key rotation.

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};

const VERSION: u8 = 1;
/// Version byte + nonce + tag: the size of an empty plaintext.
pub const OVERHEAD: usize = 1 + NONCE_LEN + 16;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("the key must be 32 bytes as 64 hex characters")]
    BadKey,
    #[error("encryption failed")]
    Seal,
    #[error("the ciphertext is invalid or bound to another record")]
    Open,
}

/// A symmetric key for sealing secrets. Not `Debug`: it holds key material.
pub struct SecretBox {
    key: LessSafeKey,
}

impl SecretBox {
    /// A key from 64 hex characters (`openssl rand -hex 32`).
    pub fn from_hex(hex_key: &str) -> Result<Self, CryptoError> {
        let bytes = decode_hex(hex_key.trim()).ok_or(CryptoError::BadKey)?;
        if bytes.len() != 32 {
            return Err(CryptoError::BadKey);
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| CryptoError::BadKey)?;
        Ok(Self {
            key: LessSafeKey::new(key),
        })
    }

    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| CryptoError::Seal)?;
        let mut body = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| CryptoError::Seal)?;
        let mut out = Vec::with_capacity(OVERHEAD + plaintext.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < OVERHEAD || sealed[0] != VERSION {
            return Err(CryptoError::Open);
        }
        let nonce: [u8; NONCE_LEN] = sealed[1..=NONCE_LEN]
            .try_into()
            .map_err(|_| CryptoError::Open)?;
        let mut body = sealed[1 + NONCE_LEN..].to_vec();
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| CryptoError::Open)?;
        Ok(plain.to_vec())
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn round_trip_bound_to_the_aad() {
        let b = SecretBox::from_hex(KEY).unwrap();
        let sealed = b.seal(b"fio-token", b"tenant:account").unwrap();
        assert_eq!(sealed.len(), OVERHEAD + 9);
        assert_eq!(b.open(&sealed, b"tenant:account").unwrap(), b"fio-token");
        assert_eq!(b.open(&sealed, b"tenant:other"), Err(CryptoError::Open));
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(b.open(&tampered, b"tenant:account"), Err(CryptoError::Open));
        assert_ne!(
            b.seal(b"fio-token", b"a").unwrap()[1..13],
            b.seal(b"fio-token", b"a").unwrap()[1..13],
            "fresh nonces"
        );
        let other = SecretBox::from_hex(&KEY.replace("00", "ff")).unwrap();
        assert_eq!(
            other.open(&sealed, b"tenant:account"),
            Err(CryptoError::Open)
        );
    }

    #[test]
    fn keys_must_be_32_hex_bytes() {
        assert!(SecretBox::from_hex("abcd").is_err());
        assert!(SecretBox::from_hex(&"zz".repeat(32)).is_err());
        assert!(SecretBox::from_hex(&KEY[..63]).is_err());
    }
}
