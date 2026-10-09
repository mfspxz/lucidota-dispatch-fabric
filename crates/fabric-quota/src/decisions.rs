//! T050: dispatch decision records.
//!
//! Every dispatch outcome — dispatched, delayed, rerouted, or denied — is
//! recorded with the task, the attempt, the instant, the policy or limit that
//! drove it (`policy_ref`), and a human-readable reason. This is the audit
//! trail answering "why was this task dispatched, delayed, rerouted, or
//! denied".

use crate::UnixMillis;
use fabric_types::TaskId;
use serde::{Deserialize, Serialize};

/// The four dispatch outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionKind {
    /// Sent to an endpoint.
    Dispatched { endpoint: String },
    /// Held back; may be retried at `retry_at_ms` when known.
    Delayed {
        reason: String,
        retry_at_ms: Option<UnixMillis>,
    },
    /// Moved from one endpoint to another.
    Rerouted {
        from: String,
        to: String,
        reason: String,
    },
    /// Refused outright.
    Denied { reason: String },
}

/// One recorded dispatch decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchDecision {
    pub task_id: TaskId,
    pub attempt: u32,
    pub at_ms: UnixMillis,
    /// What drove the decision, e.g. `"quota:TOKEN_RATE"`, `"budget"`,
    /// `"fairness"`, `"adaptive"`, `"reset-window"`, `"policy"`.
    pub policy_ref: String,
    pub kind: DecisionKind,
}

impl DispatchDecision {
    pub fn new(
        task_id: TaskId,
        attempt: u32,
        at_ms: UnixMillis,
        policy_ref: impl Into<String>,
        kind: DecisionKind,
    ) -> Self {
        Self {
            task_id,
            attempt,
            at_ms,
            policy_ref: policy_ref.into(),
            kind,
        }
    }

    /// True for [`DecisionKind::Dispatched`].
    pub fn is_dispatched(&self) -> bool {
        matches!(self.kind, DecisionKind::Dispatched { .. })
    }

    /// True for [`DecisionKind::Denied`].
    pub fn is_denied(&self) -> bool {
        matches!(self.kind, DecisionKind::Denied { .. })
    }
}

/// Append-only log of dispatch decisions.
#[derive(Debug, Clone, Default)]
pub struct DecisionLog {
    entries: Vec<DispatchDecision>,
}

impl DecisionLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, decision: DispatchDecision) {
        self.entries.push(decision);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, DispatchDecision> {
        self.entries.iter()
    }

    /// All decisions for one task, in record order.
    pub fn for_task(&self, task_id: &TaskId) -> Vec<&DispatchDecision> {
        self.entries
            .iter()
            .filter(|d| &d.task_id == task_id)
            .collect()
    }

    /// All denials, in record order.
    pub fn denied(&self) -> Vec<&DispatchDecision> {
        self.entries.iter().filter(|d| d.is_denied()).collect()
    }

    /// All decisions driven by one policy reference.
    pub fn by_policy(&self, policy_ref: &str) -> Vec<&DispatchDecision> {
        self.entries
            .iter()
            .filter(|d| d.policy_ref == policy_ref)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(kind: DecisionKind) -> DispatchDecision {
        DispatchDecision::new(
            TaskId::new("T001").unwrap(),
            1,
            1_000,
            "quota:TOKEN_RATE",
            kind,
        )
    }

    #[test]
    fn records_and_queries() {
        let mut log = DecisionLog::new();
        assert!(log.is_empty());
        log.record(decision(DecisionKind::Dispatched {
            endpoint: "ep1".to_string(),
        }));
        log.record(decision(DecisionKind::Denied {
            reason: "over budget".to_string(),
        }));
        log.record(DispatchDecision::new(
            TaskId::new("T002").unwrap(),
            1,
            2_000,
            "fairness",
            DecisionKind::Delayed {
                reason: "class weight".to_string(),
                retry_at_ms: Some(5_000),
            },
        ));
        log.record(DispatchDecision::new(
            TaskId::new("T001").unwrap(),
            2,
            3_000,
            "adaptive",
            DecisionKind::Rerouted {
                from: "ep1".to_string(),
                to: "ep2".to_string(),
                reason: "ep1 throttled".to_string(),
            },
        ));
        assert_eq!(log.len(), 4);
        assert_eq!(log.for_task(&TaskId::new("T001").unwrap()).len(), 3);
        assert_eq!(log.for_task(&TaskId::new("T002").unwrap()).len(), 1);
        assert_eq!(log.denied().len(), 1);
        assert_eq!(log.by_policy("quota:TOKEN_RATE").len(), 2);
        assert_eq!(log.by_policy("fairness").len(), 1);
    }

    #[test]
    fn decision_predicates() {
        let d = decision(DecisionKind::Dispatched {
            endpoint: "e".to_string(),
        });
        assert!(d.is_dispatched());
        assert!(!d.is_denied());
        let d2 = decision(DecisionKind::Denied {
            reason: "r".to_string(),
        });
        assert!(d2.is_denied());
    }

    #[test]
    fn round_trips() {
        let d = decision(DecisionKind::Rerouted {
            from: "a".to_string(),
            to: "b".to_string(),
            reason: "why".to_string(),
        });
        let j = serde_json::to_string(&d).unwrap();
        let back: DispatchDecision = serde_json::from_str(&j).unwrap();
        assert_eq!(d, back);
    }
}
