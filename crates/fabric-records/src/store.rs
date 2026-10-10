//! T081 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Store patches, prompts, responses, logs, test output, manifests, and
//! reports by content digest. The digest is the identity: `put` returns the
//! digest of the exact bytes stored; `get` retrieves by digest and must
//! return byte-identical content or an error. Storage is immutable — there
//! is no update or delete.
//!
//! Worker R2: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Content digest: `sha256:` + lowercase hex of the exact stored bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentDigest(pub String);

impl ContentDigest {
    /// Parse a `sha256:<64 hex chars>` digest. Rejects malformed input —
    /// a digest is an identity, never a free-form string.
    pub fn parse(s: &str) -> Result<Self> {
        let hex = (s.len() >= 7 && s[..7].eq_ignore_ascii_case("sha256:"))
            .then(|| &s[7..])
            .ok_or_else(|| {
                fabric_types::FabricError::InvalidInput {
                    field: "digest",
                    reason: format!("digest must start with 'sha256:': {:?}", truncate(s)),
                }
            })?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "digest",
                reason: format!("digest must be 'sha256:' + 64 hex chars: {:?}", truncate(s)),
            });
        }
        Ok(ContentDigest(format!("sha256:{}", hex.to_ascii_lowercase())))
    }
}

/// T081: content-addressed, immutable byte store.
#[derive(Debug, Default)]
pub struct ContentStore {
    bytes: std::collections::HashMap<ContentDigest, Vec<u8>>,
}

impl ContentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store bytes; return their content digest. Storing identical bytes
    /// twice returns the same digest and stores once.
    pub fn put(&mut self, bytes: &[u8]) -> ContentDigest {
        let digest = digest_bytes(bytes);
        self.bytes.entry(digest.clone()).or_insert_with(|| bytes.to_vec());
        digest
    }

    /// Retrieve bytes by digest. Returns None when the digest was never
    /// stored. Returned bytes are always byte-identical to what was stored.
    pub fn get(&self, digest: &ContentDigest) -> Option<&[u8]> {
        self.bytes.get(digest).map(Vec::as_slice)
    }

    /// Number of distinct blobs stored.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Compute the content digest of bytes: `sha256:` + hex(sha256(bytes)).
pub fn digest_bytes(bytes: &[u8]) -> ContentDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    ContentDigest(format!("sha256:{}", hex_encode(&hash)))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Truncate long inputs for diagnostics without panicking on UTF-8
/// boundaries (byte-truncate to at most 64 bytes, then trim to char
/// boundary).
fn truncate(s: &str) -> String {
    const MAX: usize = 64;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_sha256_prefix_and_hex() {
        // Known sha256 of "abc".
        let d = digest_bytes(b"abc");
        assert_eq!(
            d.0,
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn empty_digest_matches_known_sha256() {
        let d = digest_bytes(b"");
        assert_eq!(
            d.0,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn put_get_roundtrip_is_byte_identical() {
        let mut store = ContentStore::new();
        let payload = b"\x00\xff binary \xf0\x9f\x98\x80 data";
        let digest = store.put(payload);
        assert_eq!(store.get(&digest), Some(payload.as_slice()));
    }

    #[test]
    fn duplicate_put_stores_once() {
        let mut store = ContentStore::new();
        let d1 = store.put(b"same");
        let d2 = store.put(b"same");
        assert_eq!(d1, d2);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn distinct_bytes_distinct_digests() {
        let mut store = ContentStore::new();
        let d1 = store.put(b"one");
        let d2 = store.put(b"two");
        assert_ne!(d1, d2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn get_unknown_digest_returns_none() {
        let store = ContentStore::new();
        let d = digest_bytes(b"never stored");
        assert_eq!(store.get(&d), None);
    }

    #[test]
    fn is_empty_and_len() {
        let mut store = ContentStore::new();
        assert!(store.is_empty());
        store.put(b"x");
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn parse_accepts_canonical_digests() {
        let d = digest_bytes(b"abc");
        assert_eq!(ContentDigest::parse(&d.0), Ok(d.clone()));
        // Uppercase hex normalizes to lowercase.
        assert_eq!(ContentDigest::parse(&d.0.to_ascii_uppercase()), Ok(d));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(ContentDigest::parse("no-prefix").is_err());
        assert!(ContentDigest::parse("sha256:zzzz").is_err());
        assert!(ContentDigest::parse("sha256:abc").is_err());
        assert!(ContentDigest::parse(&format!("sha256:{}", "a".repeat(65))).is_err());
    }

    #[test]
    fn truncate_is_utf8_safe() {
        // 20 emoji = 80 bytes; truncation must not split a codepoint.
        let s = "🙂".repeat(20);
        let t = truncate(&s);
        assert!(t.ends_with("..."));
        assert!(t.len() <= 64 + 3);
        // Valid UTF-8 by construction (would panic on invalid slicing).
        let _ = t.as_str();
    }
}
