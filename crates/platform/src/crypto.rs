//! Integration secrets at rest (spec §14): AES-256-GCM with a platform key from the
//! environment (`SECRETS_KEY`). Stored form: 12-byte random nonce || ciphertext + tag. The
//! associated data binds a ciphertext to its row (e.g. `webhook:<subscription id>`), so a
//! ciphertext copied into another row does not decrypt.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};

const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
#[error("secret could not be decrypted (wrong SECRETS_KEY or tampered data)")]
pub struct DecryptError;

#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key)),
        }
    }

    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        let nonce_bytes: [u8; NONCE_LEN] = rand::random();
        let sealed = self
            .cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            // Only fails for plaintexts over ~64 GB.
            .unwrap_or_default();
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&sealed);
        out
    }

    pub fn open(&self, stored: &[u8], aad: &[u8]) -> Result<Vec<u8>, DecryptError> {
        if stored.len() < NONCE_LEN {
            return Err(DecryptError);
        }
        let (nonce, sealed) = stored.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| DecryptError)?;
        self.cipher
            .decrypt(&Nonce::from(nonce), Payload { msg: sealed, aad })
            .map_err(|_| DecryptError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_other_keys_rows_and_tampering() {
        let a = SecretBox::new(&[7; 32]);
        let sealed = a.seal(b"whsec_abc", b"webhook:1");
        assert_ne!(sealed, a.seal(b"whsec_abc", b"webhook:1"), "fresh nonce");
        assert_eq!(a.open(&sealed, b"webhook:1").unwrap(), b"whsec_abc");
        assert!(a.open(&sealed, b"webhook:2").is_err());
        assert!(
            SecretBox::new(&[8; 32])
                .open(&sealed, b"webhook:1")
                .is_err()
        );
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(a.open(&bad, b"webhook:1").is_err());
        assert!(a.open(&[1, 2], b"webhook:1").is_err());
    }
}
