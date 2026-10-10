//! T086 — response artifact storage with bounded outputs.
//!
//! Stores response artifacts (or bounded, redacted outputs) together with
//! provider request IDs, usage figures, finish reasons, and attempt
//! metadata. Outputs are capped at a configurable byte bound: over-cap
//! content is truncated on a UTF-8 character boundary and flagged with
//! `truncated: true` plus the original size — never truncated silently.
//!
//! Content addressing needs a digest function. `sha2` is deliberately NOT
//! a dependency of this module (the crate's dependency set is frozen to this
//! cell); instead the digester is injected via [`ContentDigest`]. The
//! integrator MUST wire a real SHA-256 implementation (e.g. add
//! `sha2 = "0.10"` to `fabric-verify/Cargo.toml` and provide a
//! `Sha256Digest`). Tests use a deterministic non-crypto double.

use fabric_types::{ArtifactRef, FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Injectable content digest. Object-safe so stores can own a boxed digester.
pub trait ContentDigest: Send + Sync {
    /// Algorithm label used in `algo:hex` digest strings, e.g. `"sha256"`.
    fn algorithm(&self) -> &'static str;
    /// Lowercase hex digest of `bytes`.
    fn digest_hex(&self, bytes: &[u8]) -> String;
    /// Full `algo:hex` digest reference.
    fn digest_ref(&self, bytes: &[u8]) -> String {
        format!("{}:{}", self.algorithm(), self.digest_hex(bytes))
    }
}

/// Why an attempt finished. Distinct outcomes, never stringly-typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinishReason {
    Completed,
    Timeout,
    Cancelled,
    MalformedOutput,
    PolicyDenied,
    VerifierFailed,
    InfraError,
}

/// Provider-reported usage figures. Unreported fields stay `None` — they are
/// never zero-filled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageFigures {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub latency_ms: Option<u64>,
    /// Estimated cost as a decimal string with currency, e.g. `"0.0012 USD"`.
    /// String, not float: no rounding surprises in the ledger.
    pub estimated_cost: Option<String>,
}

/// Metadata for one attempt, bound to the artifacts it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptMetadata {
    pub task_id: TaskId,
    /// 1-based attempt number.
    pub attempt: u32,
    /// Provider-side request/correlation ID, if the provider reported one.
    pub provider_request_id: Option<String>,
    pub usage: UsageFigures,
    pub finish_reason: FinishReason,
    /// Inject at the call site; the store never reads the system clock.
    pub recorded_at_ms: u64,
}

/// One stored artifact. `body` is the bytes actually retained (possibly
/// truncated); `original_size_bytes` is what the producer emitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredArtifact {
    /// Content digest of the RETAINED bytes (`algo:hex`).
    pub digest: String,
    pub media_type: String,
    pub provenance: String,
    pub size_bytes: u64,
    pub original_size_bytes: u64,
    /// True when the producer's output exceeded the byte cap.
    pub truncated: bool,
    /// Retained bytes (at most `max_output_bytes`).
    pub body: Vec<u8>,
}

impl StoredArtifact {
    /// Build the [`ArtifactRef`] pointing at this artifact.
    pub fn as_ref(&self) -> Result<ArtifactRef> {
        ArtifactRef::new(
            self.digest.clone(),
            self.media_type.clone(),
            self.size_bytes,
            self.provenance.clone(),
        )
    }
}

/// Configuration for [`ArtifactStore`].
pub struct StoreConfig {
    /// Maximum retained bytes per artifact. Must be non-zero.
    pub max_output_bytes: u64,
    /// Digest implementation (SHA-256 in production; see module docs).
    pub digester: Box<dyn ContentDigest>,
}

/// Append-only artifact store with bounded outputs.
pub struct ArtifactStore {
    config: StoreConfig,
    artifacts: HashMap<String, StoredArtifact>,
    attempts: Vec<AttemptRecord>,
}

impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("max_output_bytes", &self.config.max_output_bytes)
            .field("artifacts", &self.artifacts.len())
            .field("attempts", &self.attempts.len())
            .finish()
    }
}

/// One attempt bound to the digests of the artifacts it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub metadata: AttemptMetadata,
    pub artifact_digests: Vec<String>,
}

impl ArtifactStore {
    pub fn new(config: StoreConfig) -> Result<Self> {
        if config.max_output_bytes == 0 {
            return Err(FabricError::InvalidInput {
                field: "max_output_bytes",
                reason: "must be non-zero".to_string(),
            });
        }
        Ok(Self {
            config,
            artifacts: HashMap::new(),
            attempts: Vec::new(),
        })
    }

    /// Store a response body, truncating to the byte cap when necessary.
    /// Truncation happens on a UTF-8 character boundary and is always
    /// flagged; the original size is preserved in the record.
    pub fn store_response(
        &mut self,
        body: &[u8],
        media_type: impl Into<String>,
        provenance: impl Into<String>,
    ) -> Result<StoredArtifact> {
        let cap = self.config.max_output_bytes as usize;
        let (retained, truncated) = truncate_utf8_safe(body, cap);
        let digest = self.config.digester.digest_ref(&retained);
        let artifact = StoredArtifact {
            digest: digest.clone(),
            media_type: media_type.into(),
            provenance: provenance.into(),
            size_bytes: retained.len() as u64,
            original_size_bytes: body.len() as u64,
            truncated,
            body: retained,
        };
        self.artifacts.insert(digest, artifact.clone());
        Ok(artifact)
    }

    /// Record one attempt's metadata, bound to artifact digests.
    /// Every digest must already be stored; unknown digests are rejected.
    pub fn record_attempt(
        &mut self,
        metadata: AttemptMetadata,
        artifact_digests: Vec<String>,
    ) -> Result<()> {
        validate_attempt(&metadata)?;
        for d in &artifact_digests {
            if !self.artifacts.contains_key(d) {
                return Err(FabricError::InvalidInput {
                    field: "artifact_digests",
                    reason: format!("unknown artifact digest: {}", truncate_diag(d)),
                });
            }
        }
        self.attempts.push(AttemptRecord {
            metadata,
            artifact_digests,
        });
        Ok(())
    }

    /// Look up a stored artifact by digest.
    pub fn get(&self, digest: &str) -> Option<&StoredArtifact> {
        self.artifacts.get(digest)
    }

    /// All attempt records for a task, in record order.
    pub fn attempts_for(&self, task_id: &TaskId) -> Vec<&AttemptRecord> {
        self.attempts
            .iter()
            .filter(|r| &r.metadata.task_id == task_id)
            .collect()
    }

    /// Number of stored artifacts.
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

fn validate_attempt(m: &AttemptMetadata) -> Result<()> {
    if m.attempt == 0 {
        return Err(FabricError::InvalidInput {
            field: "attempt",
            reason: "attempt numbers are 1-based".to_string(),
        });
    }
    if let Some(id) = &m.provider_request_id {
        if id.is_empty() || id.len() > 256 {
            return Err(FabricError::InvalidInput {
                field: "provider_request_id",
                reason: "length must be 1..=256 characters".to_string(),
            });
        }
    }
    Ok(())
}

/// Truncate to at most `cap` bytes without splitting a UTF-8 character.
/// Returns the retained bytes and whether truncation occurred.
fn truncate_utf8_safe(body: &[u8], cap: usize) -> (Vec<u8>, bool) {
    if body.len() <= cap {
        return (body.to_vec(), false);
    }
    let mut end = cap;
    while end > 0 && !is_char_boundary(body, end) {
        end -= 1;
    }
    (body[..end].to_vec(), true)
}

fn is_char_boundary(body: &[u8], idx: usize) -> bool {
    if idx == 0 || idx >= body.len() {
        return true;
    }
    // A boundary is any byte that is not a UTF-8 continuation byte.
    (body[idx] & 0xC0) != 0x80
}

/// UTF-8-safe diagnostic truncation: cut to 64 chars, never mid-character.
fn truncate_diag(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.char_indices() {
        if i >= 64 {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::ContentDigest;

    /// Deterministic non-crypto digest for tests ONLY. Never used in
    /// production; the integrator wires SHA-256 (see module docs).
    pub struct TestDigest;

    impl ContentDigest for TestDigest {
        fn algorithm(&self) -> &'static str {
            "fnv1a64-test"
        }

        fn digest_hex(&self, bytes: &[u8]) -> String {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            format!("{:016x}", h)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TestDigest;
    use super::*;
    use fabric_types::TaskId;

    fn store() -> ArtifactStore {
        ArtifactStore::new(StoreConfig {
            max_output_bytes: 16,
            digester: Box::new(TestDigest),
        })
        .unwrap()
    }

    fn meta() -> AttemptMetadata {
        AttemptMetadata {
            task_id: TaskId::new("T-1").unwrap(),
            attempt: 1,
            provider_request_id: Some("req-abc".to_string()),
            usage: UsageFigures {
                input_tokens: Some(10),
                output_tokens: Some(20),
                latency_ms: None,
                estimated_cost: None,
            },
            finish_reason: FinishReason::Completed,
            recorded_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn stores_small_body_untruncated() {
        let mut s = store();
        let a = s.store_response(b"hello", "text/plain", "worker-1").unwrap();
        assert!(!a.truncated);
        assert_eq!(a.size_bytes, 5);
        assert_eq!(a.original_size_bytes, 5);
        assert_eq!(a.body, b"hello");
        assert!(a.digest.starts_with("fnv1a64-test:"));
        assert_eq!(s.get(&a.digest).unwrap().body, b"hello");
    }

    #[test]
    fn truncates_over_cap_and_flags_it() {
        let mut s = store();
        let a = s
            .store_response(b"0123456789abcdefGHIJ", "text/plain", "w")
            .unwrap();
        assert!(a.truncated);
        assert_eq!(a.size_bytes, 16);
        assert_eq!(a.original_size_bytes, 20);
        assert_eq!(a.body, b"0123456789abcdef");
    }

    #[test]
    fn truncation_never_splits_utf8() {
        let mut s = store();
        // "é" is 2 bytes; cap 16 lands mid-character if naive.
        let body = "0123456789abcdeéFG".as_bytes(); // len 20
        let a = s.store_response(body, "text/plain", "w").unwrap();
        assert!(a.truncated);
        let text = std::str::from_utf8(&a.body).expect("must be valid UTF-8");
        assert!(text.ends_with('e')); // the é was dropped whole, not split
        assert_eq!(a.body.len(), 15);
    }

    #[test]
    fn record_attempt_binds_digests() {
        let mut s = store();
        let a = s.store_response(b"data", "application/json", "w").unwrap();
        s.record_attempt(meta(), vec![a.digest.clone()]).unwrap();
        let recs = s.attempts_for(&TaskId::new("T-1").unwrap());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].artifact_digests, vec![a.digest]);
        assert_eq!(recs[0].metadata.finish_reason, FinishReason::Completed);
        assert_eq!(recs[0].metadata.usage.input_tokens, Some(10));
        assert_eq!(recs[0].metadata.usage.latency_ms, None); // unknown stays unknown
    }

    #[test]
    fn record_attempt_rejects_unknown_digest() {
        let mut s = store();
        let err = s
            .record_attempt(meta(), vec!["sha256:deadbeef".to_string()])
            .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn record_attempt_rejects_zero_attempt() {
        let mut s = store();
        let mut m = meta();
        m.attempt = 0;
        let err = s.record_attempt(m, vec![]).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn zero_cap_rejected() {
        let err = ArtifactStore::new(StoreConfig {
            max_output_bytes: 0,
            digester: Box::new(TestDigest),
        })
        .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn artifact_ref_round_trip() {
        let mut s = store();
        let a = s.store_response(b"x", "text/plain", "w").unwrap();
        let r = a.as_ref().unwrap();
        assert_eq!(r.digest, a.digest);
        assert_eq!(r.size_bytes, 1);
    }

    #[test]
    fn usage_figures_serde_stable() {
        let u = UsageFigures {
            input_tokens: Some(1),
            output_tokens: None,
            latency_ms: Some(42),
            estimated_cost: Some("0.0012 USD".to_string()),
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: UsageFigures = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);
    }
}
