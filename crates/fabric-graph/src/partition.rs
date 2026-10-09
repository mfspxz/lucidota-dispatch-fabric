//! T055: partition ready tasks.
//!
//! Groups the ready set into deterministic partitions keyed by task class,
//! required capabilities, context budget, and isolation needs, so the
//! dispatcher can batch compatible work onto suitable endpoints. Tasks with
//! no capability entry land in an explicit "unknown" bucket (empty
//! capabilities, unbounded context) instead of being silently dropped.

use crate::nodes::TaskGraph;
use fabric_types::{CapabilityRequirement, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Isolation a task's worker needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IsolationNeeds {
    None,
    Workspace,
    WorkspaceAndNetwork,
}

/// Deterministic partition key. Ordered so `BTreeMap` iteration is stable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PartitionKey {
    pub class: String,
    pub capabilities: Vec<String>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub isolation: IsolationNeeds,
}

/// Partition ready tasks. Each partition's task list is ID-sorted.
/// Ready IDs absent from the graph are skipped (the graph is authoritative).
pub fn partition_ready(
    ready: &[TaskId],
    graph: &TaskGraph,
    capabilities: &HashMap<TaskId, CapabilityRequirement>,
    isolation: &HashMap<TaskId, IsolationNeeds>,
) -> BTreeMap<PartitionKey, Vec<TaskId>> {
    let index: HashMap<&TaskId, _> = graph.nodes().into_iter().map(|n| (&n.id, n)).collect();
    let mut parts: BTreeMap<PartitionKey, Vec<TaskId>> = BTreeMap::new();
    for id in ready {
        let node = match index.get(id) {
            Some(n) => n,
            None => continue,
        };
        let (caps, max_in, max_out) = match capabilities.get(id) {
            Some(c) => {
                let mut cs = c.capabilities.clone();
                cs.sort();
                cs.dedup();
                (
                    cs,
                    c.context_budget.max_input_tokens,
                    c.context_budget.max_output_tokens,
                )
            }
            None => (Vec::new(), None, None),
        };
        let key = PartitionKey {
            class: node.contract.class.name().to_string(),
            capabilities: caps,
            max_input_tokens: max_in,
            max_output_tokens: max_out,
            isolation: isolation.get(id).copied().unwrap_or(IsolationNeeds::None),
        };
        parts.entry(key).or_default().push(id.clone());
    }
    for tasks in parts.values_mut() {
        tasks.sort();
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::test_node;
    use fabric_types::{ContextBudget, QualityTier, TaskClass};

    fn caps(names: &[&str], max_in: Option<u64>) -> CapabilityRequirement {
        CapabilityRequirement::new(
            names.iter().map(|s| s.to_string()).collect(),
            QualityTier::Standard,
            ContextBudget {
                max_input_tokens: max_in,
                max_output_tokens: None,
            },
            false,
            None,
            vec![],
        )
        .unwrap()
    }

    fn id(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }

    #[test]
    fn groups_by_class_capability_budget_isolation() {
        let mut g = TaskGraph::new();
        g.add_node(test_node("T001", TaskClass::Implement)).unwrap();
        g.add_node(test_node("T002", TaskClass::Implement)).unwrap();
        g.add_node(test_node("T003", TaskClass::Review)).unwrap();
        let mut capabilities = HashMap::new();
        capabilities.insert(id("T001"), caps(&["coding"], Some(8000)));
        capabilities.insert(id("T002"), caps(&["coding"], Some(8000)));
        capabilities.insert(id("T003"), caps(&["review"], Some(8000)));
        let mut isolation = HashMap::new();
        isolation.insert(id("T002"), IsolationNeeds::Workspace);
        let parts = partition_ready(
            &[id("T003"), id("T002"), id("T001")],
            &g,
            &capabilities,
            &isolation,
        );
        // T001 and T003 differ by class+caps; T001 and T002 differ by isolation.
        assert_eq!(parts.len(), 3);
        // BTreeMap iteration is deterministic: sorted by key.
        let keys: Vec<String> = parts.keys().map(|k| k.class.clone()).collect();
        assert_eq!(keys, vec!["IMPLEMENT", "IMPLEMENT", "REVIEW"]);
        // task lists inside partitions are ID-sorted
        for tasks in parts.values() {
            let mut sorted = tasks.clone();
            sorted.sort();
            assert_eq!(tasks, &sorted);
        }
    }

    #[test]
    fn same_shape_tasks_share_a_partition() {
        let mut g = TaskGraph::new();
        g.add_node(test_node("T001", TaskClass::Implement)).unwrap();
        g.add_node(test_node("T002", TaskClass::Implement)).unwrap();
        let mut capabilities = HashMap::new();
        capabilities.insert(id("T001"), caps(&["coding", "review"], Some(8000)));
        // capability order must not matter
        capabilities.insert(id("T002"), caps(&["review", "coding"], Some(8000)));
        let parts = partition_ready(
            &[id("T001"), id("T002")],
            &g,
            &capabilities,
            &HashMap::new(),
        );
        assert_eq!(parts.len(), 1);
        let only: Vec<&Vec<TaskId>> = parts.values().collect();
        assert_eq!(only[0], &vec![id("T001"), id("T002")]);
    }

    #[test]
    fn missing_capabilities_go_to_unknown_bucket() {
        let mut g = TaskGraph::new();
        g.add_node(test_node("T001", TaskClass::Implement)).unwrap();
        let parts = partition_ready(&[id("T001")], &g, &HashMap::new(), &HashMap::new());
        assert_eq!(parts.len(), 1);
        let key = parts.keys().next().unwrap();
        assert!(key.capabilities.is_empty());
        assert_eq!(key.max_input_tokens, None);
    }
}
