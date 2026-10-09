//! T069: idempotency keys and effect receipts.
//!
//! # Inference delivery semantics
//!
//! "Exactly once" is a property of the *effect*, not of the network. This
//! module gives each external side effect an idempotency key and records an
//! effect receipt, so retries and hedged duplicates collapse to a single
//! accepted effect. The guarantees differ per effect kind — they are stated
//! here, not assumed:
//!
//! | Effect kind     | Delivery        | Dedup mechanism                                             |
//! |-----------------|-----------------|-------------------------------------------------------------|
//! | `InferenceCall` | at-least-once   | Key checked before dispatch; only the first receipt's       |
//! |                 |                 | result is accepted. Retries and hedge legs (T065) may      |
//! |                 |                 | execute the model twice — the *effect* is still single.     |
//! | `ArtifactWrite` | at-most-once    | Content-addressed: same digest + same key is a no-op       |
//! |                 |                 | success. A different digest under the same key is a        |
//! |                 |                 | conflict and is rejected, never overwritten.                |
//! | `ExternalPost`  | at-most-once*   | Key checked before send, receipt recorded after. *Across   |
//! |                 | (intent)        | a crash between send and record, a duplicate send is       |
//! |                 |                 | possible; receivers MUST dedupe on the idempotency key,    |
//! |                 |                 | which is always included in the payload.                     |
//! | `LedgerAppend`  | at-least-once   | Receipts carry the key; replays dedupe on read. The       |
//! |                 | delivery,       | ledger itself stays append-only — duplicates are filtered  |
//! |                 | exactly-once    | at query time, never deleted.                               |
//! |                 | effect          |                                                             |
//!
//! Rules that follow from the table:
//! - A retry or hedge leg MUST reuse the attempt's key for the same effect;
//!   a new key means a new effect.
//! - `record` is called only after the effect actually happened. Recording
//!   before performing is forgery and is not possible through this API's
//!   intended flow (`check` -> perform -> `record`).
//! - Keys are derived deterministically (see [`IdempotencyLog::derive_key`])
//!   so a restarted dispatcher derives the same key for the same effect.

use fabric_types::{Result, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Opaque idempotency key. Non-empty, `[A-Za-z0-9-_.:]`, max 256 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if s.is_empty() || s.len() > 256 {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "idempotency_key",
                reason: "length must be 1..=256".to_string(),
            });
        }
        if !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')) {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "idempotency_key",
                reason: "charset is [A-Za-z0-9-_.:]".to_string(),
            });
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind of external side effect being guarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EffectKind {
    InferenceCall,
    ArtifactWrite,
    ExternalPost,
    LedgerAppend,
}

impl EffectKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::InferenceCall => "INFERENCE_CALL",
            Self::ArtifactWrite => "ARTIFACT_WRITE",
            Self::ExternalPost => "EXTERNAL_POST",
            Self::LedgerAppend => "LEDGER_APPEND",
        }
    }
}

/// Proof that an effect was performed exactly once under its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub key: IdempotencyKey,
    pub effect: EffectKind,
    pub task_id: TaskId,
    pub attempt: u32,
    /// Unix millis when the effect completed.
    pub performed_ms: u64,
    /// sha256 of the effect's result bytes.
    pub result_digest: String,
}

impl EffectReceipt {
    pub fn new(
        key: IdempotencyKey,
        effect: EffectKind,
        task_id: TaskId,
        attempt: u32,
        performed_ms: u64,
        result_digest: impl Into<String>,
    ) -> Result<Self> {
        if attempt == 0 {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "attempt",
                reason: "attempt numbers start at 1".to_string(),
            });
        }
        let result_digest = result_digest.into();
        if result_digest.is_empty() {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "result_digest",
                reason: "must be non-empty".to_string(),
            });
        }
        Ok(Self {
            key,
            effect,
            task_id,
            attempt,
            performed_ms,
            result_digest,
        })
    }
}

/// Outcome of consulting the idempotency log before acting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotentOutcome {
    /// Effect already performed: do NOT perform it again. The receipt is the
    /// authoritative result — reuse it.
    AlreadyPerformed(EffectReceipt),
    /// First time this key is seen: perform the effect, then `record`.
    FirstTime,
}

/// In-memory idempotency log. The durable version lives with the ledger
/// (later batch); this type owns the key-derivation and check/record
/// protocol so the durable port is mechanical.
#[derive(Debug, Default)]
pub struct IdempotencyLog {
    entries: HashMap<IdempotencyKey, EffectReceipt>,
}

impl IdempotencyLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive the canonical key for an effect: envelope digest + effect kind
    /// + attempt. Deterministic across restarts by construction.
    ///
    /// Fails if the envelope digest contains characters outside the key
    /// charset — callers must pass the real envelope digest, not free text.
    pub fn derive_key(
        envelope_digest: &str,
        effect: EffectKind,
        attempt: u32,
    ) -> Result<IdempotencyKey> {
        IdempotencyKey::new(format!("idem:{}:{}:{attempt}", effect.name(), envelope_digest))
    }

    /// Check whether the effect was already performed.
    pub fn check(&self, key: &IdempotencyKey) -> IdempotentOutcome {
        match self.entries.get(key) {
            Some(r) => IdempotentOutcome::AlreadyPerformed(r.clone()),
            None => IdempotentOutcome::FirstTime,
        }
    }

    /// Record a performed effect. A *different* result digest under an
    /// existing key is a conflict: the first write wins and the conflict is
    /// reported rather than silently overwritten.
    pub fn record(&mut self, receipt: EffectReceipt) -> Result<()> {
        if let Some(existing) = self.entries.get(&receipt.key) {
            if existing.result_digest != receipt.result_digest {
                return Err(fabric_types::FabricError::Conflict(format!(
                    "idempotency key {} already recorded with a different result digest",
                    receipt.key
                )));
            }
            return Ok(()); // same key + same digest: idempotent no-op
        }
        self.entries.insert(receipt.key.clone(), receipt);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> IdempotencyKey {
        IdempotencyKey::new(s).unwrap()
    }

    fn receipt(k: &str) -> EffectReceipt {
        EffectReceipt::new(
            key(k),
            EffectKind::InferenceCall,
            TaskId::new("T001").unwrap(),
            1,
            1000,
            "sha256:abc",
        )
        .unwrap()
    }

    #[test]
    fn check_before_record_is_first_time() {
        let mut log = IdempotencyLog::new();
        assert_eq!(log.check(&key("k1")), IdempotentOutcome::FirstTime);
        log.record(receipt("k1")).unwrap();
        match log.check(&key("k1")) {
            IdempotentOutcome::AlreadyPerformed(r) => assert_eq!(r.result_digest, "sha256:abc"),
            IdempotentOutcome::FirstTime => panic!("should be already performed"),
        }
    }

    #[test]
    fn duplicate_record_with_same_digest_is_noop() {
        let mut log = IdempotencyLog::new();
        log.record(receipt("k1")).unwrap();
        log.record(receipt("k1")).unwrap(); // idempotent
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn conflicting_digest_is_rejected() {
        let mut log = IdempotencyLog::new();
        log.record(receipt("k1")).unwrap();
        let mut other = receipt("k1");
        other.result_digest = "sha256:different".to_string();
        assert!(log.record(other).is_err());
    }

    #[test]
    fn derive_key_is_deterministic() {
        let a = IdempotencyLog::derive_key("deadbeef", EffectKind::ArtifactWrite, 2).unwrap();
        let b = IdempotencyLog::derive_key("deadbeef", EffectKind::ArtifactWrite, 2).unwrap();
        assert_eq!(a, b);
        let c = IdempotencyLog::derive_key("deadbeef", EffectKind::ArtifactWrite, 3).unwrap();
        assert_ne!(a, c);
        let d = IdempotencyLog::derive_key("deadbeef", EffectKind::InferenceCall, 2).unwrap();
        assert_ne!(a, d);
        // Caller-supplied garbage is rejected, never panicked on.
        assert!(IdempotencyLog::derive_key("not a digest!", EffectKind::InferenceCall, 1).is_err());
    }

    #[test]
    fn rejects_bad_keys_and_zero_attempt() {
        assert!(IdempotencyKey::new("").is_err());
        assert!(IdempotencyKey::new("has space").is_err());
        assert!(IdempotencyKey::new("ok:key-1.2_3").is_ok());
        assert!(EffectReceipt::new(
            key("k"),
            EffectKind::LedgerAppend,
            TaskId::new("T1").unwrap(),
            0,
            0,
            "sha256:x"
        )
        .is_err());
    }
}
