//! T082 — typed lifecycle events for the dispatch fabric.
//!
//! Every event carries a monotonic-friendly payload: `timestamp_ms` (Unix
//! milliseconds, supplied by the caller — this module never reads a clock),
//! `task_id`, `attempt` (1-based attempt number), and a typed `payload`
//! enum. Serde serialization is stable: field order is fixed by declaration
//! and the payload uses SCREAMING_SNAKE_CASE tags, so canonical JSON is
//! byte-stable across processes and runs.

use fabric_types::{TaskId, Verdict};
use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch. Always injected by the caller; logic
/// in this module never reads the system clock.
pub type UnixMillis = u64;

/// One typed lifecycle event. The `payload` selects which operation the
/// event records; `timestamp_ms`/`task_id`/`attempt` are the monotonic-friendly
/// envelope every payload shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FabricEvent {
    pub timestamp_ms: UnixMillis,
    pub task_id: TaskId,
    /// 1-based attempt number this event belongs to.
    pub attempt: u32,
    pub payload: EventPayload,
}

impl FabricEvent {
    /// Construct an event. `attempt` must be >= 1.
    pub fn new(
        timestamp_ms: UnixMillis,
        task_id: TaskId,
        attempt: u32,
        payload: EventPayload,
    ) -> Result<Self, fabric_types::FabricError> {
        if attempt < 1 {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "attempt",
                reason: "attempt must be >= 1".to_string(),
            });
        }
        Ok(Self {
            timestamp_ms,
            task_id,
            attempt,
            payload,
        })
    }

    /// Canonical JSON bytes of this event. Deterministic: serde derive
    /// emits fields in declaration order and the payload tag is fixed.
    pub fn canonical_json(&self) -> Vec<u8> {
        // Serialization of these types cannot fail: no maps, no custom impls.
        serde_json::to_vec(self).expect("FabricEvent serialization is infallible")
    }
}

/// Typed payloads for the nine lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventPayload {
    /// A prompt was rendered and pinned for an attempt.
    Compile {
        /// Content digest of the exact rendered prompt bytes.
        prompt_digest: String,
    },
    /// The task was admitted to a scheduling queue.
    Schedule {
        /// Endpoint the task was queued for, if routing is decided.
        endpoint: Option<String>,
    },
    /// The task was dispatched to a worker.
    Dispatch {
        /// Digest of the dispatch envelope (see fabric-dispatch).
        envelope_digest: String,
        /// Workspace the worker is confined to.
        workspace: String,
        /// The contract's declared failure-handling path, if any.
        /// Advisory: recorded in the tamper-evident log at dispatch.
        /// The driver does not execute it — no action language exists
        /// for it (DS1-005); the driver's own failure handling
        /// (quarantine, retry, retire) applies.
        failure_path: Option<String>,
    },
    /// A worker response arrived (before acceptance — not proof of success).
    Response {
        /// Content digest of the raw response bytes.
        response_digest: String,
        /// Content digest of the candidate artifact, if one was produced.
        artifact_digest: Option<String>,
    },
    /// A verifier verdict was recorded.
    Verify { verdict: Verdict },
    /// A retry was scheduled for a later attempt.
    Retry {
        /// The attempt number this retry will be.
        next_attempt: u32,
        /// Earliest dispatch time for the retry (injectable clock domain).
        not_before_ms: UnixMillis,
        /// Why the retry was granted.
        reason: String,
    },
    /// An artifact was accepted into the integrated tree.
    Integrate {
        /// Content digest of the accepted artifact.
        artifact_digest: String,
        /// Commit the artifact was integrated from.
        commit: String,
    },
    /// The task was denied (quota, policy, budget, or fairness).
    Deny {
        /// Machine-readable reason code.
        reason: String,
    },
    /// A worker receipt was rejected before gating (malformed JSON,
    /// digest-linkage mismatch, or over the size cap). The task is
    /// quarantined — its gate fails and it may retry within policy —
    /// but the run continues. The quarantine is a durable state
    /// transition, so it gets a chained record (DS1-004).
    ReceiptRejected {
        /// Human-readable cause naming the offending receipt file.
        cause: String,
    },
    /// A reservation or workspace was released without dispatch.
    Release {
        /// What was released, as a machine-readable code.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(ts: u64, attempt: u32, payload: EventPayload) -> FabricEvent {
        FabricEvent::new(ts, TaskId::new("T001").unwrap(), attempt, payload).unwrap()
    }

    #[test]
    fn rejects_attempt_zero() {
        let err = FabricEvent::new(0, TaskId::new("T001").unwrap(), 0, EventPayload::Release {
            reason: "x".to_string(),
        });
        assert!(err.is_err());
    }

    #[test]
    fn canonical_json_is_stable_across_processes() {
        let e1 = event(1234, 1, EventPayload::Verify { verdict: Verdict::Pass });
        let e2 = event(1234, 1, EventPayload::Verify { verdict: Verdict::Pass });
        assert_eq!(e1.canonical_json(), e2.canonical_json());
    }

    #[test]
    fn payload_tag_uses_screaming_snake_case() {
        let e = event(1, 1, EventPayload::Retry {
            next_attempt: 2,
            not_before_ms: 99,
            reason: "timeout".to_string(),
        });
        let v: serde_json::Value = serde_json::from_slice(&e.canonical_json()).unwrap();
        assert_eq!(v["payload"]["op"], "RETRY");
    }

    #[test]
    fn serde_roundtrip_for_all_payloads() {
        let payloads = vec![
            EventPayload::Compile { prompt_digest: "sha256:abc".to_string() },
            EventPayload::Schedule { endpoint: Some("ep-1".to_string()) },
            EventPayload::Schedule { endpoint: None },
            EventPayload::Dispatch {
                envelope_digest: "sha256:def".to_string(),
                workspace: "ws-1".to_string(),
                failure_path: Some("quarantine artifacts".to_string()),
            },
            EventPayload::Response {
                response_digest: "sha256:000".to_string(),
                artifact_digest: Some("sha256:111".to_string()),
            },
            EventPayload::Response {
                response_digest: "sha256:000".to_string(),
                artifact_digest: None,
            },
            EventPayload::Verify { verdict: Verdict::Pass },
            EventPayload::Retry {
                next_attempt: 2,
                not_before_ms: 5000,
                reason: "timeout".to_string(),
            },
            EventPayload::Integrate {
                artifact_digest: "sha256:222".to_string(),
                commit: "deadbeef".to_string(),
            },
            EventPayload::Deny { reason: "QUOTA_EXHAUSTED".to_string() },
            EventPayload::ReceiptRejected {
                cause: "receipt abc123.receipt.json fails to parse: bad json".to_string(),
            },
            EventPayload::Release { reason: "superseded".to_string() },
        ];
        for p in payloads {
            let e = event(7, 3, p);
            let json = e.canonical_json();
            let back: FabricEvent = serde_json::from_slice(&json).unwrap();
            assert_eq!(e, back);
            assert_eq!(back.timestamp_ms, 7);
            assert_eq!(back.attempt, 3);
            assert_eq!(back.task_id.as_str(), "T001");
        }
    }
}
