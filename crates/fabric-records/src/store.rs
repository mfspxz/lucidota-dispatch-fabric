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

/// Content digest: `sha256:` + lowercase hex of the exact stored bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentDigest(pub String);

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
        let _ = bytes;
        todo!("T081: implement content-addressed put")
    }

    /// Retrieve bytes by digest. Returns None when the digest was never
    /// stored. Returned bytes are always byte-identical to what was stored.
    pub fn get(&self, digest: &ContentDigest) -> Option<&[u8]> {
        let _ = digest;
        todo!("T081: implement content-addressed get")
    }

    /// Number of distinct blobs stored.
    pub fn len(&self) -> usize {
        todo!("T081: implement store length")
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Compute the content digest of bytes: `sha256:` + hex(sha256(bytes)).
pub fn digest_bytes(bytes: &[u8]) -> ContentDigest {
    let _ = bytes;
    todo!("T081: implement sha256 digest")
}
