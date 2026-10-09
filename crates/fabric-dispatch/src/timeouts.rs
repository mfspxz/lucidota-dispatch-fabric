//! T063: typed deadline enforcement.
//!
//! Four independent deadlines — connection, first-token, total-response, and
//! task — each producing a typed [`FabricError::Timeout`] that names the
//! stage and the budget that expired. No stage's timeout implies another's.

use fabric_types::{FabricError, Result};
use std::future::Future;

/// The four deadline stages for one inference attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutStage {
    /// Establishing transport to the endpoint.
    Connect,
    /// Waiting for the first response token/byte.
    FirstToken,
    /// The whole response body.
    TotalResponse,
    /// The whole task attempt, end to end.
    Task,
}

impl TimeoutStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::FirstToken => "first-token",
            Self::TotalResponse => "total-response",
            Self::Task => "task",
        }
    }

    /// This stage's budget from a config.
    pub fn budget_ms(self, cfg: &TimeoutConfig) -> u64 {
        match self {
            Self::Connect => cfg.connect_ms,
            Self::FirstToken => cfg.first_token_ms,
            Self::TotalResponse => cfg.total_response_ms,
            Self::Task => cfg.task_ms,
        }
    }
}

/// Deadline budgets for one attempt. All budgets must be positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    pub connect_ms: u64,
    pub first_token_ms: u64,
    pub total_response_ms: u64,
    pub task_ms: u64,
}

impl TimeoutConfig {
    pub fn new(
        connect_ms: u64,
        first_token_ms: u64,
        total_response_ms: u64,
        task_ms: u64,
    ) -> Result<Self> {
        for (field, v) in [
            ("connect_ms", connect_ms),
            ("first_token_ms", first_token_ms),
            ("total_response_ms", total_response_ms),
            ("task_ms", task_ms),
        ] {
            if v == 0 {
                return Err(FabricError::InvalidInput {
                    field,
                    reason: "timeout budgets must be positive".to_string(),
                });
            }
        }
        Ok(Self {
            connect_ms,
            first_token_ms,
            total_response_ms,
            task_ms,
        })
    }

    /// Sane defaults: 10s connect, 60s first token, 10min response, 30min task.
    pub fn generous() -> Self {
        Self {
            connect_ms: 10_000,
            first_token_ms: 60_000,
            total_response_ms: 600_000,
            task_ms: 1_800_000,
        }
    }
}

/// The typed timeout error for a stage. Always [`FabricError::Timeout`], with
/// the stage name and expired budget embedded.
pub fn stage_timeout(stage: TimeoutStage, budget_ms: u64) -> FabricError {
    FabricError::Timeout(format!(
        "{} deadline exceeded after {}ms",
        stage.name(),
        budget_ms
    ))
}

/// Run `fut` under one stage's deadline. On expiry returns the typed stage
/// timeout; the inner future's own errors pass through untouched.
pub async fn enforce<F, T>(
    stage: TimeoutStage,
    timeout_ms: u64,
    fut: F,
) -> std::result::Result<T, FabricError>
where
    F: Future<Output = std::result::Result<T, FabricError>>,
{
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Ok(inner) => inner,
        Err(_) => Err(stage_timeout(stage, timeout_ms)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_budgets() {
        assert!(TimeoutConfig::new(0, 1, 1, 1).is_err());
        assert!(TimeoutConfig::new(1, 1, 1, 1).is_ok());
    }

    #[test]
    fn stage_timeout_error_is_typed() {
        let e = stage_timeout(TimeoutStage::FirstToken, 5000);
        match e {
            FabricError::Timeout(msg) => {
                assert!(msg.contains("first-token"));
                assert!(msg.contains("5000ms"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn enforce_passes_fast_future() {
        let r = enforce(TimeoutStage::Connect, 1000, async { Ok::<_, FabricError>(42) }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn enforce_times_out_pending_future() {
        // Real (short) durations: no test-util feature needed.
        let r = enforce(
            TimeoutStage::TotalResponse,
            50,
            async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok::<_, FabricError>(())
            },
        )
        .await;
        match r {
            Err(FabricError::Timeout(msg)) => assert!(msg.contains("total-response")),
            _ => panic!("expected timeout"),
        }
    }

    #[tokio::test]
    async fn enforce_passes_inner_errors_through() {
        let r: std::result::Result<(), FabricError> = enforce(
            TimeoutStage::Task,
            1000,
            async { Err(FabricError::TransportFailure("boom".to_string())) },
        )
        .await;
        assert!(matches!(r, Err(FabricError::TransportFailure(_))));
    }
}
