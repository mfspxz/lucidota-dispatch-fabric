//! T010: shared structured errors.
//!
//! One error enum for the whole dispatch system so every subsystem classifies
//! failures the same way: policy denial, quota exhaustion, timeout, transport
//! failure, malformed output, conflicts, and verification failure are distinct
//! variants, never stringly-typed.

use thiserror::Error;

/// Structured error for the dispatch fabric (T010).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FabricError {
    /// A value failed validation. `field` is a static path, never user data.
    #[error("invalid input for {field}: {reason}")]
    InvalidInput { field: &'static str, reason: String },

    /// An illegal task state transition was attempted.
    #[error("illegal state transition {from} -> {to}")]
    IllegalTransition { from: &'static str, to: &'static str },

    /// Policy denied the action.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// A quota or budget limit was exhausted.
    #[error("quota exhausted: {0}")]
    QuotaExhausted(String),

    /// A deadline (connection, first-token, total-response, task) expired.
    #[error("timeout: {0}")]
    Timeout(String),

    /// The transport to a provider failed.
    #[error("transport failure: {0}")]
    TransportFailure(String),

    /// A provider or worker returned output that does not parse.
    #[error("malformed output: {0}")]
    MalformedOutput(String),

    /// Conflicting writes, duplicate IDs, or incompatible interfaces.
    #[error("conflict: {0}")]
    Conflict(String),

    /// A verifier evaluated an acceptance predicate as failed.
    #[error("verification failure: {0}")]
    VerificationFailure(String),

    /// A task class string matched no known class and no Unknown carrier.
    #[error("unknown task class: {0}")]
    UnknownClass(String),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, FabricError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_stable() {
        let e = FabricError::QuotaExhausted("tokens".to_string());
        assert_eq!(e.to_string(), "quota exhausted: tokens");
    }

    #[test]
    fn invalid_input_carries_field_and_reason() {
        let e = FabricError::InvalidInput {
            field: "task_id",
            reason: "empty".to_string(),
        };
        assert!(e.to_string().contains("task_id"));
    }
}
