//! T098 — review findings with mandatory fields.
//!
//! Every finding must include: severity, task/requirement reference,
//! evidence, and reproduction steps. Validation is mechanical: a finding
//! missing any required field is rejected with `InvalidInput`. Severity is
//! a closed enum — no free-form strings.

use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};

/// Closed severity scale. No other values can be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// A review finding. All fields are required by construction; `validate`
/// rejects findings with empty or missing content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub severity: Severity,
    pub task_id: TaskId,
    pub requirement_ref: String,
    /// Evidence supporting the finding (digests or verbatim excerpts).
    pub evidence: String,
    /// Steps to reproduce, in order. Non-empty by construction.
    pub repro_steps: Vec<String>,
}

impl ReviewFinding {
    pub fn validate(&self) -> Result<()> {
        if self.requirement_ref.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "finding.requirement_ref",
                reason: "empty".to_string(),
            });
        }
        if self.evidence.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "finding.evidence",
                reason: "empty".to_string(),
            });
        }
        if self.repro_steps.is_empty() || self.repro_steps.iter().all(|s| s.trim().is_empty()) {
            return Err(FabricError::InvalidInput {
                field: "finding.repro_steps",
                reason: "no usable reproduction steps".to_string(),
            });
        }
        Ok(())
    }

    /// Builder entry point. All arguments are required; use the setters for
    /// clarity on large findings.
    pub fn new(
        severity: Severity,
        task_id: TaskId,
        requirement_ref: impl Into<String>,
        evidence: impl Into<String>,
        repro_steps: Vec<String>,
    ) -> Result<Self> {
        let f = ReviewFinding {
            severity,
            task_id,
            requirement_ref: requirement_ref.into(),
            evidence: evidence.into(),
            repro_steps,
        };
        f.validate()?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(sev: Severity) -> ReviewFinding {
        ReviewFinding {
            severity: sev,
            task_id: TaskId::new("T-9").unwrap(),
            requirement_ref: "REQ-3".to_string(),
            evidence: "sha256:feed".to_string(),
            repro_steps: vec!["run the verifier".to_string()],
        }
    }

    #[test]
    fn valid_finding_passes() {
        assert!(valid(Severity::High).validate().is_ok());
        assert!(ReviewFinding::new(
            Severity::Critical,
            TaskId::new("T-9").unwrap(),
            "REQ-3",
            "sha256:feed",
            vec!["step one".to_string()]
        )
        .is_ok());
    }

    #[test]
    fn missing_requirement_ref_rejected() {
        let mut f = valid(Severity::Medium);
        f.requirement_ref = "  ".to_string();
        assert!(matches!(
            f.validate().unwrap_err(),
            FabricError::InvalidInput { .. }
        ));
    }

    #[test]
    fn missing_evidence_rejected() {
        let mut f = valid(Severity::Medium);
        f.evidence.clear();
        assert!(matches!(
            f.validate().unwrap_err(),
            FabricError::InvalidInput { .. }
        ));
    }

    #[test]
    fn missing_repro_steps_rejected() {
        let mut f = valid(Severity::Medium);
        f.repro_steps.clear();
        assert!(matches!(
            f.validate().unwrap_err(),
            FabricError::InvalidInput { .. }
        ));
        f.repro_steps = vec![" ".to_string()];
        assert!(f.validate().is_err());
    }

    #[test]
    fn severity_is_a_closed_enum() {
        // serde rejects anything outside the five known severities.
        let bad: std::result::Result<Severity, _> = serde_json::from_str("\"Extreme\"");
        assert!(bad.is_err());
        let ok: Severity = serde_json::from_str("\"HIGH\"").unwrap();
        assert_eq!(ok, Severity::High);
        assert!(Severity::Info < Severity::Critical);
    }
}
