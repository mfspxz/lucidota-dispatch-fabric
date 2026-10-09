//! T026: evidence-based task success rates by endpoint and task class.
//!
//! Only outcomes linked to an [`ArtifactRef`] are recorded. There is no API
//! for recording an unverified self-report: exclusion is enforced by the type
//! system (the only `record_*` method takes `evidence: &ArtifactRef`), not by
//! policy text. Evidence digests are deduplicated so one artifact can never
//! inflate a rate by double counting.

use fabric_types::{ArtifactRef, EndpointId, FabricError, Result, TaskClass};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Evidence-based success statistics, keyed by (endpoint, task class).
#[derive(Debug, Default)]
pub struct SuccessTracker {
    counts: HashMap<(EndpointId, TaskClass), (u64, u64)>,
    seen_evidence: HashSet<String>,
}

/// A point-in-time, evidence-backed success rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuccessRate {
    pub endpoint: EndpointId,
    pub class: TaskClass,
    pub verified_passes: u64,
    pub verified_total: u64,
    /// In [0.0, 1.0].
    pub rate: f64,
}

impl SuccessTracker {
    /// Empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one verified outcome. `evidence` is mandatory: an outcome without
    /// linked evidence cannot be expressed through this API, so unverified
    /// self-reports are excluded by construction. Recording the same evidence
    /// digest twice is a [`FabricError::Conflict`].
    pub fn record_verified_outcome(
        &mut self,
        endpoint: &EndpointId,
        class: TaskClass,
        passed: bool,
        evidence: &ArtifactRef,
    ) -> Result<()> {
        if evidence.digest.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "evidence.digest",
                reason: "evidence digest is required; unverified self-reports are not recorded"
                    .to_string(),
            });
        }
        if !self.seen_evidence.insert(evidence.digest.clone()) {
            return Err(FabricError::Conflict(format!(
                "evidence {} already recorded",
                evidence.digest
            )));
        }
        let entry = self
            .counts
            .entry((endpoint.clone(), class))
            .or_insert((0, 0));
        entry.1 += 1;
        if passed {
            entry.0 += 1;
        }
        Ok(())
    }

    /// Evidence-based success rate, or `None` when no verified outcomes exist
    /// for this (endpoint, class) pair. Never synthesizes a rate from zero data.
    pub fn success_rate(&self, endpoint: &EndpointId, class: TaskClass) -> Option<SuccessRate> {
        let (passes, total) = self.counts.get(&(endpoint.clone(), class.clone()))?;
        if *total == 0 {
            return None;
        }
        Some(SuccessRate {
            endpoint: endpoint.clone(),
            class,
            verified_passes: *passes,
            verified_total: *total,
            rate: *passes as f64 / *total as f64,
        })
    }

    /// Number of verified outcomes recorded for this pair.
    pub fn verified_total(&self, endpoint: &EndpointId, class: TaskClass) -> u64 {
        self.counts
            .get(&(endpoint.clone(), class))
            .map(|(_, t)| *t)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep() -> EndpointId {
        EndpointId::new("ep-1").unwrap()
    }

    fn evidence(digest: &str) -> ArtifactRef {
        ArtifactRef::new(digest, "application/json", 10, "verifier").unwrap()
    }

    #[test]
    fn rate_math_is_evidence_based() {
        let mut t = SuccessTracker::new();
        t.record_verified_outcome(&ep(), TaskClass::Implement, true, &evidence("sha256:aa"))
            .unwrap();
        t.record_verified_outcome(&ep(), TaskClass::Implement, false, &evidence("sha256:bb"))
            .unwrap();
        t.record_verified_outcome(&ep(), TaskClass::Implement, true, &evidence("sha256:cc"))
            .unwrap();
        let r = t.success_rate(&ep(), TaskClass::Implement).unwrap();
        assert_eq!(r.verified_passes, 2);
        assert_eq!(r.verified_total, 3);
        assert!((r.rate - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(t.verified_total(&ep(), TaskClass::Implement), 3);
    }

    #[test]
    fn unknown_pair_yields_none_not_zero() {
        let t = SuccessTracker::new();
        assert!(t.success_rate(&ep(), TaskClass::Review).is_none());
        assert_eq!(t.verified_total(&ep(), TaskClass::Review), 0);
    }

    #[test]
    fn rates_are_per_class() {
        let mut t = SuccessTracker::new();
        t.record_verified_outcome(&ep(), TaskClass::Implement, true, &evidence("sha256:aa"))
            .unwrap();
        t.record_verified_outcome(&ep(), TaskClass::Review, false, &evidence("sha256:bb"))
            .unwrap();
        assert_eq!(t.success_rate(&ep(), TaskClass::Implement).unwrap().rate, 1.0);
        assert_eq!(t.success_rate(&ep(), TaskClass::Review).unwrap().rate, 0.0);
    }

    #[test]
    fn duplicate_evidence_is_rejected() {
        let mut t = SuccessTracker::new();
        t.record_verified_outcome(&ep(), TaskClass::Implement, true, &evidence("sha256:aa"))
            .unwrap();
        let dup =
            t.record_verified_outcome(&ep(), TaskClass::Implement, true, &evidence("sha256:aa"));
        assert!(matches!(dup, Err(FabricError::Conflict(_))));
        assert_eq!(t.verified_total(&ep(), TaskClass::Implement), 1);
    }

    #[test]
    fn empty_evidence_digest_is_rejected() {
        let mut t = SuccessTracker::new();
        // A digest of only whitespace carries no evidence; must not count.
        let bad = ArtifactRef {
            digest: "   ".to_string(),
            media_type: "text/plain".to_string(),
            size_bytes: 0,
            provenance: "x".to_string(),
        };
        let r = t.record_verified_outcome(&ep(), TaskClass::Implement, true, &bad);
        assert!(matches!(r, Err(FabricError::InvalidInput { .. })));
        assert_eq!(t.verified_total(&ep(), TaskClass::Implement), 0);
    }
}
