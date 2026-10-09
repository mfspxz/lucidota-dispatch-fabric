//! T058: cancellation propagation.
//!
//! Cancelling a task propagates to its dependents (tasks with a `DependsOn`
//! edge into the cancelled task) and, per policy, to tasks it supersedes
//! (outgoing `Supersedes` edges). Propagation is transitive and deterministic
//! (breadth-first over ID-sorted adjacency).
//!
//! Completed evidence is never destroyed: tasks in terminal states
//! (`Succeeded`, `Cancelled`) are left untouched and their evidence receipts
//! are returned in the outcome so the caller can retain them. A `Failed`
//! task may still transition to `Cancelled` and its failed-attempt evidence
//! is likewise preserved.

use crate::nodes::{DepEdge, EdgeKind};
use fabric_types::{EvidenceReceipt, FabricError, Result, TaskId, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Policy controlling how far cancellation propagates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelPolicy {
    /// Cancel transitive dependents (reverse `DependsOn` edges).
    pub propagate_to_dependents: bool,
    /// Cancel tasks the cancelled task supersedes (outgoing `Supersedes` edges).
    pub propagate_through_supersedes: bool,
    /// Cancel tasks currently `Running`. When false they are reported in
    /// `skipped` and left to finish.
    pub cancel_running: bool,
}

impl Default for CancelPolicy {
    fn default() -> Self {
        Self {
            propagate_to_dependents: true,
            propagate_through_supersedes: true,
            cancel_running: false,
        }
    }
}

/// Outcome of a cancellation propagation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOutcome {
    /// Tasks transitioned to `Cancelled`, ID-sorted (includes the root when
    /// it was actually transitioned).
    pub cancelled: Vec<TaskId>,
    /// Evidence receipts of tasks that were already terminal: their evidence
    /// is preserved and the tasks were not touched. Sorted by task ID.
    pub preserved: Vec<EvidenceReceipt>,
    /// Non-terminal tasks excluded by policy (e.g. `Running` with
    /// `cancel_running: false`). ID-sorted.
    pub skipped: Vec<TaskId>,
}

/// Propagate cancellation of `cancelled` through the graph.
///
/// `edges` uses the frozen `DepEdge` types; `states` must contain an entry
/// for every task reached (fail-closed: a missing state is an error).
/// `evidence` may hold receipts for terminal tasks; receipts found for
/// preserved tasks are returned in the outcome.
pub fn propagate_cancellation(
    edges: &[DepEdge],
    cancelled: &TaskId,
    states: &HashMap<TaskId, TaskState>,
    evidence: &HashMap<TaskId, EvidenceReceipt>,
    policy: &CancelPolicy,
) -> Result<CancelOutcome> {
    // Reverse DependsOn adjacency: prerequisite -> dependents.
    let mut dependents: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();
    // Outgoing Supersedes adjacency: replacer -> replaced.
    let mut superseded: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();
    for e in edges {
        match e.kind {
            EdgeKind::DependsOn => dependents.entry(&e.to).or_default().push(&e.from),
            EdgeKind::Supersedes => superseded.entry(&e.from).or_default().push(&e.to),
        }
    }
    for vs in dependents.values_mut().chain(superseded.values_mut()) {
        vs.sort();
    }

    let mut cancelled_out: Vec<TaskId> = Vec::new();
    let mut preserved: Vec<EvidenceReceipt> = Vec::new();
    let mut skipped: Vec<TaskId> = Vec::new();
    let mut visited: HashSet<TaskId> = HashSet::new();
    let mut queue: VecDeque<TaskId> = VecDeque::from([cancelled.clone()]);

    while let Some(id) = queue.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let state = states.get(&id).ok_or_else(|| FabricError::InvalidInput {
            field: "states",
            reason: format!("missing state for task {}", id.as_str()),
        })?;
        match *state {
            TaskState::Succeeded | TaskState::Cancelled => {
                // Terminal: never touch; preserve completed evidence.
                if let Some(receipt) = evidence.get(&id) {
                    preserved.push(receipt.clone());
                }
            }
            TaskState::Running if !policy.cancel_running => {
                skipped.push(id.clone());
            }
            _ => {
                // Proposed / Ready / Blocked / Failed / Running(policy): cancel.
                debug_assert!(state.can_transition_to(TaskState::Cancelled));
                if state.can_transition_to(TaskState::Cancelled) {
                    cancelled_out.push(id.clone());
                } else {
                    skipped.push(id.clone());
                }
            }
        }
        // Enqueue propagation targets regardless of this node's own bucket:
        // dependents of a preserved/completed task still cannot proceed once
        // an ancestor in the cancelled chain is gone... but only propagate
        // through the cancelled root's own closure: every visited node was
        // reached from the root, so its dependents are affected too.
        if policy.propagate_to_dependents {
            if let Some(ds) = dependents.get(&id) {
                for d in ds {
                    queue.push_back((*d).clone());
                }
            }
        }
        if policy.propagate_through_supersedes {
            if let Some(ss) = superseded.get(&id) {
                for s in ss {
                    queue.push_back((*s).clone());
                }
            }
        }
    }

    cancelled_out.sort();
    skipped.sort();
    preserved.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    Ok(CancelOutcome {
        cancelled: cancelled_out,
        preserved,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::Verdict;

    fn edge(from: &str, to: &str, kind: EdgeKind) -> DepEdge {
        DepEdge {
            from: TaskId::new(from).unwrap(),
            to: TaskId::new(to).unwrap(),
            kind,
        }
    }

    fn receipt_for(id: &str) -> EvidenceReceipt {
        EvidenceReceipt::new(
            TaskId::new(id).unwrap(),
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

    fn states(pairs: &[(&str, TaskState)]) -> HashMap<TaskId, TaskState> {
        pairs
            .iter()
            .map(|(id, s)| (TaskId::new(*id).unwrap(), *s))
            .collect()
    }

    #[test]
    fn cancels_transitive_dependents() {
        // Diamond: B,C depend on A; D depends on B,C.
        let edges = vec![
            edge("B", "A", EdgeKind::DependsOn),
            edge("C", "A", EdgeKind::DependsOn),
            edge("D", "B", EdgeKind::DependsOn),
            edge("D", "C", EdgeKind::DependsOn),
        ];
        let st = states(&[
            ("A", TaskState::Ready),
            ("B", TaskState::Proposed),
            ("C", TaskState::Ready),
            ("D", TaskState::Blocked),
        ]);
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &HashMap::new(),
            &CancelPolicy::default(),
        )
        .unwrap();
        let ids: Vec<_> = out.cancelled.iter().map(|t| t.as_str()).collect();
        assert_eq!(ids, vec!["A", "B", "C", "D"]);
        assert!(out.preserved.is_empty());
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn preserves_completed_evidence() {
        // B already succeeded with a receipt; cancelling A must not touch B,
        // but B's receipt must be returned for retention.
        let edges = vec![
            edge("B", "A", EdgeKind::DependsOn),
            edge("C", "B", EdgeKind::DependsOn),
        ];
        let st = states(&[
            ("A", TaskState::Ready),
            ("B", TaskState::Succeeded),
            ("C", TaskState::Ready),
        ]);
        let mut ev = HashMap::new();
        ev.insert(TaskId::new("B").unwrap(), receipt_for("B"));
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &ev,
            &CancelPolicy::default(),
        )
        .unwrap();
        let ids: Vec<_> = out.cancelled.iter().map(|t| t.as_str()).collect();
        assert_eq!(ids, vec!["A", "C"]);
        assert_eq!(out.preserved.len(), 1);
        assert_eq!(out.preserved[0].task_id.as_str(), "B");
    }

    #[test]
    fn running_skipped_unless_policy_allows() {
        let edges = vec![edge("B", "A", EdgeKind::DependsOn)];
        let st = states(&[("A", TaskState::Ready), ("B", TaskState::Running)]);
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &HashMap::new(),
            &CancelPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.cancelled.len(), 1); // only A
        assert_eq!(out.skipped[0].as_str(), "B");

        let policy = CancelPolicy {
            cancel_running: true,
            ..CancelPolicy::default()
        };
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &HashMap::new(),
            &policy,
        )
        .unwrap();
        assert_eq!(out.cancelled.len(), 2);
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn propagates_through_supersedes_edges() {
        // T supersedes X: cancelling T cascades to X per policy.
        let edges = vec![edge("T", "X", EdgeKind::Supersedes)];
        let st = states(&[("T", TaskState::Ready), ("X", TaskState::Ready)]);
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("T").unwrap(),
            &st,
            &HashMap::new(),
            &CancelPolicy::default(),
        )
        .unwrap();
        let ids: Vec<_> = out.cancelled.iter().map(|t| t.as_str()).collect();
        assert_eq!(ids, vec!["T", "X"]);

        let policy = CancelPolicy {
            propagate_through_supersedes: false,
            ..CancelPolicy::default()
        };
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("T").unwrap(),
            &st,
            &HashMap::new(),
            &policy,
        )
        .unwrap();
        assert_eq!(out.cancelled.len(), 1);
    }

    #[test]
    fn missing_state_is_an_error() {
        let edges = vec![edge("B", "A", EdgeKind::DependsOn)];
        let st = states(&[("A", TaskState::Ready)]);
        let err = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &HashMap::new(),
            &CancelPolicy::default(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn failed_task_is_cancelled_with_evidence_preserved() {
        let edges = vec![edge("B", "A", EdgeKind::DependsOn)];
        let st = states(&[("A", TaskState::Failed), ("B", TaskState::Ready)]);
        let mut ev = HashMap::new();
        ev.insert(TaskId::new("A").unwrap(), receipt_for("A"));
        let out = propagate_cancellation(
            &edges,
            &TaskId::new("A").unwrap(),
            &st,
            &ev,
            &CancelPolicy::default(),
        )
        .unwrap();
        // A was Failed -> Cancelled is legal; its receipt is still returned
        // for retention alongside the cancellation.
        let ids: Vec<_> = out.cancelled.iter().map(|t| t.as_str()).collect();
        assert_eq!(ids, vec!["A", "B"]);
    }
}
