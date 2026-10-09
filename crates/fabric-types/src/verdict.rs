//! T008: Verdict and FailureKind.
//!
//! Trinary-plus discipline: PASS, FAIL, INCONCLUSIVE, BLOCKED, INVALID, and
//! INFRA_ERROR are distinct. Missing evidence never becomes PASS (enforced
//! by the verifier in T095, represented here).

use serde::{Deserialize, Serialize};

/// T008: outcome of evaluating a task attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
    Blocked,
    Invalid,
    InfraError,
}

impl Verdict {
    /// True only for an unqualified pass.
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// T008: machine-readable failure classification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureKind {
    #[serde(rename = "POLICY_DENIED")]
    PolicyDenied,
    #[serde(rename = "QUOTA_EXHAUSTED")]
    QuotaExhausted,
    #[serde(rename = "TIMEOUT")]
    Timeout,
    #[serde(rename = "TRANSPORT_FAILURE")]
    TransportFailure,
    #[serde(rename = "MALFORMED_OUTPUT")]
    MalformedOutput,
    #[serde(rename = "CONFLICT")]
    Conflict,
    #[serde(rename = "VERIFICATION_FAILURE")]
    VerificationFailure,
    #[serde(rename = "INFRA_ERROR")]
    InfraError,
    /// Explicit carrier for a failure kind this build does not recognize.
    #[serde(rename = "UNKNOWN")]
    Unknown(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pass_is_pass() {
        assert!(Verdict::Pass.is_pass());
        for v in [
            Verdict::Fail,
            Verdict::Inconclusive,
            Verdict::Blocked,
            Verdict::Invalid,
            Verdict::InfraError,
        ] {
            assert!(!v.is_pass(), "{v:?}");
        }
    }

    #[test]
    fn serializes_as_names() {
        assert_eq!(serde_json::to_string(&Verdict::Inconclusive).unwrap(), "\"INCONCLUSIVE\"");
        assert_eq!(
            serde_json::to_string(&FailureKind::QuotaExhausted).unwrap(),
            "\"QUOTA_EXHAUSTED\""
        );
    }
}
