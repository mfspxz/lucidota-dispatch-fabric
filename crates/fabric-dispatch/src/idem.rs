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
//! | `ExternalPost`  | at-most-once*   | GUARDED path: `reserve` (durable, fsync'd via
//! |                 | (guarded)       | [`DurableEffectLog`]) BEFORE send; `confirm` after.
//! |                 |                 | A crash between send and confirm leaves UNKNOWN,
//! |                 |                 | never a silent gap: restart reconciles, it does
//! |                 |                 | NOT re-send. *The UNGUARDED check -> record path
//! |                 |                 | still carries the old caveat: a crash between
//! |                 |                 | send and record can duplicate, and receivers
//! |                 |                 | MUST dedupe on the idempotency key, which is
//! |                 |                 | always included in the payload.                     |
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
    /// Intent for this key was reserved durably (see [`IdempotencyLog::reserve`])
    /// but its outcome is UNKNOWN: the effect may or may not have happened
    /// (e.g. the process crashed between send and confirm). This is NEITHER
    /// `FirstTime` NOR `AlreadyPerformed`: the caller MUST reconcile
    /// (see [`IdempotencyLog::confirm`]) and MUST NOT blindly perform the
    /// effect again. Treating UNKNOWN as `FirstTime` is exactly the
    /// duplicate-send bug this module exists to prevent.
    ReservedUnknown { reserved_ms: u64 },
}

/// Durable state of one guarded effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectState {
    /// Intent reserved, outcome unknown. Reserved BEFORE the outbound
    /// effect is attempted, so a crash after the send still leaves a
    /// durable UNKNOWN instead of a silent gap.
    Unknown { reserved_ms: u64 },
    /// Effect confirmed performed, with its receipt.
    Proven(EffectReceipt),
}

/// In-memory idempotency log. The durable version is [`DurableEffectLog`]
/// below (same protocol, fsync'd append-only JSONL); this type owns the
/// key-derivation and check/record/reserve/confirm protocol.
#[derive(Debug, Default)]
pub struct IdempotencyLog {
    entries: HashMap<IdempotencyKey, EffectState>,
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
    ///
    /// UNKNOWN keys report [`IdempotentOutcome::ReservedUnknown`], never
    /// `FirstTime`: a caller that retries on `FirstTime` must not be
    /// handed an uncertain effect.
    pub fn check(&self, key: &IdempotencyKey) -> IdempotentOutcome {
        match self.entries.get(key) {
            Some(EffectState::Proven(r)) => IdempotentOutcome::AlreadyPerformed(r.clone()),
            Some(EffectState::Unknown { reserved_ms }) => {
                IdempotentOutcome::ReservedUnknown {
                    reserved_ms: *reserved_ms,
                }
            }
            None => IdempotentOutcome::FirstTime,
        }
    }

    /// Reserve intent for an effect BEFORE attempting it. This is the
    /// guarded path for effects whose outcome can become uncertain
    /// (`ExternalPost` especially; see the module table):
    ///
    /// ```text
    /// reserve(key) -> FirstTime  ... perform the effect ...
    /// confirm(key, receipt)     ... outcome now PROVEN
    /// // crash anywhere between reserve and confirm:
    /// check(key) == ReservedUnknown -> RECONCILE, never re-perform blindly
    /// ```
    ///
    /// Re-reserving an UNKNOWN key is an idempotent no-op returning
    /// `ReservedUnknown` (the original reservation stands); reserving a
    /// PROVEN key returns `AlreadyPerformed`. Reservation never overwrites.
    pub fn reserve(
        &mut self,
        key: IdempotencyKey,
        reserved_ms: u64,
    ) -> Result<IdempotentOutcome> {
        match self.entries.get(&key) {
            None => {
                self.entries
                    .insert(key, EffectState::Unknown { reserved_ms });
                Ok(IdempotentOutcome::FirstTime)
            }
            Some(EffectState::Unknown { reserved_ms }) => Ok(
                IdempotentOutcome::ReservedUnknown {
                    reserved_ms: *reserved_ms,
                },
            ),
            Some(EffectState::Proven(r)) => {
                Ok(IdempotentOutcome::AlreadyPerformed(r.clone()))
            }
        }
    }

    /// Reconcile an UNKNOWN effect after a crash (or after any uncertain
    /// completion): transition UNKNOWN -> PROVEN with the provider's
    /// receipt. This is the ONLY way an UNKNOWN effect leaves UNKNOWN,
    /// and it requires evidence (the receipt digest), not a re-execution.
    ///
    /// Errors: unreserved key (reserve first — confirming an effect that
    /// was never reserved is forgery-shaped and refused); a different
    /// digest under an already-PROVEN key (conflict, first write wins).
    /// Same digest under a PROVEN key is an idempotent no-op.
    pub fn confirm(&mut self, receipt: EffectReceipt) -> Result<()> {
        match self.entries.get(&receipt.key) {
            None => Err(fabric_types::FabricError::InvalidInput {
                field: "idempotency_key",
                reason: "cannot confirm an effect that was never reserved".to_string(),
            }),
            Some(EffectState::Unknown { .. }) => {
                self.entries
                    .insert(receipt.key.clone(), EffectState::Proven(receipt));
                Ok(())
            }
            Some(EffectState::Proven(existing)) => {
                if existing.result_digest != receipt.result_digest {
                    return Err(fabric_types::FabricError::Conflict(format!(
                        "idempotency key {} already recorded with a different result digest",
                        receipt.key
                    )));
                }
                Ok(())
            }
        }
    }

    /// Record a performed effect. A *different* result digest under an
    /// existing key is a conflict: the first write wins and the conflict is
    /// reported rather than silently overwritten. Recording over an
    /// UNKNOWN reservation is the same transition as [`Self::confirm`].
    pub fn record(&mut self, receipt: EffectReceipt) -> Result<()> {
        if let Some(existing) = self.entries.get(&receipt.key) {
            match existing {
                EffectState::Proven(prev) if prev.result_digest != receipt.result_digest => {
                    return Err(fabric_types::FabricError::Conflict(format!(
                        "idempotency key {} already recorded with a different result digest",
                        receipt.key
                    )));
                }
                EffectState::Proven(_) => return Ok(()), // same digest: no-op
                EffectState::Unknown { .. } => {}        // fall through: prove it
            }
        }
        self.entries
            .insert(receipt.key.clone(), EffectState::Proven(receipt));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One record of the durable effect-state file. JSONL, one record per
/// line, appended and fsync'd BEFORE the corresponding in-memory
/// transition is reported as done — so a returned reservation is durable
/// against process crash (machine-crash safety is modulo the OS/storage
/// stack honoring fsync, same bound as the host receipt log).
///
/// This file is the SOLE effect-state truth for a deployment: the host
/// receipt chain (ORL1) remains the sole receipt truth, and the two are
/// correlated by idempotency key, never duplicated. The host-side reader
/// of this same schema lives in `organelle-host::receipt_log`
/// (`EffectStateLog`); both sides replay the same file, they are not two
/// independent stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum EffectRecord {
    Reserve {
        key: IdempotencyKey,
        effect: EffectKind,
        task_id: TaskId,
        attempt: u32,
        reserved_ms: u64,
    },
    Confirm {
        key: IdempotencyKey,
        receipt: EffectReceipt,
        /// Provider-side id for this effect, when the provider supplies
        /// one (correlation/audit metadata; the conflict decision stays
        /// digest-based, matching [`IdempotencyLog::confirm`]).
        provider_id: Option<String>,
    },
}

/// File-backed [`IdempotencyLog`]: every `reserve`/`confirm` is appended
/// to an fsync'd JSONL file before it takes effect in memory.
///
/// Crash semantics (mirroring the host receipt log's torn-tail rule):
/// - A torn FINAL line (crash mid-append) is truncated on reopen: only
///   fully-written records count. Under the guarded protocol this is
///   safe — a torn reservation means the send must not have happened,
///   because callers send only after a durable reserve returns `Ok`.
/// - A complete line that does not parse, or a record that violates the
///   protocol on replay (e.g. confirming a key never reserved in the
///   file), fails the open: corruption fails closed, never silently.
pub struct DurableEffectLog {
    inner: IdempotencyLog,
    file: std::fs::File,
    pub path: std::path::PathBuf,
}

impl DurableEffectLog {
    /// Open (creating) the effect-state file at `path`, replay it, and
    /// leave it positioned for appends.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|e| fabric_types::FabricError::TransportFailure(format!(
                "effect log open {}: {e}",
                path.display()
            )))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| {
            fabric_types::FabricError::TransportFailure(format!(
                "effect log read {}: {e}",
                path.display()
            ))
        })?;
        let mut inner = IdempotencyLog::new();
        let mut offset = 0usize; // byte offset of the current line's start
        let mut good_end = 0usize; // offset just past the last good line
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            let line_start = offset;
            offset += line.len();
            let text = match std::str::from_utf8(line) {
                Ok(t) => t.trim_end_matches(['\n', '\r']),
                Err(_) => {
                    // Non-UTF8 tail can only be a torn append: truncate.
                    break;
                }
            };
            if text.is_empty() {
                good_end = offset;
                continue;
            }
            let rec: EffectRecord = match serde_json::from_str(text) {
                Ok(r) => r,
                Err(_) => {
                    // A torn append has no trailing newline (crash
                    // mid-write): truncate it. A newline-terminated line
                    // that still fails to parse is a COMPLETE record that
                    // is corrupt — fail closed, wherever it sits.
                    let is_torn_tail =
                        offset >= bytes.len() && !line.ends_with(b"\n");
                    if is_torn_tail {
                        break;
                    }
                    return Err(fabric_types::FabricError::InvalidInput {
                        field: "effect_log",
                        reason: format!(
                            "unparseable effect record at byte {line_start}"
                        ),
                    });
                }
            };
            match rec {
                EffectRecord::Reserve {
                    key,
                    reserved_ms,
                    ..
                } => {
                    // Replay must not overwrite: a reserve replayed over
                    // an already-PROVEN key would rewind state. Replays
                    // only ever see reserve-before-confirm in file order,
                    // so a PROVEN key here means a corrupt file.
                    match inner.check(&key) {
                        IdempotentOutcome::AlreadyPerformed(_) => {
                            return Err(fabric_types::FabricError::InvalidInput {
                                field: "effect_log",
                                reason: format!(
                                    "reserve replay for already-proven key {key}"
                                ),
                            });
                        }
                        _ => {
                            inner.reserve(key, reserved_ms)?;
                        }
                    }
                }
                EffectRecord::Confirm { receipt, .. } => {
                    inner.confirm(receipt)?;
                }
            }
            good_end = offset;
        }
        if good_end < bytes.len() {
            // Torn tail: cut it, fsync the truncation, keep the good prefix.
            file.set_len(good_end as u64).map_err(|e| {
                fabric_types::FabricError::TransportFailure(format!(
                    "effect log truncate {}: {e}",
                    path.display()
                ))
            })?;
            file.sync_all().map_err(|e| {
                fabric_types::FabricError::TransportFailure(format!(
                    "effect log fsync after truncate {}: {e}",
                    path.display()
                ))
            })?;
        }
        file.seek(SeekFrom::End(0)).map_err(|e| {
            fabric_types::FabricError::TransportFailure(format!(
                "effect log seek {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self {
            inner,
            file,
            path: path.to_path_buf(),
        })
    }

    /// Durably reserve intent BEFORE attempting the effect. Returns
    /// `FirstTime` only when this reservation is new; re-reserving an
    /// UNKNOWN key returns `ReservedUnknown` without a new file record
    /// (the original reservation is already durable).
    pub fn reserve(
        &mut self,
        key: IdempotencyKey,
        effect: EffectKind,
        task_id: TaskId,
        attempt: u32,
        reserved_ms: u64,
    ) -> Result<IdempotentOutcome> {
        use std::io::Write;
        match self.inner.check(&key) {
            IdempotentOutcome::FirstTime => {
                let rec = EffectRecord::Reserve {
                    key: key.clone(),
                    effect,
                    task_id,
                    attempt,
                    reserved_ms,
                };
                let mut line = serde_json::to_vec(&rec).map_err(|e| {
                    fabric_types::FabricError::InvalidInput {
                        field: "effect_log",
                        reason: format!("serialize reserve: {e}"),
                    }
                })?;
                line.push(b'\n');
                self.file.write_all(&line).map_err(|e| {
                    fabric_types::FabricError::TransportFailure(format!(
                        "effect log write {}: {e}",
                        self.path.display()
                    ))
                })?;
                self.file.sync_all().map_err(|e| {
                    fabric_types::FabricError::TransportFailure(format!(
                        "effect log fsync {}: {e}",
                        self.path.display()
                    ))
                })?;
                self.inner.reserve(key, reserved_ms)
            }
            other => Ok(other),
        }
    }

    /// Reconcile an UNKNOWN effect with the provider's receipt (and, when
    /// available, its provider-side id). Durably appended + fsync'd before
    /// the in-memory transition. Refuses unreserved keys and conflicting
    /// digests, exactly like [`IdempotencyLog::confirm`].
    pub fn confirm(
        &mut self,
        receipt: EffectReceipt,
        provider_id: Option<String>,
    ) -> Result<()> {
        use std::io::Write;
        // Validate first, write only on a transition (or idempotent no-op
        // that needs no record).
        match self.inner.check(&receipt.key) {
            IdempotentOutcome::AlreadyPerformed(prev)
                if prev.result_digest == receipt.result_digest =>
            {
                return Ok(());
            }
            IdempotentOutcome::FirstTime => {
                return Err(fabric_types::FabricError::InvalidInput {
                    field: "idempotency_key",
                    reason: "cannot confirm an effect that was never reserved".to_string(),
                });
            }
            IdempotentOutcome::AlreadyPerformed(_) => {
                return Err(fabric_types::FabricError::Conflict(format!(
                    "idempotency key {} already recorded with a different result digest",
                    receipt.key
                )));
            }
            IdempotentOutcome::ReservedUnknown { .. } => {}
        }
        let rec = EffectRecord::Confirm {
            key: receipt.key.clone(),
            receipt: receipt.clone(),
            provider_id,
        };
        let mut line = serde_json::to_vec(&rec).map_err(|e| {
            fabric_types::FabricError::InvalidInput {
                field: "effect_log",
                reason: format!("serialize confirm: {e}"),
            }
        })?;
        line.push(b'\n');
        self.file.write_all(&line).map_err(|e| {
            fabric_types::FabricError::TransportFailure(format!(
                "effect log write {}: {e}",
                self.path.display()
            ))
        })?;
        self.file.sync_all().map_err(|e| {
            fabric_types::FabricError::TransportFailure(format!(
                "effect log fsync {}: {e}",
                self.path.display()
            ))
        })?;
        self.inner.confirm(receipt)
    }

    /// Consult the recovered state without mutating it.
    pub fn check(&self, key: &IdempotencyKey) -> IdempotentOutcome {
        self.inner.check(key)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
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
            IdempotentOutcome::ReservedUnknown { .. } => {
                panic!("should be performed, not unknown")
            }
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
    fn reserved_unknown_is_neither_first_time_nor_performed() {
        let mut log = IdempotencyLog::new();
        let k = key("post:1");
        assert_eq!(log.reserve(k.clone(), 1000).unwrap(), IdempotentOutcome::FirstTime);
        // Second reserve does NOT re-arm the effect: still UNKNOWN.
        assert_eq!(
            log.reserve(k.clone(), 1001).unwrap(),
            IdempotentOutcome::ReservedUnknown { reserved_ms: 1000 }
        );
        assert_eq!(
            log.check(&k),
            IdempotentOutcome::ReservedUnknown { reserved_ms: 1000 }
        );
        // Confirming without a reservation is refused.
        assert!(log.confirm(receipt("never-reserved")).is_err());
        // Reconcile with the provider's receipt: UNKNOWN -> PROVEN.
        let mut r = receipt("post:1");
        r.effect = EffectKind::ExternalPost;
        log.confirm(r.clone()).unwrap();
        match log.check(&k) {
            IdempotentOutcome::AlreadyPerformed(got) => {
                assert_eq!(got.result_digest, r.result_digest)
            }
            other => panic!("expected AlreadyPerformed, got {other:?}"),
        }
        // A blind retry with a different digest is a conflict, not a send.
        let mut conflicting = r.clone();
        conflicting.result_digest = "sha256:other".to_string();
        assert!(log.confirm(conflicting).is_err());
    }

    #[test]
    fn durable_crash_recovery_reconciles_instead_of_repeating() {
        // REQUIRED P0 test: an external operation succeeds immediately
        // before a simulated crash; after restart the system must
        // reconcile, never blindly repeat.
        let dir = std::env::temp_dir().join(format!(
            "lucidota-effect-log-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("effects.jsonl");
        let k = key("ext:send-123");
        let mut receipt = receipt("ext:send-123");
        receipt.effect = EffectKind::ExternalPost;
        {
            let mut log = DurableEffectLog::open(&path).unwrap();
            // 1. Reserve durably BEFORE the outbound effect.
            assert_eq!(
                log.reserve(
                    k.clone(),
                    EffectKind::ExternalPost,
                    TaskId::new("T001").unwrap(),
                    1,
                    1000
                )
                .unwrap(),
                IdempotentOutcome::FirstTime
            );
            // 2. The provider SEND SUCCEEDS here (outside this log).
            // 3. SIMULATED CRASH: the log is dropped without confirm.
            //    No confirm record exists; the reservation is durable.
        }
        // 4. RESTART: reopen the same file.
        let mut log = DurableEffectLog::open(&path).unwrap();
        // 5. The effect is UNKNOWN — crucially NOT FirstTime. A caller
        //    that re-executes on FirstTime would send twice.
        assert_eq!(
            log.check(&k),
            IdempotentOutcome::ReservedUnknown { reserved_ms: 1000 }
        );
        // 6. RECONCILE with the provider's receipt (provider id supplied),
        //    instead of repeating the send.
        log.confirm(receipt.clone(), Some("provider:msg-123".to_string()))
            .unwrap();
        match log.check(&k) {
            IdempotentOutcome::AlreadyPerformed(got) => {
                assert_eq!(got.result_digest, receipt.result_digest)
            }
            other => panic!("expected AlreadyPerformed after reconcile, got {other:?}"),
        }
        // 7. A blind retry with a conflicting result is refused.
        let mut conflicting = receipt.clone();
        conflicting.result_digest = "sha256:second-send".to_string();
        assert!(log
            .confirm(conflicting, Some("provider:msg-999".to_string()))
            .is_err());
        // Durability evidence: reserve + confirm records are in the file.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"reserve\""), "reserve record missing");
        assert!(contents.contains("\"confirm\""), "confirm record missing");
        assert!(contents.contains("provider:msg-123"), "provider id missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durable_torn_tail_is_truncated_on_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "lucidota-effect-torn-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("effects.jsonl");
        {
            let mut log = DurableEffectLog::open(&path).unwrap();
            log.reserve(
                key("torn:1"),
                EffectKind::ExternalPost,
                TaskId::new("T001").unwrap(),
                1,
                1000,
            )
            .unwrap();
        }
        // Simulate crash mid-append: partial JSON, no trailing newline.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"op\":\"reserve\",\"key\":\"tor").unwrap();
            f.sync_all().unwrap();
        }
        let log = DurableEffectLog::open(&path).unwrap();
        assert_eq!(
            log.check(&key("torn:1")),
            IdempotentOutcome::ReservedUnknown { reserved_ms: 1000 }
        );
        assert_eq!(log.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
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
