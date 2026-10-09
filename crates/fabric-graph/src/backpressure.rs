//! T057: bounded queue growth with deterministic backpressure.
//!
//! The queue has a hard cap (`max_queued`). Admission is a pure function of
//! the current depth: at or above the cap, new submissions are rejected —
//! never silently dropped, never admitted past the bound. Pressure levels
//! are derived from fixed watermarks so every observer computes the same
//! level from the same depth. No randomness, no wall-clock, no hidden state.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// Queue capacity and watermark configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueBounds {
    max_queued: usize,
    elevated_pct: u8,
    critical_pct: u8,
}

impl QueueBounds {
    /// Build bounds. Requires `max_queued >= 1` and
    /// `0 < elevated_pct < critical_pct <= 100`.
    pub fn new(max_queued: usize, elevated_pct: u8, critical_pct: u8) -> Result<Self> {
        if max_queued == 0 {
            return Err(FabricError::InvalidInput {
                field: "max_queued",
                reason: "queue capacity must be at least 1".to_string(),
            });
        }
        if elevated_pct == 0 || critical_pct == 0 {
            return Err(FabricError::InvalidInput {
                field: "watermarks",
                reason: "watermark percentages must be positive".to_string(),
            });
        }
        if elevated_pct >= critical_pct {
            return Err(FabricError::InvalidInput {
                field: "watermarks",
                reason: "elevated_pct must be strictly below critical_pct".to_string(),
            });
        }
        if critical_pct > 100 {
            return Err(FabricError::InvalidInput {
                field: "watermarks",
                reason: "critical_pct must be at most 100".to_string(),
            });
        }
        Ok(Self {
            max_queued,
            elevated_pct,
            critical_pct,
        })
    }

    pub fn max_queued(&self) -> usize {
        self.max_queued
    }

    /// Depth at which pressure becomes elevated (ceil of percentage).
    pub fn elevated_at(&self) -> usize {
        self.max_queued
            .saturating_mul(self.elevated_pct as usize)
            .saturating_add(99)
            / 100
    }

    /// Depth at which pressure becomes critical (ceil of percentage).
    pub fn critical_at(&self) -> usize {
        self.max_queued
            .saturating_mul(self.critical_pct as usize)
            .saturating_add(99)
            / 100
    }
}

/// Deterministic pressure level derived from queue depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PressureLevel {
    Normal,
    Elevated,
    Critical,
}

/// Deterministic admission decision for one submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Admission {
    /// Queue has room; the submission may enter.
    Admit,
    /// Queue is at capacity; the submission is rejected with backpressure.
    /// The caller must surface this to the submitter — never drop silently.
    RejectBackpressure,
}

/// Backpressure controller: pure functions of queue depth.
#[derive(Debug, Clone, Copy)]
pub struct Backpressure {
    bounds: QueueBounds,
}

impl Backpressure {
    pub fn new(bounds: QueueBounds) -> Self {
        Self { bounds }
    }

    pub fn bounds(&self) -> QueueBounds {
        self.bounds
    }

    /// Pressure level for `queued` tasks. Deterministic: same depth always
    /// yields the same level.
    pub fn level(&self, queued: usize) -> PressureLevel {
        if queued >= self.bounds.critical_at() {
            PressureLevel::Critical
        } else if queued >= self.bounds.elevated_at() {
            PressureLevel::Elevated
        } else {
            PressureLevel::Normal
        }
    }

    /// Admission decision for a new submission at depth `queued`.
    /// Rejects if and only if `queued >= max_queued`.
    pub fn admit(&self, queued: usize) -> Admission {
        if queued >= self.bounds.max_queued() {
            Admission::RejectBackpressure
        } else {
            Admission::Admit
        }
    }

    /// How many tasks must be shed (completed, cancelled, or deferred) to
    /// return below the elevated watermark. Zero when already below it.
    pub fn shed_to_elevated(&self, queued: usize) -> usize {
        let target = self.bounds.elevated_at();
        if queued < target {
            0
        } else {
            queued - target + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bp() -> Backpressure {
        Backpressure::new(QueueBounds::new(100, 70, 90).unwrap())
    }

    #[test]
    fn rejects_invalid_bounds() {
        assert!(QueueBounds::new(0, 70, 90).is_err());
        assert!(QueueBounds::new(100, 0, 90).is_err());
        assert!(QueueBounds::new(100, 90, 90).is_err());
        assert!(QueueBounds::new(100, 95, 90).is_err());
        assert!(QueueBounds::new(100, 70, 101).is_err());
        assert!(QueueBounds::new(1, 50, 100).is_ok());
    }

    #[test]
    fn watermarks_use_ceiling() {
        let b = QueueBounds::new(100, 70, 90).unwrap();
        assert_eq!(b.elevated_at(), 70);
        assert_eq!(b.critical_at(), 90);
        // 70% of 10 = 7 exactly; 33% of 10 -> ceil = 4
        let b2 = QueueBounds::new(10, 33, 66).unwrap();
        assert_eq!(b2.elevated_at(), 4);
        assert_eq!(b2.critical_at(), 7);
    }

    #[test]
    fn pressure_levels_follow_watermarks() {
        let b = bp();
        assert_eq!(b.level(0), PressureLevel::Normal);
        assert_eq!(b.level(69), PressureLevel::Normal);
        assert_eq!(b.level(70), PressureLevel::Elevated);
        assert_eq!(b.level(89), PressureLevel::Elevated);
        assert_eq!(b.level(90), PressureLevel::Critical);
        assert_eq!(b.level(10_000), PressureLevel::Critical);
    }

    #[test]
    fn admission_rejects_exactly_at_capacity() {
        let b = bp();
        assert_eq!(b.admit(99), Admission::Admit);
        assert_eq!(b.admit(100), Admission::RejectBackpressure);
        assert_eq!(b.admit(101), Admission::RejectBackpressure);
    }

    #[test]
    fn shed_counts_return_below_elevated() {
        let b = bp();
        assert_eq!(b.shed_to_elevated(0), 0);
        assert_eq!(b.shed_to_elevated(69), 0);
        assert_eq!(b.shed_to_elevated(70), 1);
        assert_eq!(b.shed_to_elevated(100), 31);
    }

    #[test]
    fn decisions_are_deterministic() {
        let b = bp();
        for q in [0, 1, 69, 70, 90, 100, 500] {
            assert_eq!(b.level(q), b.level(q));
            assert_eq!(b.admit(q), b.admit(q));
            assert_eq!(b.shed_to_elevated(q), b.shed_to_elevated(q));
        }
    }

    #[test]
    fn serializes_stably() {
        let json = serde_json::to_string(&PressureLevel::Critical).unwrap();
        assert_eq!(json, "\"CRITICAL\"");
        let json = serde_json::to_string(&Admission::RejectBackpressure).unwrap();
        assert_eq!(json, "\"REJECT_BACKPRESSURE\"");
    }
}
