//! T067: track in-flight attempt progress.
//!
//! Workers may report progress while an attempt runs. These reports are
//! **informational only**: they drive dashboards, stall detection, and
//! operator visibility — they are never evidence. Only a verifier's verdict
//! (see the verification engine) can settle an attempt. A worker claiming
//! "100% done" in a progress report proves nothing; the attempt is complete
//! when its acceptance predicates are verified, not when the worker says so.
//!
//! Trust model:
//! - `message` and `reported_ms` are worker claims: untrusted.
//! - `received_ms` is stamped by the tracker on receipt: trusted.
//! - Stale reports make an attempt a *candidate* for orphan investigation
//!   (T068); staleness is not failure.

use fabric_types::{Result, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Identity of one attempt: task plus 1-based attempt number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttemptKey {
    pub task_id: TaskId,
    pub attempt: u32,
}

impl AttemptKey {
    pub fn new(task_id: TaskId, attempt: u32) -> Result<Self> {
        if attempt == 0 {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "attempt",
                reason: "attempt numbers start at 1".to_string(),
            });
        }
        Ok(Self { task_id, attempt })
    }
}

/// One worker progress claim. Informational, not evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub key: AttemptKey,
    /// 0..=100 when the worker quantifies progress; None otherwise.
    pub percent: Option<u8>,
    /// Free-text claim from the worker. Untrusted.
    pub message: String,
    /// Worker's own timestamp. Untrusted.
    pub reported_ms: u64,
    /// Tracker-stamped receipt time. Trusted.
    pub received_ms: u64,
}

impl ProgressUpdate {
    pub fn new(
        key: AttemptKey,
        percent: Option<u8>,
        message: impl Into<String>,
        reported_ms: u64,
        received_ms: u64,
    ) -> Result<Self> {
        if percent.is_some_and(|p| p > 100) {
            return Err(fabric_types::FabricError::InvalidInput {
                field: "percent",
                reason: "must be 0..=100".to_string(),
            });
        }
        Ok(Self {
            key,
            percent,
            message: message.into(),
            reported_ms,
            received_ms,
        })
    }
}

/// In-flight progress board. Keeps the latest claim per attempt.
#[derive(Debug, Default)]
pub struct ProgressTracker {
    latest: HashMap<AttemptKey, ProgressUpdate>,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a worker progress claim. Later claims for the same attempt
    /// replace earlier ones. This records a claim; it proves nothing.
    pub fn report(&mut self, update: ProgressUpdate) {
        self.latest.insert(update.key.clone(), update);
    }

    /// Latest claim for an attempt, if any.
    pub fn latest(&self, key: &AttemptKey) -> Option<&ProgressUpdate> {
        self.latest.get(key)
    }

    /// Number of attempts currently tracked.
    pub fn in_flight(&self) -> usize {
        self.latest.len()
    }

    /// Attempts whose last trusted receipt timestamp is older than
    /// `stale_after_ms` relative to `now_ms`. These are candidates for orphan
    /// investigation — staleness is a signal to look, not a verdict.
    pub fn stale(&self, now_ms: u64, stale_after_ms: u64) -> Vec<AttemptKey> {
        self.latest
            .iter()
            .filter(|(_, u)| now_ms.saturating_sub(u.received_ms) > stale_after_ms)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Drop tracking for an attempt (called when it reaches a terminal
    /// verdict). Progress history is not evidence and is not retained here;
    /// the evidence receipt carries what matters.
    pub fn forget(&mut self, key: &AttemptKey) -> bool {
        self.latest.remove(key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AttemptKey {
        AttemptKey::new(TaskId::new("T001").unwrap(), 1).unwrap()
    }

    #[test]
    fn report_and_latest() {
        let mut t = ProgressTracker::new();
        let u = ProgressUpdate::new(key(), Some(50), "halfway", 1000, 1005).unwrap();
        t.report(u.clone());
        assert_eq!(t.latest(&key()), Some(&u));
        assert_eq!(t.in_flight(), 1);
    }

    #[test]
    fn later_report_replaces_earlier() {
        let mut t = ProgressTracker::new();
        t.report(ProgressUpdate::new(key(), Some(10), "start", 1000, 1001).unwrap());
        t.report(ProgressUpdate::new(key(), Some(90), "almost", 2000, 2001).unwrap());
        assert_eq!(t.latest(&key()).unwrap().percent, Some(90));
        assert_eq!(t.in_flight(), 1);
    }

    #[test]
    fn rejects_bad_percent_and_zero_attempt() {
        assert!(ProgressUpdate::new(key(), Some(101), "x", 0, 0).is_err());
        assert!(AttemptKey::new(TaskId::new("T1").unwrap(), 0).is_err());
    }

    #[test]
    fn stale_detection_uses_trusted_timestamp() {
        let mut t = ProgressTracker::new();
        // Worker claims a fresh timestamp, but we received it long ago.
        t.report(ProgressUpdate::new(key(), None, "lying", u64::MAX, 100).unwrap());
        assert_eq!(t.stale(10_000, 1_000), vec![key()]);
        assert!(t.stale(500, 1_000).is_empty());
    }

    #[test]
    fn forget_drops_tracking() {
        let mut t = ProgressTracker::new();
        t.report(ProgressUpdate::new(key(), None, "x", 0, 0).unwrap());
        assert!(t.forget(&key()));
        assert!(!t.forget(&key()));
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn progress_claim_is_not_a_verdict() {
        // Even 100% is still just a claim: the tracker exposes no verdict API.
        let mut t = ProgressTracker::new();
        t.report(ProgressUpdate::new(key(), Some(100), "done done done", 0, 0).unwrap());
        let u = t.latest(&key()).unwrap();
        assert_eq!(u.percent, Some(100));
        // ...and the attempt is still merely "in flight" until verified.
        assert_eq!(t.in_flight(), 1);
    }
}
