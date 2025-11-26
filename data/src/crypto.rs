//! Crypto/hash utilities (replacement for hash_t and CryptoUtil stubs).

use sha2::{Digest, Sha512};

/// SHA-512 hash wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash512(pub [u8; 64]);

/// SHA-256 hash wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hash256(pub [u8; 32]);

impl From<Hash512> for Vec<u8> {
    fn from(h: Hash512) -> Self {
        h.0.to_vec()
    }
}

pub fn sha512(bytes: &[u8]) -> Hash512 {
    let mut hasher = Sha512::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&out);
    Hash512(arr)
}

/// Convenience hash of strings.
pub fn sha512_str(s: &str) -> Hash512 {
    sha512(s.as_bytes())
}

pub fn sha256(bytes: &[u8]) -> Hash256 {
    use sha2::Sha256;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    Hash256(arr)
}

pub fn sha256_str(s: &str) -> Hash256 {
    sha256(s.as_bytes())
}
