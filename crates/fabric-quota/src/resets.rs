//! T046: quota reset windows and Retry-After modeling.
//!
//! Models *when* provider quota may become available again. The cardinal rule:
//! never assume universal reset behavior. Every variant of [`ResetModel`] is
//! either explicitly configured or directly observed. An expired or absent
//! hint yields `None` (unknown) — never an invented reset time.

use crate::UnixMillis;
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// How we believe a provider's quota resets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetModel {
    /// No reset information. Never infer a reset time from this.
    Unknown,
    /// Quota resets on a fixed cadence anchored at a known instant.
    /// Must be configured explicitly (e.g. from provider documentation);
    /// never assumed.
    FixedWindow {
        window_ms: u64,
        anchor_ms: UnixMillis,
    },
    /// A `Retry-After` hint observed at `observed_at_ms`. Applies once: after
    /// it elapses the model returns to [`ResetModel::Unknown`] rather than
    /// assuming recovery.
    RetryAfterHint {
        retry_after_ms: u64,
        observed_at_ms: UnixMillis,
    },
    /// The provider explicitly reported the next reset instant.
    ProviderReported { resets_at_ms: UnixMillis },
}

/// Tracks one endpoint's reset model. All clocks are injected.
#[derive(Debug, Clone)]
pub struct ResetTracker {
    model: ResetModel,
}

impl ResetTracker {
    pub fn new() -> Self {
        Self {
            model: ResetModel::Unknown,
        }
    }

    pub fn model(&self) -> &ResetModel {
        &self.model
    }

    /// Configure a fixed reset cadence. `window_ms` must be positive.
    pub fn configure_fixed_window(
        &mut self,
        window_ms: u64,
        anchor_ms: UnixMillis,
    ) -> Result<()> {
        if window_ms == 0 {
            return Err(FabricError::InvalidInput {
                field: "window_ms",
                reason: "reset window must be positive".to_string(),
            });
        }
        self.model = ResetModel::FixedWindow {
            window_ms,
            anchor_ms,
        };
        Ok(())
    }

    /// Record a `Retry-After` hint (milliseconds) observed at `now`.
    pub fn observe_retry_after(&mut self, retry_after_ms: u64, now: UnixMillis) {
        self.model = ResetModel::RetryAfterHint {
            retry_after_ms,
            observed_at_ms: now,
        };
    }

    /// Record a provider-reported next-reset instant.
    pub fn observe_reset_at(&mut self, resets_at_ms: UnixMillis) {
        self.model = ResetModel::ProviderReported { resets_at_ms };
    }

    /// Forget all reset information.
    pub fn clear(&mut self) {
        self.model = ResetModel::Unknown;
    }

    /// Earliest instant we believe quota *may* be available again.
    /// `None` means unknown — callers must not assume headroom.
    pub fn blocked_until(&self, now: UnixMillis) -> Option<UnixMillis> {
        match &self.model {
            ResetModel::Unknown => None,
            ResetModel::FixedWindow {
                window_ms,
                anchor_ms,
            } => {
                if now < *anchor_ms {
                    return Some(*anchor_ms);
                }
                let elapsed = now.saturating_sub(*anchor_ms);
                let periods = elapsed / window_ms + 1;
                Some(anchor_ms.saturating_add(periods.saturating_mul(*window_ms)))
            }
            ResetModel::RetryAfterHint {
                retry_after_ms,
                observed_at_ms,
            } => {
                let until = observed_at_ms.saturating_add(*retry_after_ms);
                // Expired hint -> unknown, never "recovered".
                if now < until {
                    Some(until)
                } else {
                    None
                }
            }
            ResetModel::ProviderReported { resets_at_ms } => {
                if now < *resets_at_ms {
                    Some(*resets_at_ms)
                } else {
                    None
                }
            }
        }
    }

    /// True when no *known* reset information blocks an attempt right now.
    /// Unknown never blocks by itself; admission control is the token
    /// bucket's job, and the reason is recorded by the decision log.
    pub fn may_attempt(&self, now: UnixMillis) -> bool {
        self.blocked_until(now).is_none()
    }
}

impl Default for ResetTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_never_blocks_and_never_invents() {
        let t = ResetTracker::new();
        assert_eq!(t.blocked_until(1_000_000), None);
        assert!(t.may_attempt(1_000_000));
    }

    #[test]
    fn fixed_window_computes_next_boundary() {
        let mut t = ResetTracker::new();
        t.configure_fixed_window(60_000, 0).unwrap();
        // now=90_000 -> next boundary at 120_000
        assert_eq!(t.blocked_until(90_000), Some(120_000));
        assert!(!t.may_attempt(90_000));
        // exactly on a boundary -> next one
        assert_eq!(t.blocked_until(60_000), Some(120_000));
        // before the anchor -> the anchor itself
        assert_eq!(t.blocked_until(0), Some(60_000));
    }

    #[test]
    fn retry_after_expires_to_unknown_not_recovered() {
        let mut t = ResetTracker::new();
        t.observe_retry_after(5_000, 100_000);
        assert_eq!(t.blocked_until(102_000), Some(105_000));
        assert!(!t.may_attempt(102_000));
        // after the hint elapses: unknown, not "recovered"
        assert_eq!(t.blocked_until(106_000), None);
        assert!(t.may_attempt(106_000));
    }

    #[test]
    fn provider_reported_reset() {
        let mut t = ResetTracker::new();
        t.observe_reset_at(500_000);
        assert_eq!(t.blocked_until(400_000), Some(500_000));
        assert_eq!(t.blocked_until(500_000), None);
    }

    #[test]
    fn zero_window_rejected() {
        let mut t = ResetTracker::new();
        assert!(t.configure_fixed_window(0, 0).is_err());
    }

    #[test]
    fn model_round_trips() {
        let mut t = ResetTracker::new();
        t.configure_fixed_window(60_000, 10).unwrap();
        let j = serde_json::to_string(t.model()).unwrap();
        let back: ResetModel = serde_json::from_str(&j).unwrap();
        assert_eq!(*t.model(), back);
    }
}
