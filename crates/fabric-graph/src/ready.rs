//! T052: ready-set computation.
//!
//! A task is ready when every one of its DependsOn prerequisites has passed
//! its acceptance gate (verdict `Pass`) and the task itself has not already
//! passed. Gate verdicts come from the evidence ledger; a missing entry means
//! the gate has not been evaluated and does not count as passed.

use crate::nodes::TaskGraph;
use fabric_types::{TaskId, Verdict};
use std::collections::HashMap;

/// Acceptance-gate verdict per task. Absent = gate not yet evaluated.
pub type GateVerdicts = HashMap<TaskId, Verdict>;

/// Compute the ready set: tasks whose required dependencies have passed
/// their acceptance gates and which have not themselves passed yet.
/// Output is ID-sorted and therefore deterministic.
pub fn ready_tasks(graph: &TaskGraph, gates: &GateVerdicts) -> Vec<TaskId> {
    graph
        .nodes()
        .into_iter()
        .map(|n| &n.id)
        .filter(|id| {
            // Already-passed tasks are complete, not ready.
            if gates.get(*id).is_some_and(|v| v.is_pass()) {
                return false;
            }
            graph
                .prerequisites(id)
                .iter()
                .all(|p| gates.get(p).is_some_and(|v| v.is_pass()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::{test_node, DepEdge, EdgeKind};
    use fabric_types::TaskClass;

    fn diamond() -> TaskGraph {
        // T003 depends on T001, T002; T004 depends on T003.
        let mut g = TaskGraph::new();
        for id in ["T001", "T002", "T003", "T004"] {
            g.add_node(test_node(id, TaskClass::Implement)).unwrap();
        }
        for (from, to) in [("T003", "T001"), ("T003", "T002"), ("T004", "T003")] {
            g.add_edge(DepEdge {
                from: TaskId::new(from).unwrap(),
                to: TaskId::new(to).unwrap(),
                kind: EdgeKind::DependsOn,
            })
            .unwrap();
        }
        g
    }

    fn id(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }

    #[test]
    fn roots_are_ready_with_no_gates() {
        let g = diamond();
        let ready = ready_tasks(&g, &GateVerdicts::new());
        assert_eq!(ready, vec![id("T001"), id("T002")]);
    }

    #[test]
    fn unlocks_only_when_all_prerequisites_pass() {
        let g = diamond();
        let mut gates = GateVerdicts::new();
        gates.insert(id("T001"), Verdict::Pass);
        // T002 not passed yet: T003 still blocked.
        assert_eq!(ready_tasks(&g, &gates), vec![id("T002")]);
        gates.insert(id("T002"), Verdict::Pass);
        assert_eq!(ready_tasks(&g, &gates), vec![id("T003")]);
    }

    #[test]
    fn passed_tasks_leave_the_ready_set() {
        let g = diamond();
        let mut gates = GateVerdicts::new();
        gates.insert(id("T001"), Verdict::Pass);
        gates.insert(id("T002"), Verdict::Pass);
        gates.insert(id("T003"), Verdict::Pass);
        assert_eq!(ready_tasks(&g, &gates), vec![id("T004")]);
    }

    #[test]
    fn non_pass_verdicts_do_not_unlock() {
        let g = diamond();
        let mut gates = GateVerdicts::new();
        gates.insert(id("T001"), Verdict::Pass);
        gates.insert(id("T002"), Verdict::Fail);
        // T003 blocked: T002 failed, not passed. T002 itself has no
        // prerequisites and has not passed, so it stays eligible for retry
        // (retry budgets are the dispatcher's decision, not the ready set's).
        assert_eq!(ready_tasks(&g, &gates), vec![id("T002")]);
    }
}
