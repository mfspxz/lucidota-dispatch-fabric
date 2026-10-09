//! T059: machine-readable blocked-task diagnostics.
//!
//! For a task that cannot proceed, produce a [`BlockedReport`]: one
//! [`BlockReason`] per unmet prerequisite (each naming the dependency and its
//! state — waiting, failed, or cancelled) plus a concrete `dependency_path`
//! from the task down to the root blocker. Everything is `Serialize`, so the
//! report is directly machine-readable (JSON) with deterministic ordering.

use crate::nodes::{DepEdge, EdgeKind};
use fabric_types::{FabricError, Result, TaskId, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Why one prerequisite blocks a task. Distinct variants, never inferred.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlockReason {
    /// The dependency has not finished yet; carries its current state.
    WaitingOn {
        dependency: TaskId,
        state: TaskState,
    },
    /// The dependency failed its acceptance gate.
    DependencyFailed { dependency: TaskId },
    /// The dependency was cancelled and will never complete.
    DependencyCancelled { dependency: TaskId },
}

impl BlockReason {
    pub fn dependency(&self) -> &TaskId {
        match self {
            BlockReason::WaitingOn { dependency, .. } => dependency,
            BlockReason::DependencyFailed { dependency } => dependency,
            BlockReason::DependencyCancelled { dependency } => dependency,
        }
    }
}

/// Machine-readable diagnostic for a blocked task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedReport {
    pub task: TaskId,
    /// One reason per unmet prerequisite, sorted by dependency ID.
    pub reasons: Vec<BlockReason>,
    /// Concrete path `task -> ... -> root blocker` following `DependsOn`
    /// edges, always starting at `task`. Cycle-guarded.
    pub dependency_path: Vec<TaskId>,
}

/// Prerequisites of `of` that have not succeeded, as `(id, state)` pairs,
/// ID-sorted. Fail-closed: a prerequisite with unknown state is an error,
/// never a guess.
fn blocking_deps(
    of: &TaskId,
    edges: &[DepEdge],
    states: &HashMap<TaskId, TaskState>,
) -> Result<Vec<(TaskId, TaskState)>> {
    let mut prereqs: Vec<&TaskId> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::DependsOn && e.from == *of)
        .map(|e| &e.to)
        .collect();
    prereqs.sort();
    prereqs.dedup();
    let mut blocking = Vec::new();
    for dep in prereqs {
        let state = states.get(dep).ok_or_else(|| FabricError::InvalidInput {
            field: "states",
            reason: format!("missing state for prerequisite {}", dep.as_str()),
        })?;
        if *state != TaskState::Succeeded {
            blocking.push((dep.clone(), *state));
        }
    }
    Ok(blocking)
}

fn reason_for(dependency: TaskId, state: TaskState) -> BlockReason {
    match state {
        TaskState::Failed => BlockReason::DependencyFailed { dependency },
        TaskState::Cancelled => BlockReason::DependencyCancelled { dependency },
        TaskState::Succeeded => unreachable!("blocking_deps never returns succeeded tasks"),
        other => BlockReason::WaitingOn {
            dependency,
            state: other,
        },
    }
}

/// Diagnose why `task` is blocked.
///
/// Returns `Ok(None)` when the task is not blocked (every prerequisite has
/// `Succeeded`, or it has no prerequisites). Returns an error when a
/// prerequisite's state is unknown (fail-closed: no guessing).
pub fn diagnose_blocked(
    task: &TaskId,
    edges: &[DepEdge],
    states: &HashMap<TaskId, TaskState>,
) -> Result<Option<BlockedReport>> {
    let blocking = blocking_deps(task, edges, states)?;
    if blocking.is_empty() {
        return Ok(None);
    }

    let mut reasons: Vec<BlockReason> = blocking
        .iter()
        .map(|(dep, state)| reason_for(dep.clone(), *state))
        .collect();
    reasons.sort_by(|a, b| a.dependency().cmp(b.dependency()));

    // Dependency path: from the task, follow the ID-smallest blocking
    // dependency at each level down to the root blocker (a blocking task
    // whose own prerequisites have all succeeded, i.e. the true root cause).
    // Cycle-guarded.
    let mut path = vec![task.clone()];
    let mut visited: HashSet<TaskId> = HashSet::from([task.clone()]);
    let mut current = task.clone();
    loop {
        let next_blocking = blocking_deps(&current, edges, states)?;
        match next_blocking.into_iter().next() {
            Some((dep, _)) if visited.insert(dep.clone()) => {
                path.push(dep.clone());
                current = dep;
            }
            _ => break,
        }
    }

    Ok(Some(BlockedReport {
        task: task.clone(),
        reasons,
        dependency_path: path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            from: TaskId::new(from).unwrap(),
            to: TaskId::new(to).unwrap(),
            kind: EdgeKind::DependsOn,
        }
    }

    fn states(pairs: &[(&str, TaskState)]) -> HashMap<TaskId, TaskState> {
        pairs
            .iter()
            .map(|(id, s)| (TaskId::new(*id).unwrap(), *s))
            .collect()
    }

    #[test]
    fn reports_failed_dependency_with_path() {
        // C depends on B depends on A; B failed.
        let edges = vec![edge("C", "B"), edge("B", "A")];
        let st = states(&[
            ("A", TaskState::Succeeded),
            ("B", TaskState::Failed),
            ("C", TaskState::Blocked),
        ]);
        let report = diagnose_blocked(&TaskId::new("C").unwrap(), &edges, &st)
            .unwrap()
            .expect("C is blocked");
        assert_eq!(report.task.as_str(), "C");
        assert_eq!(report.reasons.len(), 1);
        assert!(matches!(
            &report.reasons[0],
            BlockReason::DependencyFailed { dependency } if dependency.as_str() == "B"
        ));
        let path: Vec<_> = report.dependency_path.iter().map(|t| t.as_str()).collect();
        assert_eq!(path, vec!["C", "B"]);
    }

    #[test]
    fn path_walks_to_root_blocker() {
        // D -> C -> B -> A; B failed (root cause), C merely waiting, A done.
        let edges = vec![edge("D", "C"), edge("C", "B"), edge("B", "A")];
        let st = states(&[
            ("A", TaskState::Succeeded),
            ("B", TaskState::Failed),
            ("C", TaskState::Blocked),
            ("D", TaskState::Blocked),
        ]);
        let report = diagnose_blocked(&TaskId::new("D").unwrap(), &edges, &st)
            .unwrap()
            .expect("D is blocked");
        // D's own reason names C (waiting)...
        assert!(matches!(
            &report.reasons[0],
            BlockReason::WaitingOn { state: TaskState::Blocked, .. }
        ));
        // ...but the path walks down to the root cause B.
        let path: Vec<_> = report.dependency_path.iter().map(|t| t.as_str()).collect();
        assert_eq!(path, vec!["D", "C", "B"]);
    }

    #[test]
    fn distinguishes_waiting_failed_cancelled() {
        // D depends on B (running), C (failed), E (cancelled).
        let edges = vec![edge("D", "B"), edge("D", "C"), edge("D", "E")];
        let st = states(&[
            ("B", TaskState::Running),
            ("C", TaskState::Failed),
            ("E", TaskState::Cancelled),
            ("D", TaskState::Blocked),
        ]);
        let report = diagnose_blocked(&TaskId::new("D").unwrap(), &edges, &st)
            .unwrap()
            .expect("D is blocked");
        assert_eq!(report.reasons.len(), 3);
        // Sorted by dependency ID: B, C, E.
        assert!(matches!(
            &report.reasons[0],
            BlockReason::WaitingOn { state: TaskState::Running, .. }
        ));
        assert!(matches!(&report.reasons[1], BlockReason::DependencyFailed { .. }));
        assert!(matches!(
            &report.reasons[2],
            BlockReason::DependencyCancelled { .. }
        ));
    }

    #[test]
    fn not_blocked_returns_none() {
        let edges = vec![edge("B", "A")];
        let st = states(&[("A", TaskState::Succeeded), ("B", TaskState::Ready)]);
        assert!(diagnose_blocked(&TaskId::new("B").unwrap(), &edges, &st)
            .unwrap()
            .is_none());
        // No prerequisites at all: also not blocked.
        assert!(diagnose_blocked(&TaskId::new("Z").unwrap(), &edges, &st)
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_state_is_an_error() {
        let edges = vec![edge("B", "A")];
        let st = states(&[("B", TaskState::Blocked)]);
        assert!(diagnose_blocked(&TaskId::new("B").unwrap(), &edges, &st).is_err());
    }

    #[test]
    fn report_serializes_to_stable_json() {
        let edges = vec![edge("C", "B")];
        let st = states(&[("B", TaskState::Failed), ("C", TaskState::Blocked)]);
        let report = diagnose_blocked(&TaskId::new("C").unwrap(), &edges, &st)
            .unwrap()
            .unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"task\":\"C\""));
        assert!(json.contains("DEPENDENCY_FAILED"));
        assert!(json.contains("\"dependency_path\":[\"C\",\"B\"]"));
        // Deterministic across repeated serialization.
        assert_eq!(json, serde_json::to_string(&report).unwrap());
    }
}
