//! T007: EvidenceReceipt.
//!
//! Links task, attempt, artifact digests, commands, exit codes, verifier, and
//! verdict. A receipt is only valid when every command has a recorded exit
//! code and the verifier identity is present: completion without evidence
//! references cannot be recorded (T084 relies on this).

use crate::error::{FabricError, Result};
use crate::ids::TaskId;
use crate::verdict::{FailureKind, Verdict};
use serde::{Deserialize, Serialize};

/// T007: evidence receipt for one task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReceipt {
    pub task_id: TaskId,
    /// 1-based attempt number.
    pub attempt: u32,
    /// Content digests of produced artifacts (`algo:hex`).
    pub artifact_digests: Vec<String>,
    /// Exact commands executed, in order.
    pub commands: Vec<String>,
    /// Exit code per command; length must equal `commands.len()`.
    pub exit_codes: Vec<i32>,
    /// Verifier identity, e.g. `cargo-test`.
    pub verifier: String,
    pub verifier_version: String,
    pub verdict: Verdict,
    pub failure: Option<FailureKind>,
}

impl EvidenceReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: TaskId,
        attempt: u32,
        artifact_digests: Vec<String>,
        commands: Vec<String>,
        exit_codes: Vec<i32>,
        verifier: impl Into<String>,
        verifier_version: impl Into<String>,
        verdict: Verdict,
        failure: Option<FailureKind>,
    ) -> Result<Self> {
        if attempt == 0 {
            return Err(FabricError::InvalidInput {
                field: "attempt",
                reason: "attempt numbers start at 1".to_string(),
            });
        }
        if commands.len() != exit_codes.len() {
            return Err(FabricError::InvalidInput {
                field: "exit_codes",
                reason: "one exit code per command is required".to_string(),
            });
        }
        let verifier = verifier.into();
        if verifier.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "verifier",
                reason: "verifier identity is required".to_string(),
            });
        }
        Ok(Self {
            task_id,
            attempt,
            artifact_digests,
            commands,
            exit_codes,
            verifier,
            verifier_version: verifier_version.into(),
            verdict,
            failure,
        })
    }

    /// True when every recorded command exited 0 and the verdict is Pass.
    pub fn all_commands_ok(&self) -> bool {
        self.exit_codes.iter().all(|c| *c == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt() -> EvidenceReceipt {
        EvidenceReceipt::new(
            TaskId::new("T001").unwrap(),
            1,
            vec!["sha256:abcd".to_string()],
            vec!["cargo test".to_string()],
            vec![0],
            "cargo-test",
            "1.99.0",
            Verdict::Pass,
            None,
        )
        .unwrap()
    }

    #[test]
    fn valid_receipt_builds() {
        let r = receipt();
        assert!(r.all_commands_ok());
    }

    #[test]
    fn rejects_mismatched_exit_codes() {
        let r = EvidenceReceipt::new(
            TaskId::new("T1").unwrap(),
            1,
            vec![],
            vec!["a".to_string(), "b".to_string()],
            vec![0],
            "v",
            "1",
            Verdict::Fail,
            Some(FailureKind::VerificationFailure),
        );
        assert!(r.is_err());
    }

    #[test]
    fn rejects_zero_attempt_and_empty_verifier() {
        let base = || {
            EvidenceReceipt::new(
                TaskId::new("T1").unwrap(),
                1,
                vec![],
                vec![],
                vec![],
                "v",
                "1",
                Verdict::Pass,
                None,
            )
        };
        assert!(base().is_ok());
        assert!(receipt().clone().attempt == 1);
        let zero = EvidenceReceipt::new(
            TaskId::new("T1").unwrap(),
            0,
            vec![],
            vec![],
            vec![],
            "v",
            "1",
            Verdict::Pass,
            None,
        );
        assert!(zero.is_err());
    }

    #[test]
    fn round_trips() {
        let r = receipt();
        let j = serde_json::to_string(&r).unwrap();
        let back: EvidenceReceipt = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }
}
