//! T096 — requirement coverage reports.
//!
//! Coverage is computed from predicate outcomes only. No requirement is ever
//! marked proven without linked passing evidence: a `Pass` outcome counts
//! toward proof only when it carries an evidence digest.
//!
//! Local minimal predicate type: `PredicateOutcome`. A sibling cell owns the
//! real predicate-evaluation types; rewire at integration if they exist.

use fabric_types::{Result, Verdict};
use serde::{Deserialize, Serialize};

/// INTEGRATOR: local minimal predicate outcome. Rewire to the sibling cell's
/// predicate types when they land; fields are deliberately narrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateOutcome {
    pub predicate_id: String,
    pub requirement_id: String,
    pub verdict: Verdict,
    /// Content digest of the evidence backing this evaluation, e.g.
    /// `sha256:<hex>`. Required for a pass to count toward proof.
    pub evidence_digest: Option<String>,
}

/// Coverage status of one requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequirementStatus {
    /// At least one predicate passed with linked passing evidence.
    Proven,
    /// At least one predicate failed (takes precedence over blocked).
    Failed,
    /// Blocked with no pass and no fail.
    Blocked,
    /// No evaluated predicates at all.
    Unverified,
}

/// Coverage of a single requirement with evidence references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementCoverage {
    pub requirement_id: String,
    pub status: RequirementStatus,
    /// Evidence digests backing the status (proven: passing evidence only).
    pub evidence: Vec<String>,
    pub predicates_evaluated: u32,
    pub predicates_passed: u32,
    pub predicates_failed: u32,
    pub predicates_blocked: u32,
}

/// Full coverage report, deterministic (ID-sorted) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub requirements: Vec<RequirementCoverage>,
    pub total: u32,
    pub proven: u32,
    pub failed: u32,
    pub blocked: u32,
    pub unverified: u32,
}

impl CoverageReport {
    /// Compute coverage for `requirement_ids` from predicate outcomes.
    ///
    /// Status precedence per requirement: Failed > Proven > Blocked >
    /// Unverified. A pass counts toward proof only with linked evidence.
    pub fn compute(requirement_ids: &[String], outcomes: &[PredicateOutcome]) -> Result<Self> {
        let mut requirements: Vec<RequirementCoverage> = requirement_ids
            .iter()
            .map(|req| {
                let rel: Vec<&PredicateOutcome> =
                    outcomes.iter().filter(|o| &o.requirement_id == req).collect();
                let passed: u32 = rel
                    .iter()
                    .filter(|o| {
                        o.verdict.is_pass() && o.evidence_digest.as_ref().is_some_and(|d| !d.is_empty())
                    })
                    .count() as u32;
                let failed: u32 =
                    rel.iter().filter(|o| o.verdict == Verdict::Fail).count() as u32;
                let blocked: u32 =
                    rel.iter().filter(|o| o.verdict == Verdict::Blocked).count() as u32;
                let status = if failed > 0 {
                    RequirementStatus::Failed
                } else if passed > 0 {
                    RequirementStatus::Proven
                } else if blocked > 0 {
                    RequirementStatus::Blocked
                } else {
                    RequirementStatus::Unverified
                };
                let mut evidence: Vec<String> = rel
                    .iter()
                    .filter(|o| o.verdict.is_pass())
                    .filter_map(|o| o.evidence_digest.clone())
                    .filter(|d| !d.is_empty())
                    .collect();
                evidence.sort();
                evidence.dedup();
                RequirementCoverage {
                    requirement_id: req.clone(),
                    status,
                    evidence,
                    predicates_evaluated: rel.len() as u32,
                    predicates_passed: passed,
                    predicates_failed: failed,
                    predicates_blocked: blocked,
                }
            })
            .collect();
        requirements.sort_by(|a, b| a.requirement_id.cmp(&b.requirement_id));
        let (mut proven, mut failed, mut blocked, mut unverified) = (0, 0, 0, 0);
        for r in &requirements {
            match r.status {
                RequirementStatus::Proven => proven += 1,
                RequirementStatus::Failed => failed += 1,
                RequirementStatus::Blocked => blocked += 1,
                RequirementStatus::Unverified => unverified += 1,
            }
        }
        Ok(CoverageReport {
            total: requirements.len() as u32,
            proven,
            failed,
            blocked,
            unverified,
            requirements,
        })
    }

    /// True only when every requirement is proven.
    pub fn all_proven(&self) -> bool {
        self.total > 0 && self.proven == self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(req: &str, pred: &str, verdict: Verdict, ev: Option<&str>) -> PredicateOutcome {
        PredicateOutcome {
            predicate_id: pred.to_string(),
            requirement_id: req.to_string(),
            verdict,
            evidence_digest: ev.map(|s| s.to_string()),
        }
    }

    #[test]
    fn pass_without_evidence_is_not_proven() {
        let r = CoverageReport::compute(
            &["R1".to_string()],
            &[outcome("R1", "p1", Verdict::Pass, None)],
        )
        .unwrap();
        assert_eq!(r.requirements[0].status, RequirementStatus::Unverified);
    }

    #[test]
    fn pass_with_evidence_is_proven() {
        let r = CoverageReport::compute(
            &["R1".to_string()],
            &[outcome("R1", "p1", Verdict::Pass, Some("sha256:abc"))],
        )
        .unwrap();
        let c = &r.requirements[0];
        assert_eq!(c.status, RequirementStatus::Proven);
        assert_eq!(c.evidence, vec!["sha256:abc".to_string()]);
    }

    #[test]
    fn fail_takes_precedence_over_pass() {
        let r = CoverageReport::compute(
            &["R1".to_string()],
            &[
                outcome("R1", "p1", Verdict::Pass, Some("sha256:a")),
                outcome("R1", "p2", Verdict::Fail, None),
            ],
        )
        .unwrap();
        assert_eq!(r.requirements[0].status, RequirementStatus::Failed);
    }

    #[test]
    fn blocked_when_only_blocked() {
        let r = CoverageReport::compute(
            &["R1".to_string()],
            &[outcome("R1", "p1", Verdict::Blocked, None)],
        )
        .unwrap();
        assert_eq!(r.requirements[0].status, RequirementStatus::Blocked);
    }

    #[test]
    fn no_outcomes_is_unverified() {
        let r = CoverageReport::compute(&["R1".to_string(), "R2".to_string()], &[]).unwrap();
        assert_eq!(r.unverified, 2);
        assert!(!r.all_proven());
    }

    #[test]
    fn report_is_id_sorted_and_totals_add_up() {
        let r = CoverageReport::compute(
            &["R2".to_string(), "R1".to_string()],
            &[outcome("R1", "p", Verdict::Pass, Some("sha256:e"))],
        )
        .unwrap();
        assert_eq!(r.requirements[0].requirement_id, "R1");
        assert_eq!(r.proven, 1);
        assert_eq!(r.unverified, 1);
        assert_eq!(r.total, r.proven + r.failed + r.blocked + r.unverified);
    }
}
