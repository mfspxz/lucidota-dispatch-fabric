//! T039 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Normalize provider errors into typed retryable/non-retryable errors while
//! preserving safe diagnostic evidence. `safe_detail` must never contain
//! credentials, request bodies, or response bodies beyond a bounded prefix.
//!
//! Worker A2: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::FabricError;
use thiserror::Error;

/// T039: provider error classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterErrorKind {
    Timeout,
    Transport,
    MalformedResponse,
    Cancelled,
    Quota,
    Auth,
    Unknown,
}

/// T039: normalized adapter error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind:?}: {safe_detail}")]
pub struct AdapterError {
    pub retryable: bool,
    pub kind: AdapterErrorKind,
    pub safe_detail: String,
}

impl AdapterError {
    /// Build a retryable error with a redacted detail string.
    ///
    /// `detail` must already be redacted: no credentials, no raw bodies
    /// beyond a bounded prefix. This constructor truncates defensively.
    pub fn retryable(kind: AdapterErrorKind, detail: impl Into<String>) -> Self {
        Self {
            retryable: true,
            kind,
            safe_detail: redact_detail(detail.into()),
        }
    }

    /// Build a non-retryable (fatal) error with a redacted detail string.
    pub fn fatal(kind: AdapterErrorKind, detail: impl Into<String>) -> Self {
        Self {
            retryable: false,
            kind,
            safe_detail: redact_detail(detail.into()),
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Map onto the shared structured error taxonomy (T010).
    pub fn to_fabric(&self) -> FabricError {
        let msg = format!("{:?}: {}", self.kind, self.safe_detail);
        match self.kind {
            AdapterErrorKind::Timeout => FabricError::Timeout(msg),
            AdapterErrorKind::Transport => FabricError::TransportFailure(msg),
            AdapterErrorKind::MalformedResponse => FabricError::MalformedOutput(msg),
            AdapterErrorKind::Quota => FabricError::QuotaExhausted(msg),
            AdapterErrorKind::Auth => FabricError::PolicyDenied(msg),
            AdapterErrorKind::Cancelled => {
                FabricError::PolicyDenied(format!("cancelled: {}", self.safe_detail))
            }
            AdapterErrorKind::Unknown => FabricError::TransportFailure(msg),
        }
    }
}

/// Bound the detail length; diagnostics stay small and redacted by policy.
fn redact_detail(detail: String) -> String {
    const MAX: usize = 512;
    if detail.len() <= MAX {
        detail
    } else {
        format!("{}…[truncated]", &detail[..MAX])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_flag_survives() {
        let e = AdapterError::retryable(AdapterErrorKind::Timeout, "slow");
        assert!(e.is_retryable());
        let f = AdapterError::fatal(AdapterErrorKind::Auth, "bad key");
        assert!(!f.is_retryable());
    }

    #[test]
    fn detail_is_bounded() {
        let e = AdapterError::fatal(AdapterErrorKind::Unknown, "x".repeat(2000));
        assert!(e.safe_detail.len() < 600);
    }

    #[test]
    fn maps_to_fabric_taxonomy() {
        let e = AdapterError::retryable(AdapterErrorKind::Quota, "429");
        assert!(matches!(e.to_fabric(), FabricError::QuotaExhausted(_)));
        let e = AdapterError::fatal(AdapterErrorKind::MalformedResponse, "bad json");
        assert!(matches!(e.to_fabric(), FabricError::MalformedOutput(_)));
    }
}
