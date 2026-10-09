//! T047: budget reservations.
//!
//! Reserve request/token/cost budgets *before* dispatch and reconcile or
//! release them after completion or failure. Reservations are tracked per
//! endpoint and limit dimension so concurrent dispatches cannot collectively
//! exceed a configured limit.
//!
//! Unknown limits fail closed: when a dimension's limit is unknown the
//! reservation is denied with an explicit reason ("cannot assume headroom"),
//! never silently granted.

use crate::{LimitKind, LimitSet, QuotaLimit, UnixMillis};
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of limit dimensions. Implemented for [`LimitSet`]; unit tests use a
/// local stub so this module does not depend on another cell's `todo!()`
/// bodies. (Integrator note: the `LimitSet` impl delegates to
/// `LimitSet::get`; confirm Q1's implementation before wiring live traffic.)
pub trait LimitSource {
    fn limit(&self, kind: LimitKind) -> Option<&QuotaLimit>;
}

impl LimitSource for LimitSet {
    fn limit(&self, kind: LimitKind) -> Option<&QuotaLimit> {
        self.get(kind)
    }
}

/// What a single dispatch intends to consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider-defined micro-cost units.
    pub cost_micros: u64,
}

impl CostEstimate {
    /// Tokens across both directions (saturating).
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Why a reservation was denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyReason {
    /// The blocking dimension; `None` means the dimension itself is unknown.
    pub limit: Option<LimitKind>,
    pub detail: String,
}

/// Outcome of a reservation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReserveOutcome {
    Granted(Reservation),
    Denied(DenyReason),
}

/// A live budget reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: u64,
    pub endpoint: String,
    pub reserved: CostEstimate,
    pub created_ms: UnixMillis,
}

/// What reconciliation found: reserved vs actually consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationDelta {
    pub id: u64,
    pub reserved: CostEstimate,
    pub actual: CostEstimate,
}

/// Tracks live reservations and committed (reserved-but-unreconciled) usage
/// per endpoint and limit dimension.
#[derive(Debug, Default)]
pub struct ReservationBook {
    next_id: u64,
    active: HashMap<u64, Reservation>,
    committed: HashMap<(String, LimitKind), u64>,
}

impl ReservationBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Map an estimate onto reservable (dimension, amount) pairs.
    /// Cooldown is time-based and not reservable; it is enforced elsewhere.
    fn dimensions(estimate: &CostEstimate) -> [(LimitKind, u64); 5] {
        [
            (LimitKind::RequestRate, estimate.requests),
            (LimitKind::TokenRate, estimate.total_tokens()),
            (LimitKind::DailyUse, estimate.requests),
            (LimitKind::Concurrency, 1),
            (LimitKind::Cost, estimate.cost_micros),
        ]
    }

    fn deny_unknown(endpoint: &str, kind: LimitKind) -> ReserveOutcome {
        ReserveOutcome::Denied(DenyReason {
            limit: None,
            detail: format!(
                "{kind:?} limit unknown for endpoint '{endpoint}': cannot assume headroom"
            ),
        })
    }

    /// Attempt to reserve budget for one dispatch. Fails closed on unknown
    /// limits and on any dimension that would be exceeded.
    pub fn reserve(
        &mut self,
        endpoint: &str,
        limits: &dyn LimitSource,
        estimate: &CostEstimate,
        now: UnixMillis,
    ) -> ReserveOutcome {
        for (kind, want) in Self::dimensions(estimate) {
            let quota = match limits.limit(kind) {
                None => return Self::deny_unknown(endpoint, kind),
                Some(q) => q,
            };
            let max = match quota.max {
                None => return Self::deny_unknown(endpoint, kind),
                Some(m) => m,
            };
            let used = self
                .committed
                .get(&(endpoint.to_string(), kind))
                .copied()
                .unwrap_or(0);
            if used.saturating_add(want) > max {
                return ReserveOutcome::Denied(DenyReason {
                    limit: Some(kind),
                    detail: format!(
                        "would exceed {kind:?} for '{endpoint}': committed {used} + requested {want} > max {max}"
                    ),
                });
            }
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        for (kind, want) in Self::dimensions(estimate) {
            let entry = self
                .committed
                .entry((endpoint.to_string(), kind))
                .or_insert(0);
            *entry = entry.saturating_add(want);
        }
        let reservation = Reservation {
            id,
            endpoint: endpoint.to_string(),
            reserved: *estimate,
            created_ms: now,
        };
        self.active.insert(id, reservation.clone());
        ReserveOutcome::Granted(reservation)
    }

    /// Reconcile after completion: release the reservation and report
    /// reserved-vs-actual so the caller can record true usage.
    pub fn reconcile(
        &mut self,
        id: u64,
        actual: &CostEstimate,
    ) -> Result<ReservationDelta> {
        let reservation = self.active.remove(&id).ok_or(FabricError::InvalidInput {
            field: "reservation_id",
            reason: format!("unknown reservation {id}"),
        })?;
        for (kind, want) in Self::dimensions(&reservation.reserved) {
            if let Some(used) = self
                .committed
                .get_mut(&(reservation.endpoint.clone(), kind))
            {
                *used = used.saturating_sub(want);
            }
        }
        Ok(ReservationDelta {
            id,
            reserved: reservation.reserved,
            actual: *actual,
        })
    }

    /// Release after failure or cancellation: drop the whole reservation.
    pub fn release(&mut self, id: u64) -> Result<()> {
        let reservation = self.active.remove(&id).ok_or(FabricError::InvalidInput {
            field: "reservation_id",
            reason: format!("unknown reservation {id}"),
        })?;
        for (kind, want) in Self::dimensions(&reservation.reserved) {
            if let Some(used) = self
                .committed
                .get_mut(&(reservation.endpoint.clone(), kind))
            {
                *used = used.saturating_sub(want);
            }
        }
        Ok(())
    }

    /// Number of live reservations.
    pub fn outstanding(&self) -> usize {
        self.active.len()
    }

    /// Committed-but-unreconciled usage for one endpoint and dimension.
    pub fn committed_for(&self, endpoint: &str, kind: LimitKind) -> u64 {
        self.committed
            .get(&(endpoint.to_string(), kind))
            .copied()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local stub so tests don't depend on another cell's unfinished bodies.
    struct StubLimits(HashMap<LimitKind, QuotaLimit>);

    impl StubLimits {
        fn full(max: u64) -> Self {
            let mut m = HashMap::new();
            for kind in LimitSet::all_kinds() {
                m.insert(kind, QuotaLimit { kind, max: Some(max), window_ms: None });
            }
            Self(m)
        }

        /// Tight on RequestRate (max 1) but roomy elsewhere: the first
        /// reservation fits, the second must be denied on RequestRate.
        fn tight_requests() -> Self {
            let mut m = HashMap::new();
            for kind in LimitSet::all_kinds() {
                let max = if kind == LimitKind::RequestRate {
                    1
                } else {
                    10_000
                };
                m.insert(kind, QuotaLimit { kind, max: Some(max), window_ms: None });
            }
            Self(m)
        }
    }

    impl LimitSource for StubLimits {
        fn limit(&self, kind: LimitKind) -> Option<&QuotaLimit> {
            self.0.get(&kind)
        }
    }

    fn est() -> CostEstimate {
        CostEstimate {
            requests: 1,
            input_tokens: 100,
            output_tokens: 50,
            cost_micros: 10,
        }
    }

    #[test]
    fn grants_when_headroom_exists() {
        let mut book = ReservationBook::new();
        let limits = StubLimits::full(10_000);
        match book.reserve("ep1", &limits, &est(), 1_000) {
            ReserveOutcome::Granted(r) => {
                assert_eq!(r.id, 0);
                assert_eq!(r.endpoint, "ep1");
            }
            ReserveOutcome::Denied(d) => panic!("unexpected deny: {d:?}"),
        }
        assert_eq!(book.outstanding(), 1);
        assert_eq!(book.committed_for("ep1", LimitKind::TokenRate), 150);
    }

    #[test]
    fn denies_when_dimension_would_be_exceeded() {
        let mut book = ReservationBook::new();
        let tight = StubLimits::tight_requests();
        let first = book.reserve("ep1", &tight, &est(), 1_000);
        assert!(matches!(first, ReserveOutcome::Granted(_)));
        // second reservation: RequestRate 1+1 > 1 -> denied naming the dimension
        match book.reserve("ep1", &tight, &est(), 2_000) {
            ReserveOutcome::Denied(d) => assert_eq!(d.limit, Some(LimitKind::RequestRate)),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn denies_when_limit_unknown_fail_closed() {
        let mut book = ReservationBook::new();
        let empty = StubLimits(HashMap::new());
        match book.reserve("ep1", &empty, &est(), 1_000) {
            ReserveOutcome::Denied(d) => {
                assert_eq!(d.limit, None);
                assert!(d.detail.contains("cannot assume headroom"));
            }
            other => panic!("expected deny, got {other:?}"),
        }
        assert_eq!(book.outstanding(), 0);
    }

    #[test]
    fn reconcile_releases_and_reports_delta() {
        let mut book = ReservationBook::new();
        let limits = StubLimits::full(10_000);
        let r = match book.reserve("ep1", &limits, &est(), 1_000) {
            ReserveOutcome::Granted(r) => r,
            other => panic!("{other:?}"),
        };
        let actual = CostEstimate {
            requests: 1,
            input_tokens: 80,
            output_tokens: 40,
            cost_micros: 8,
        };
        let delta = book.reconcile(r.id, &actual).unwrap();
        assert_eq!(delta.reserved, est());
        assert_eq!(delta.actual, actual);
        assert_eq!(book.outstanding(), 0);
        assert_eq!(book.committed_for("ep1", LimitKind::TokenRate), 0);
        // double reconcile is an error
        assert!(book.reconcile(r.id, &actual).is_err());
    }

    #[test]
    fn release_drops_reservation_on_failure() {
        let mut book = ReservationBook::new();
        let limits = StubLimits::tight_requests();
        let r = match book.reserve("ep1", &limits, &est(), 1_000) {
            ReserveOutcome::Granted(r) => r,
            other => panic!("{other:?}"),
        };
        book.release(r.id).unwrap();
        assert_eq!(book.outstanding(), 0);
        // headroom is back: a fresh reservation fits again
        assert!(matches!(
            book.reserve("ep1", &limits, &est(), 2_000),
            ReserveOutcome::Granted(_)
        ));
        assert!(book.release(999).is_err());
    }

    #[test]
    fn outcome_round_trips() {
        let d = DenyReason {
            limit: Some(LimitKind::Cost),
            detail: "x".to_string(),
        };
        let j = serde_json::to_string(&ReserveOutcome::Denied(d)).unwrap();
        let back: ReserveOutcome = serde_json::from_str(&j).unwrap();
        assert!(matches!(back, ReserveOutcome::Denied(_)));
    }
}
