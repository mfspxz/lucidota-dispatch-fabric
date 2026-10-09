//! T051 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Represent task graph nodes and dependency edges with stable IDs and
//! explicit edge semantics.

use fabric_types::{FabricError, Result, TaskContract, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What a dependency edge means. Distinct semantics, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeKind {
    /// `from` cannot start until `to` passes its acceptance gate.
    DependsOn,
    /// `from` replaces `to`; `to` must be cancelled per policy.
    Supersedes,
}

/// A directed edge between two task nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepEdge {
    pub from: TaskId,
    pub to: TaskId,
    pub kind: EdgeKind,
}

/// A task node: stable ID plus its full contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: TaskId,
    pub contract: TaskContract,
}

/// The task graph: nodes keyed by stable ID, edges with explicit semantics.
#[derive(Debug, Default)]
pub struct TaskGraph {
    nodes: HashMap<TaskId, TaskNode>,
    edges: Vec<DepEdge>,
}

fn edge_rank(kind: EdgeKind) -> u8 {
    match kind {
        EdgeKind::DependsOn => 0,
        EdgeKind::Supersedes => 1,
    }
}

impl TaskGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node. Duplicate IDs are rejected; a node whose ID disagrees
    /// with its contract's ID is rejected.
    pub fn add_node(&mut self, node: TaskNode) -> Result<()> {
        if node.id != node.contract.id {
            return Err(FabricError::InvalidInput {
                field: "node.id",
                reason: "node id must equal contract id".to_string(),
            });
        }
        if self.nodes.contains_key(&node.id) {
            return Err(FabricError::Conflict(format!(
                "duplicate task node {}",
                node.id
            )));
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    /// Add an edge. Both endpoints must exist, self-edges are rejected,
    /// and duplicate (from, to, kind) edges are rejected.
    pub fn add_edge(&mut self, edge: DepEdge) -> Result<()> {
        if !self.nodes.contains_key(&edge.from) {
            return Err(FabricError::InvalidInput {
                field: "edge.from",
                reason: format!("unknown task {}", edge.from),
            });
        }
        if !self.nodes.contains_key(&edge.to) {
            return Err(FabricError::InvalidInput {
                field: "edge.to",
                reason: format!("unknown task {}", edge.to),
            });
        }
        if edge.from == edge.to {
            return Err(FabricError::InvalidInput {
                field: "edge",
                reason: "self-edges are not allowed".to_string(),
            });
        }
        if self
            .edges
            .iter()
            .any(|e| e.from == edge.from && e.to == edge.to && e.kind == edge.kind)
        {
            return Err(FabricError::Conflict(format!(
                "duplicate edge {} -> {}",
                edge.from, edge.to
            )));
        }
        self.edges.push(edge);
        Ok(())
    }

    /// All nodes, in deterministic (ID-sorted) order.
    pub fn nodes(&self) -> Vec<&TaskNode> {
        let mut v: Vec<&TaskNode> = self.nodes.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// All edges, in deterministic order: by (from, to, kind).
    pub fn edges(&self) -> Vec<&DepEdge> {
        let mut v: Vec<&DepEdge> = self.edges.iter().collect();
        v.sort_by(|a, b| {
            (&a.from, &a.to, edge_rank(a.kind)).cmp(&(&b.from, &b.to, edge_rank(b.kind)))
        });
        v
    }

    /// Prerequisites of `id`: targets of outgoing DependsOn edges, ID-sorted.
    /// Supersedes edges are not prerequisites and are excluded.
    pub fn prerequisites(&self, id: &TaskId) -> Vec<TaskId> {
        let mut v: Vec<TaskId> = self
            .edges
            .iter()
            .filter(|e| e.from == *id && e.kind == EdgeKind::DependsOn)
            .map(|e| e.to.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Test helper shared by all fabric-graph modules (crate-internal).
#[cfg(test)]
pub(crate) fn test_contract(
    id: &str,
    class: fabric_types::TaskClass,
    deps: Vec<TaskId>,
) -> TaskContract {
    use fabric_types::{AcceptancePredicate, Scope};
    TaskContract::builder(TaskId::new(id).unwrap(), class, format!("objective {id}"))
        .spec_revision("master-spec v2026-10-09")
        .inputs(vec!["in".to_string()])
        .outputs(vec![format!("{id}.out")])
        .dependencies(deps)
        .scope(Scope::new(vec!["crates/fabric-graph".to_string()]).unwrap())
        .acceptance(vec![AcceptancePredicate::new("p1", "done", true).unwrap()])
        .verifiers(vec!["cargo-test".to_string()])
        .failure_path("quarantine artifacts")
        .build()
        .unwrap()
}

/// Test helper: node with an empty-dependency contract.
#[cfg(test)]
pub(crate) fn test_node(id: &str, class: fabric_types::TaskClass) -> TaskNode {
    let contract = test_contract(id, class, vec![]);
    TaskNode {
        id: contract.id.clone(),
        contract,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::TaskClass;

    fn graph_two_nodes() -> TaskGraph {
        let mut g = TaskGraph::new();
        g.add_node(test_node("T001", TaskClass::Implement)).unwrap();
        g.add_node(test_node("T002", TaskClass::Review)).unwrap();
        g
    }

    #[test]
    fn rejects_duplicate_node_ids() {
        let mut g = TaskGraph::new();
        g.add_node(test_node("T001", TaskClass::Implement)).unwrap();
        let err = g.add_node(test_node("T001", TaskClass::Review));
        assert!(matches!(err, Err(FabricError::Conflict(_))));
    }

    #[test]
    fn rejects_node_id_contract_id_mismatch() {
        let mut g = TaskGraph::new();
        let contract = test_contract("T001", TaskClass::Implement, vec![]);
        let node = TaskNode {
            id: TaskId::new("T999").unwrap(),
            contract,
        };
        assert!(matches!(
            g.add_node(node),
            Err(FabricError::InvalidInput { .. })
        ));
    }

    #[test]
    fn edge_rules() {
        let mut g = graph_two_nodes();
        // unknown endpoint
        let bad = DepEdge {
            from: TaskId::new("T001").unwrap(),
            to: TaskId::new("T999").unwrap(),
            kind: EdgeKind::DependsOn,
        };
        assert!(g.add_edge(bad).is_err());
        // self edge
        let slf = DepEdge {
            from: TaskId::new("T001").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::DependsOn,
        };
        assert!(g.add_edge(slf).is_err());
        // ok then duplicate
        let e = DepEdge {
            from: TaskId::new("T002").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::DependsOn,
        };
        g.add_edge(e.clone()).unwrap();
        assert!(matches!(g.add_edge(e), Err(FabricError::Conflict(_))));
        // same endpoints, different kind: allowed
        let e2 = DepEdge {
            from: TaskId::new("T002").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::Supersedes,
        };
        assert!(g.add_edge(e2).is_ok());
    }

    #[test]
    fn listings_are_deterministic() {
        let mut g = TaskGraph::new();
        for id in ["T003", "T001", "T002"] {
            g.add_node(test_node(id, TaskClass::Implement)).unwrap();
        }
        let ids: Vec<String> = g.nodes().iter().map(|n| n.id.to_string()).collect();
        assert_eq!(ids, vec!["T001", "T002", "T003"]);
        g.add_edge(DepEdge {
            from: TaskId::new("T003").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::Supersedes,
        })
        .unwrap();
        g.add_edge(DepEdge {
            from: TaskId::new("T002").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::DependsOn,
        })
        .unwrap();
        let edges: Vec<(String, String)> = g
            .edges()
            .iter()
            .map(|e| (e.from.to_string(), e.to.to_string()))
            .collect();
        assert_eq!(
            edges,
            vec![
                ("T002".to_string(), "T001".to_string()),
                ("T003".to_string(), "T001".to_string()),
            ]
        );
    }

    #[test]
    fn prerequisites_only_counts_depends_on() {
        let mut g = graph_two_nodes();
        g.add_edge(DepEdge {
            from: TaskId::new("T002").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::Supersedes,
        })
        .unwrap();
        assert!(g.prerequisites(&TaskId::new("T002").unwrap()).is_empty());
        g.add_edge(DepEdge {
            from: TaskId::new("T002").unwrap(),
            to: TaskId::new("T001").unwrap(),
            kind: EdgeKind::DependsOn,
        })
        .unwrap();
        assert_eq!(
            g.prerequisites(&TaskId::new("T002").unwrap()),
            vec![TaskId::new("T001").unwrap()]
        );
    }
}
