//! T051 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Represent task graph nodes and dependency edges with stable IDs and
//! explicit edge semantics.
//!
//! Worker G1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{Result, TaskContract, TaskId};
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

impl TaskGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node. Duplicate IDs are rejected.
    pub fn add_node(&mut self, node: TaskNode) -> Result<()> {
        let _ = node;
        todo!("T051: implement node insertion")
    }

    /// Add an edge. Both endpoints must exist; duplicate edges rejected.
    pub fn add_edge(&mut self, edge: DepEdge) -> Result<()> {
        let _ = edge;
        todo!("T051: implement edge insertion")
    }

    /// All nodes, in deterministic (ID-sorted) order.
    pub fn nodes(&self) -> Vec<&TaskNode> {
        todo!("T051: implement deterministic node listing")
    }

    /// All edges, in deterministic order.
    pub fn edges(&self) -> Vec<&DepEdge> {
        todo!("T051: implement deterministic edge listing")
    }

    /// Prerequisites of `id` (outgoing DependsOn edges), ID-sorted.
    pub fn prerequisites(&self, id: &TaskId) -> Vec<TaskId> {
        let _ = id;
        todo!("T051: implement prerequisite lookup")
    }
}
