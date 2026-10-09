//! T041 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Model request-rate, token-rate, daily-use, concurrency, cost, and cooldown
//! limits independently. Unknown limits remain unknown (`None`), never
//! defaulted or invented.
//!
//! Worker Q1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use serde::{Deserialize, Serialize};

/// The six independently-modeled limit dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LimitKind {
    RequestRate,
    TokenRate,
    DailyUse,
    Concurrency,
    Cost,
    Cooldown,
}

/// One limit. `max: None` means the limit is UNKNOWN (not unlimited, not
/// zero) — callers must treat unknown as "cannot assume headroom".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaLimit {
    pub kind: LimitKind,
    pub max: Option<u64>,
    /// Window in milliseconds for rate-style limits; None when not applicable.
    pub window_ms: Option<u64>,
}

/// The per-endpoint limit set. Dimensions are independent; setting one never
/// implies another.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LimitSet {
    limits: Vec<QuotaLimit>,
}

impl LimitSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or replace) one limit dimension.
    pub fn set(&mut self, limit: QuotaLimit) {
        match self.limits.iter_mut().find(|l| l.kind == limit.kind) {
            Some(existing) => *existing = limit,
            None => self.limits.push(limit),
        }
    }

    /// Look up one dimension. Returns None when the dimension was never set
    /// (unknown), which is distinct from a zero limit.
    pub fn get(&self, kind: LimitKind) -> Option<&QuotaLimit> {
        self.limits.iter().find(|l| l.kind == kind)
    }

    /// All six kinds, each with its current setting (possibly unknown).
    pub fn all_kinds() -> [LimitKind; 6] {
        [
            LimitKind::RequestRate,
            LimitKind::TokenRate,
            LimitKind::DailyUse,
            LimitKind::Concurrency,
            LimitKind::Cost,
            LimitKind::Cooldown,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(kind: LimitKind, max: Option<u64>, window_ms: Option<u64>) -> QuotaLimit {
        QuotaLimit { kind, max, window_ms }
    }

    #[test]
    fn set_and_get_round_trip() {
        let mut s = LimitSet::new();
        s.set(limit(LimitKind::RequestRate, Some(60), Some(60_000)));
        let got = s.get(LimitKind::RequestRate).unwrap();
        assert_eq!(got.max, Some(60));
        assert_eq!(got.window_ms, Some(60_000));
    }

    #[test]
    fn unset_dimension_is_unknown_not_zero() {
        let s = LimitSet::new();
        assert!(s.get(LimitKind::TokenRate).is_none());
        // unknown is distinct from an explicit zero limit
        let mut s2 = LimitSet::new();
        s2.set(limit(LimitKind::TokenRate, Some(0), None));
        assert_eq!(s2.get(LimitKind::TokenRate).unwrap().max, Some(0));
    }

    #[test]
    fn set_replaces_same_dimension() {
        let mut s = LimitSet::new();
        s.set(limit(LimitKind::Concurrency, Some(4), None));
        s.set(limit(LimitKind::Concurrency, Some(8), None));
        assert_eq!(s.get(LimitKind::Concurrency).unwrap().max, Some(8));
    }

    #[test]
    fn dimensions_are_independent() {
        let mut s = LimitSet::new();
        s.set(limit(LimitKind::RequestRate, Some(60), Some(60_000)));
        s.set(limit(LimitKind::Cost, None, None)); // unknown cost limit
        assert_eq!(s.get(LimitKind::RequestRate).unwrap().max, Some(60));
        assert!(s.get(LimitKind::Cost).unwrap().max.is_none());
        assert!(s.get(LimitKind::DailyUse).is_none());
    }

    #[test]
    fn unknown_limit_survives_serde() {
        let mut s = LimitSet::new();
        s.set(limit(LimitKind::Cooldown, None, Some(5_000)));
        let json = serde_json::to_string(&s).unwrap();
        let back: LimitSet = serde_json::from_str(&json).unwrap();
        let got = back.get(LimitKind::Cooldown).unwrap();
        assert!(got.max.is_none());
        assert_eq!(got.window_ms, Some(5_000));
    }

    #[test]
    fn all_kinds_covers_six_dimensions() {
        assert_eq!(LimitSet::all_kinds().len(), 6);
    }
}
