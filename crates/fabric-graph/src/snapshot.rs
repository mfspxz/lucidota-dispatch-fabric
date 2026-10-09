//! T060: stable queue snapshots and decision replay.
//!
//! [`take_snapshot`] serializes the queue into a canonical, ID-sorted form
//! with a content digest, so two snapshots of the same queue state are
//! byte-identical. [`replay`] re-applies a recorded [`DecisionLog`] to a
//! snapshot, validating every state change through the frozen `TaskState`
//! transition table — an illegal transition fails the replay instead of
//! being silently applied.

use fabric_types::{FabricError, Result, TaskClass, TaskId, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One task's state at snapshot time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub task: TaskId,
    pub state: TaskState,
    pub class: TaskClass,
}

/// A stable, content-addressed snapshot of the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    /// `snap-` + first 12 hex chars of the digest.
    pub snapshot_id: String,
    pub taken_at_millis: u64,
    /// Entries in TaskId-sorted order. Always sorted, never insertion order.
    pub entries: Vec<SnapshotEntry>,
    /// FNV-1a hex over the canonical entry encoding. Integrity digest only,
    /// not a security boundary.
    pub digest: String,
}

/// FNV-1a 64-bit hex. Deterministic across runs.
fn fnv1a_hex(data: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Capture a stable snapshot of the queue. Input order is irrelevant: entries
/// are sorted by task ID before digesting. A task appearing twice is a
/// conflict, never silently merged.
pub fn take_snapshot(
    tasks: &[(TaskId, TaskState, TaskClass)],
    taken_at_millis: u64,
) -> Result<QueueSnapshot> {
    let mut seen: HashSet<TaskId> = HashSet::new();
    for (task, _, _) in tasks {
        if !seen.insert(task.clone()) {
            return Err(FabricError::Conflict(format!(
                "duplicate task {} in snapshot input",
                task.as_str(),
            )));
        }
    }
    let mut entries: Vec<SnapshotEntry> = tasks
        .iter()
        .map(|(task, state, class)| SnapshotEntry {
            task: task.clone(),
            state: *state,
            class: class.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.task.cmp(&b.task));
    let canonical: Vec<String> = entries
        .iter()
        .map(|e| format!("{}|{:?}|{}", e.task.as_str(), e.state, e.class))
        .collect();
    let digest = fnv1a_hex(&canonical.join("\n"));
    Ok(QueueSnapshot {
        snapshot_id: format!("snap-{}", &digest[..12]),
        taken_at_millis,
        entries,
        digest,
    })
}

/// What the scheduler decided for one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionAction {
    /// Admit to the ready set: `Proposed -> Ready`.
    Admitted,
    /// Dispatch to a worker: `Ready -> Running`.
    Dispatched,
    /// Defer: no state change.
    Deferred,
    /// Refuse the task: any non-terminal state `-> Cancelled`.
    Rejected,
}

/// One recorded scheduling decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingDecision {
    /// Monotonic sequence number within the log. Must be unique.
    pub seq: u64,
    pub task: TaskId,
    pub action: DecisionAction,
    pub reason: String,
}

/// The recorded inputs replay consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionLog {
    pub decisions: Vec<SchedulingDecision>,
}

/// Outcome of replaying a decision log against a snapshot.
#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    /// Final state per task after all decisions, including untouched tasks.
    pub final_states: HashMap<TaskId, TaskState>,
    /// Number of decisions applied (including no-op `Deferred`).
    pub applied: usize,
}

/// Replay a recorded decision log against a snapshot.
///
/// Decisions apply in `seq` order (sorted; the log's storage order does not
/// matter). Every state change goes through `TaskState::transition`, so a
/// decision that would be an illegal transition fails the whole replay with
/// `IllegalTransition` instead of corrupting state. Unknown tasks and
/// duplicate sequence numbers are rejected.
pub fn replay(log: &DecisionLog, snapshot: &QueueSnapshot) -> Result<ReplayOutcome> {
    let mut ordered: Vec<&SchedulingDecision> = log.decisions.iter().collect();
    ordered.sort_by_key(|d| d.seq);
    let mut seen_seq: HashSet<u64> = HashSet::new();
    let mut final_states: HashMap<TaskId, TaskState> = snapshot
        .entries
        .iter()
        .map(|e| (e.task.clone(), e.state))
        .collect();

    let mut applied = 0;
    for d in ordered {
        if !seen_seq.insert(d.seq) {
            return Err(FabricError::InvalidInput {
                field: "decision_log",
                reason: format!("duplicate sequence number {}", d.seq),
            });
        }
        let state = final_states.get(&d.task).ok_or_else(|| FabricError::InvalidInput {
            field: "decision_log",
            reason: format!("decision for unknown task {}", d.task.as_str()),
        })?;
        let next = match d.action {
            DecisionAction::Admitted => state.transition(TaskState::Ready)?,
            DecisionAction::Dispatched => state.transition(TaskState::Running)?,
            DecisionAction::Deferred => *state,
            DecisionAction::Rejected => {
                if *state == TaskState::Succeeded {
                    return Err(FabricError::IllegalTransition {
                        from: state.name(),
                        to: TaskState::Cancelled.name(),
                    });
                }
                state.transition(TaskState::Cancelled)?
            }
        };
        final_states.insert(d.task.clone(), next);
        applied += 1;
    }
    Ok(ReplayOutcome {
        final_states,
        applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> QueueSnapshot {
        // Deliberately unsorted input: snapshot must canonicalize.
        take_snapshot(
            &[
                (TaskId::new("T003").unwrap(), TaskState::Proposed, TaskClass::Review),
                (TaskId::new("T001").unwrap(), TaskState::Ready, TaskClass::Implement),
                (TaskId::new("T002").unwrap(), TaskState::Proposed, TaskClass::Verify),
            ],
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn snapshot_is_stable_and_sorted() {
        let s = snap();
        let ids: Vec<_> = s.entries.iter().map(|e| e.task.as_str()).collect();
        assert_eq!(ids, vec!["T001", "T002", "T003"]);
        assert!(s.snapshot_id.starts_with("snap-"));
        assert_eq!(s.snapshot_id.len(), "snap-".len() + 12);
        // Same logical queue, different input order -> identical snapshot.
        let s2 = take_snapshot(
            &[
                (TaskId::new("T001").unwrap(), TaskState::Ready, TaskClass::Implement),
                (TaskId::new("T002").unwrap(), TaskState::Proposed, TaskClass::Verify),
                (TaskId::new("T003").unwrap(), TaskState::Proposed, TaskClass::Review),
            ],
            1_000,
        )
        .unwrap();
        assert_eq!(s.digest, s2.digest);
        assert_eq!(s.snapshot_id, s2.snapshot_id);
    }

    #[test]
    fn duplicate_task_in_input_is_rejected() {
        let err = take_snapshot(
            &[
                (TaskId::new("T001").unwrap(), TaskState::Ready, TaskClass::Implement),
                (TaskId::new("T001").unwrap(), TaskState::Running, TaskClass::Implement),
            ],
            1_000,
        );
        assert!(matches!(err, Err(FabricError::Conflict(_))));
    }

    #[test]
    fn snapshot_json_is_deterministic() {
        let a = serde_json::to_string(&snap()).unwrap();
        let b = serde_json::to_string(&snap()).unwrap();
        assert_eq!(a, b);
        let back: QueueSnapshot = serde_json::from_str(&a).unwrap();
        assert_eq!(back, snap());
    }

    fn log(decisions: Vec<(u64, &str, DecisionAction)>) -> DecisionLog {
        DecisionLog {
            decisions: decisions
                .into_iter()
                .map(|(seq, task, action)| SchedulingDecision {
                    seq,
                    task: TaskId::new(task).unwrap(),
                    action,
                    reason: "test".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn replay_applies_decisions_in_seq_order() {
        // Log stored out of order; replay must sort by seq.
        let l = log(vec![
            (2, "T001", DecisionAction::Dispatched),
            (1, "T003", DecisionAction::Admitted),
            (3, "T002", DecisionAction::Deferred),
            (4, "T002", DecisionAction::Rejected),
        ]);
        let out = replay(&l, &snap()).unwrap();
        assert_eq!(out.applied, 4);
        assert_eq!(out.final_states[&TaskId::new("T001").unwrap()], TaskState::Running);
        assert_eq!(out.final_states[&TaskId::new("T003").unwrap()], TaskState::Ready);
        assert_eq!(out.final_states[&TaskId::new("T002").unwrap()], TaskState::Cancelled);
    }

    #[test]
    fn replay_rejects_illegal_transition() {
        // T003 is Proposed; Dispatched requires Ready first.
        let l = log(vec![(1, "T003", DecisionAction::Dispatched)]);
        let err = replay(&l, &snap());
        assert!(matches!(err, Err(FabricError::IllegalTransition { .. })));
    }

    #[test]
    fn replay_rejects_unknown_task_and_dup_seq() {
        let l = log(vec![(1, "T999", DecisionAction::Admitted)]);
        assert!(replay(&l, &snap()).is_err());
        let l = log(vec![
            (1, "T003", DecisionAction::Admitted),
            (1, "T002", DecisionAction::Admitted),
        ]);
        assert!(replay(&l, &snap()).is_err());
    }

    #[test]
    fn replay_refuses_to_cancel_succeeded() {
        let s = take_snapshot(
            &[(TaskId::new("T001").unwrap(), TaskState::Succeeded, TaskClass::Implement)],
            1_000,
        )
        .unwrap();
        let l = log(vec![(1, "T001", DecisionAction::Rejected)]);
        assert!(replay(&l, &s).is_err());
    }
}
