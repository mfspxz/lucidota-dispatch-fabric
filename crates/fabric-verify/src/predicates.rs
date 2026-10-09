//! T095 — independent acceptance-predicate evaluation.
//!
//! Each acceptance predicate is evaluated independently against the evidence
//! links attached to a task attempt. Evaluation is a **pure function**:
//!
//! ```text
//! evaluate(predicate, evidence_links) -> PredicateOutcome
//! ```
//!
//! No I/O, no clocks, no hidden state. Missing evidence never yields PASS:
//! when a required evidence kind is absent, the outcome is FAIL or
//! INCONCLUSIVE according to the predicate's declared `on_missing` policy.

use serde::{Deserialize, Serialize};

/// What to report when required evidence is missing. Never PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OnMissing {
    /// Missing evidence is a hard failure of the predicate.
    Fail,
    /// Missing evidence means the predicate cannot be decided.
    Inconclusive,
}

/// One link from a predicate to a piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLink {
    /// Typed evidence kind, e.g. "test-report", "diff-patch".
    pub kind: String,
    /// Reference to the evidence (digest, artifact ref, path).
    pub reference: String,
}

/// An acceptance predicate over a task attempt's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptancePredicate {
    /// Stable predicate ID.
    pub id: String,
    pub description: String,
    /// Evidence kinds required for a decision. Empty = vacuously decidable.
    pub required_evidence_kinds: Vec<String>,
    /// Outcome when any required kind is missing. Never PASS.
    pub on_missing: OnMissing,
}

/// Outcome of evaluating one predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredicateOutcome {
    /// All required evidence kinds present.
    Pass { evidence_used: Vec<String> },
    /// Required evidence missing and policy is Fail.
    Fail { reason: String },
    /// Required evidence missing and policy is Inconclusive.
    Inconclusive { reason: String },
}

/// Evaluate one predicate against the given evidence links.
///
/// Pure: no I/O, deterministic. Missing required kinds yield FAIL or
/// INCONCLUSIVE per `on_missing` — never PASS.
pub fn evaluate(
    predicate: &AcceptancePredicate,
    evidence: &[EvidenceLink],
) -> PredicateOutcome {
    let mut missing: Vec<&str> = Vec::new();
    for kind in &predicate.required_evidence_kinds {
        if !evidence.iter().any(|link| link.kind == *kind) {
            missing.push(kind.as_str());
        }
    }
    if missing.is_empty() {
        return PredicateOutcome::Pass {
            evidence_used: predicate.required_evidence_kinds.clone(),
        };
    }
    let reason = format!(
        "predicate '{}' missing required evidence kinds: {}",
        predicate.id,
        missing.join(", ")
    );
    match predicate.on_missing {
        OnMissing::Fail => PredicateOutcome::Fail { reason },
        OnMissing::Inconclusive => PredicateOutcome::Inconclusive { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(kinds: &[&str], on_missing: OnMissing) -> AcceptancePredicate {
        AcceptancePredicate {
            id: "p1".to_string(),
            description: "unit tests must pass".to_string(),
            required_evidence_kinds: kinds.iter().map(|s| s.to_string()).collect(),
            on_missing,
        }
    }

    fn link(kind: &str) -> EvidenceLink {
        EvidenceLink {
            kind: kind.to_string(),
            reference: "sha256:abc".to_string(),
        }
    }

    #[test]
    fn all_evidence_present_passes() {
        let out = evaluate(&pred(&["test-report"], OnMissing::Fail), &[link("test-report")]);
        assert!(matches!(out, PredicateOutcome::Pass { .. }));
    }

    #[test]
    fn missing_evidence_fails_per_policy() {
        let out = evaluate(&pred(&["test-report"], OnMissing::Fail), &[]);
        match out {
            PredicateOutcome::Fail { reason } => assert!(reason.contains("test-report")),
            other => panic!("expected Fail, got {:?}", other),
        }
    }

    #[test]
    fn missing_evidence_inconclusive_per_policy() {
        let out = evaluate(&pred(&["test-report"], OnMissing::Inconclusive), &[]);
        assert!(matches!(out, PredicateOutcome::Inconclusive { .. }));
    }

    #[test]
    fn missing_evidence_never_passes() {
        for policy in [OnMissing::Fail, OnMissing::Inconclusive] {
            let out = evaluate(&pred(&["a", "b"], policy), &[link("a")]);
            assert!(
                !matches!(out, PredicateOutcome::Pass { .. }),
                "policy {:?} must not PASS on missing evidence",
                policy
            );
        }
    }

    #[test]
    fn unrelated_evidence_does_not_satisfy() {
        let out = evaluate(
            &pred(&["test-report"], OnMissing::Fail),
            &[link("unrelated-kind")],
        );
        assert!(matches!(out, PredicateOutcome::Fail { .. }));
    }

    #[test]
    fn evaluation_is_pure_and_deterministic() {
        let p = pred(&["test-report"], OnMissing::Fail);
        let ev = vec![link("test-report")];
        let a = evaluate(&p, &ev);
        let b = evaluate(&p, &ev);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_requirements_pass_vacuously() {
        let out = evaluate(&pred(&[], OnMissing::Fail), &[]);
        assert!(matches!(out, PredicateOutcome::Pass { .. }));
    }
}
