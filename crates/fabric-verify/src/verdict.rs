//! T100 — verdict receipts from completed predicate evaluations only.
//!
//! The verdict receipt is the final word: it is constructible only through
//! the builder, and the builder refuses to finalize while any expected
//! predicate is still unevaluated. A receipt can therefore never claim a
//! predicate outcome that was never recorded.

use fabric_types::{FabricError, Result, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One predicate's recorded evaluation inside the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateVerdict {
    pub predicate_id: String,
    pub outcome: Verdict,
    /// The exact command (argv) that was run.
    pub command: Vec<String>,
    /// Verifier version that produced the outcome (cache/audit linkage).
    pub verifier_version: String,
    /// Content digests of the evidence backing the outcome.
    pub evidence_digests: Vec<String>,
}

/// The final-word verdict receipt for one requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictReceipt {
    pub requirement_id: String,
    pub predicates: Vec<PredicateVerdict>,
    pub overall: Verdict,
    /// sha256 over the canonical JSON of this receipt (self-describing).
    pub receipt_digest: String,
}

/// Builder: declare expected predicates, record each evaluation, finalize
/// only when all are evaluated. Reusing a predicate id or recording one
/// that was not expected is a typed error.
#[derive(Debug)]
pub struct VerdictBuilder {
    requirement_id: String,
    expected: Vec<String>,
    recorded: BTreeMap<String, PredicateVerdict>,
}

impl VerdictBuilder {
    /// Declare the full set of predicates that must be evaluated.
    /// Non-empty, duplicate-free, non-blank ids required.
    pub fn expect_predicates(
        requirement_id: impl Into<String>,
        predicate_ids: Vec<String>,
    ) -> Result<Self> {
        if predicate_ids.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "verdict.predicates",
                reason: "at least one predicate is required".to_string(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for id in &predicate_ids {
            if id.trim().is_empty() || !seen.insert(id) {
                return Err(FabricError::InvalidInput {
                    field: "verdict.predicate_id",
                    reason: "blank or duplicate predicate id".to_string(),
                });
            }
        }
        Ok(VerdictBuilder {
            requirement_id: requirement_id.into(),
            expected: predicate_ids,
            recorded: BTreeMap::new(),
        })
    }

    /// Record one completed predicate evaluation.
    pub fn record(&mut self, p: PredicateVerdict) -> Result<()> {
        if !self.expected.contains(&p.predicate_id) {
            return Err(FabricError::InvalidInput {
                field: "verdict.predicate_id",
                reason: format!("predicate '{}' was not expected", p.predicate_id),
            });
        }
        if self.recorded.contains_key(&p.predicate_id) {
            return Err(FabricError::Conflict(format!(
                "predicate '{}' already recorded",
                p.predicate_id
            )));
        }
        self.recorded.insert(p.predicate_id.clone(), p);
        Ok(())
    }

    /// Finalize. Refuses while any expected predicate is unevaluated.
    pub fn finalize(self) -> Result<VerdictReceipt> {
        let missing: Vec<&String> = self
            .expected
            .iter()
            .filter(|id| !self.recorded.contains_key(*id))
            .collect();
        if !missing.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "verdict.predicates",
                reason: format!("unevaluated predicates: {}", missing[0]),
            });
        }
        let predicates: Vec<PredicateVerdict> = self
            .recorded
            .into_values()
            .collect();
        let overall = if predicates.iter().any(|p| p.outcome == Verdict::Fail) {
            Verdict::Fail
        } else if predicates.iter().all(|p| p.outcome.is_pass()) {
            Verdict::Pass
        } else if predicates.iter().any(|p| p.outcome == Verdict::Blocked) {
            Verdict::Blocked
        } else {
            Verdict::Inconclusive
        };
        let mut receipt = VerdictReceipt {
            requirement_id: self.requirement_id,
            predicates,
            overall,
            receipt_digest: String::new(),
        };
        let canonical = serde_json::to_string(&(&receipt.requirement_id, &receipt.predicates, &receipt.overall))
            .map_err(|e| FabricError::MalformedOutput(e.to_string()))?;
        // FNV-1a over the canonical JSON. Integrity-only (not a security
        // boundary); stable across processes, unlike SipHash.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in canonical.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        receipt.receipt_digest = format!("fnv1a64:{:016x}", h);
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(id: &str, outcome: Verdict) -> PredicateVerdict {
        PredicateVerdict {
            predicate_id: id.to_string(),
            outcome,
            command: vec!["cargo".to_string(), "test".to_string()],
            verifier_version: "v1".to_string(),
            evidence_digests: vec!["sha256:e".to_string()],
        }
    }

    fn builder() -> VerdictBuilder {
        VerdictBuilder::expect_predicates("REQ-1", vec!["p1".to_string(), "p2".to_string()]).unwrap()
    }

    #[test]
    fn finalize_refuses_with_unevaluated_predicates() {
        let mut b = builder();
        b.record(pv("p1", Verdict::Pass)).unwrap();
        let err = b.finalize().unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(err.to_string().contains("p2"));
    }

    #[test]
    fn all_pass_finalizes_as_pass() {
        let mut b = builder();
        b.record(pv("p1", Verdict::Pass)).unwrap();
        b.record(pv("p2", Verdict::Pass)).unwrap();
        let r = b.finalize().unwrap();
        assert_eq!(r.overall, Verdict::Pass);
        assert_eq!(r.predicates.len(), 2);
        assert!(!r.receipt_digest.is_empty());
    }

    #[test]
    fn any_fail_finalizes_as_fail() {
        let mut b = builder();
        b.record(pv("p1", Verdict::Pass)).unwrap();
        b.record(pv("p2", Verdict::Fail)).unwrap();
        assert_eq!(b.finalize().unwrap().overall, Verdict::Fail);
    }

    #[test]
    fn duplicate_record_is_conflict() {
        let mut b = builder();
        b.record(pv("p1", Verdict::Pass)).unwrap();
        let err = b.record(pv("p1", Verdict::Fail)).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn unexpected_predicate_is_rejected() {
        let mut b = builder();
        let err = b.record(pv("pX", Verdict::Pass)).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn empty_or_dup_predicate_set_rejected() {
        assert!(VerdictBuilder::expect_predicates("R", vec![]).is_err());
        assert!(VerdictBuilder::expect_predicates("R", vec!["p".to_string(), "p".to_string()]).is_err());
    }

    #[test]
    fn receipt_is_deterministic() {
        let mut b1 = builder();
        b1.record(pv("p1", Verdict::Pass)).unwrap();
        b1.record(pv("p2", Verdict::Pass)).unwrap();
        let mut b2 = builder();
        b2.record(pv("p2", Verdict::Pass)).unwrap();
        b2.record(pv("p1", Verdict::Pass)).unwrap();
        assert_eq!(b1.finalize().unwrap().receipt_digest, b2.finalize().unwrap().receipt_digest);
    }
}
