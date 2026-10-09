//! T054: critical-path depth and estimated remaining work.
//!
//! Depth of a task is the length of the longest DependsOn chain beneath it
//! (0 for tasks with no prerequisites). Remaining work sums the configured
//! duration estimates of the task and every incomplete task in its transitive
//! prerequisite closure. Cycle detection is the spec compiler's job; here a
//! back-edge deterministically contributes 0 so a malformed graph cannot
//! hang the scheduler.

use crate::nodes::TaskGraph;
use fabric_types::{TaskClass, TaskId, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Configurable duration estimates, in hours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurationEstimates {
    /// Per-class estimates keyed by [`TaskClass::name`].
    pub by_class: HashMap<String, f64>,
    pub default_hours: f64,
}

impl DurationEstimates {
    /// Estimate for one class; unknown classes and non-positive values fall
    /// back to a clamped default. Never returns a negative value.
    pub fn estimate(&self, class: &TaskClass) -> f64 {
        self.by_class
            .get(class.name())
            .copied()
            .unwrap_or(self.default_hours)
            .max(0.0)
    }
}

impl Default for DurationEstimates {
    fn default() -> Self {
        let by_class: HashMap<String, f64> = [
            ("PLAN", 2.0),
            ("IMPLEMENT", 8.0),
            ("REVIEW", 3.0),
            ("REPAIR", 6.0),
            ("TEST_GENERATE", 4.0),
            ("ADVERSARIAL", 10.0),
            ("INTEGRATE", 5.0),
            ("VERIFY", 2.0),
            ("RELEASE", 3.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        Self {
            by_class,
            default_hours: 4.0,
        }
    }
}

fn depth_of(
    id: &TaskId,
    graph: &TaskGraph,
    memo: &mut HashMap<TaskId, u64>,
    visiting: &mut HashSet<TaskId>,
) -> u64 {
    if let Some(&d) = memo.get(id) {
        return d;
    }
    if !visiting.insert(id.clone()) {
        return 0; // cycle back-edge: contributes 0, deterministically
    }
    let d = graph
        .prerequisites(id)
        .iter()
        .map(|p| depth_of(p, graph, memo, visiting))
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    visiting.remove(id);
    memo.insert(id.clone(), d);
    d
}

/// Critical-path depth for every node in the graph.
pub fn critical_depths(graph: &TaskGraph) -> HashMap<TaskId, u64> {
    let mut memo = HashMap::new();
    for node in graph.nodes() {
        depth_of(&node.id, graph, &mut memo, &mut HashSet::new());
    }
    memo
}

/// Estimated remaining work (hours) to complete `id`: its own estimate plus
/// the estimates of every incomplete task in its transitive prerequisite
/// closure. A task is complete when its gate verdict is `Pass`.
pub fn estimated_remaining(
    graph: &TaskGraph,
    id: &TaskId,
    estimates: &DurationEstimates,
    gates: &HashMap<TaskId, Verdict>,
) -> f64 {
    let index: HashMap<&TaskId, _> = graph.nodes().into_iter().map(|n| (&n.id, n)).collect();
    let mut seen = HashSet::new();
    let mut stack = vec![id.clone()];
    let mut total = 0.0;
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if gates.get(&cur).is_some_and(|v| v.is_pass()) {
            continue;
        }
        if let Some(node) = index.get(&cur) {
            total += estimates.estimate(&node.contract.class);
            stack.extend(graph.prerequisites(&cur));
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::{test_node, DepEdge, EdgeKind};

    fn chain() -> TaskGraph {
        // T001 -> T002 -> T003 (T003 depends on T002 depends on T001)
        let mut g = TaskGraph::new();
        for id in ["T001", "T002", "T003"] {
            g.add_node(test_node(id, TaskClass::Implement)).unwrap();
        }
        for (from, to) in [("T002", "T001"), ("T003", "T002")] {
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
    fn depths_follow_longest_chain() {
        let g = chain();
        let depths = critical_depths(&g);
        assert_eq!(depths[&id("T001")], 0);
        assert_eq!(depths[&id("T002")], 1);
        assert_eq!(depths[&id("T003")], 2);
    }

    #[test]
    fn remaining_sums_incomplete_closure() {
        let g = chain();
        let est = DurationEstimates::default(); // IMPLEMENT = 8h
        let gates = HashMap::new();
        assert_eq!(estimated_remaining(&g, &id("T003"), &est, &gates), 24.0);
        let mut gates = HashMap::new();
        gates.insert(id("T001"), Verdict::Pass);
        assert_eq!(estimated_remaining(&g, &id("T003"), &est, &gates), 16.0);
    }

    #[test]
    fn remaining_of_completed_task_is_zero() {
        let g = chain();
        let est = DurationEstimates::default();
        let mut gates = HashMap::new();
        gates.insert(id("T003"), Verdict::Pass);
        assert_eq!(estimated_remaining(&g, &id("T003"), &est, &gates), 0.0);
    }

    #[test]
    fn cycle_does_not_hang() {
        let mut g = TaskGraph::new();
        for tid in ["T001", "T002"] {
            g.add_node(test_node(tid, TaskClass::Implement)).unwrap();
        }
        for (from, to) in [("T001", "T002"), ("T002", "T001")] {
            g.add_edge(DepEdge {
                from: id(from),
                to: id(to),
                kind: EdgeKind::DependsOn,
            })
            .unwrap();
        }
        let depths = critical_depths(&g); // must terminate
        assert_eq!(depths.len(), 2);
    }

    #[test]
    fn estimates_fall_back_and_clamp() {
        let est = DurationEstimates::default();
        assert_eq!(est.estimate(&TaskClass::Implement), 8.0);
        assert_eq!(
            est.estimate(&TaskClass::Unknown("X".to_string())),
            4.0
        );
        let mut bad = DurationEstimates::default();
        bad.by_class.insert("IMPLEMENT".to_string(), -5.0);
        assert_eq!(bad.estimate(&TaskClass::Implement), 0.0);
    }
}
