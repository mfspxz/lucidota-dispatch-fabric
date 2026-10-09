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
    pub fn retryable(kind: AdapterErrorKind, detail: impl Into<String>) -> Self {
        let _ = detail;
        todo!("T039: implement retryable error constructor")
    }

    /// Build a non-retryable (fatal) error with a redacted detail string.
    pub fn fatal(kind: AdapterErrorKind, detail: impl Into<String>) -> Self {
        let _ = detail;
        todo!("T039: implement fatal error constructor")
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Map onto the shared structured error taxonomy (T010).
    pub fn to_fabric(&self) -> FabricError {
        todo!("T039: implement mapping to FabricError")
    }
}
