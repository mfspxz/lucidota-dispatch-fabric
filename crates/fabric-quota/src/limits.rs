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
        let _ = limit;
        todo!("T041: implement limit set")
    }

    /// Look up one dimension. Returns None when the dimension was never set
    /// (unknown), which is distinct from a zero limit.
    pub fn get(&self, kind: LimitKind) -> Option<&QuotaLimit> {
        let _ = kind;
        todo!("T041: implement limit lookup")
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
