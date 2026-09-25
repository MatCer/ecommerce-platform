//! Capability tokens (spec A1, A4): 256 random bits, hex-encoded for the client, stored only as
//! their SHA-256. Possession is the authorization, so lookups go by hash and a database leak
//! does not hand out working tokens.

use sha2::{Digest, Sha256};

/// A freshly minted token and the hash to store.
pub struct Minted {
    pub token: String,
    pub hash: Vec<u8>,
}

pub fn mint() -> Minted {
    let bytes: [u8; 32] = rand::random();
    let token = hex::encode(bytes);
    Minted {
        hash: hash(&token),
        token,
    }
}

pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Shape check before any lookup: 64 lowercase hex characters.
pub fn well_formed(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex_and_hash_to_32_bytes() {
        let a = mint();
        let b = mint();
        assert_ne!(a.token, b.token);
        assert!(well_formed(&a.token));
        assert_eq!(a.hash.len(), 32);
        assert_eq!(a.hash, hash(&a.token));
        assert!(!well_formed(&a.token.to_uppercase()));
        assert!(!well_formed("abc"));
    }
}
