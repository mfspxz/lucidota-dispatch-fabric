//! T018 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Build a dependency DAG from task contracts, detect cycles, and report a
//! concrete cycle path.
//!
//! Worker S2: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{Result, TaskContract, TaskId};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Error carrying one concrete cycle path, in dependency order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleError {
    /// Task IDs forming the cycle, first element repeated at the end.
    pub path: Vec<TaskId>,
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let chain: Vec<_> = self.path.iter().map(|t| t.as_str()).collect();
        write!(f, "dependency cycle: {}", chain.join(" -> "))
    }
}

impl std::error::Error for CycleError {}

/// Dependency DAG over task contracts.
#[derive(Debug, Clone)]
pub struct Dag {
    /// contract id -> dependency ids (edges point from task to its prerequisites)
    edges: HashMap<TaskId, Vec<TaskId>>,
}

impl Dag {
    /// Build the DAG. Unknown dependency IDs are a [`fabric_types::FabricError`];
    /// cycles are a [`CycleError`] with a concrete path.
    pub fn build(contracts: &[TaskContract]) -> Result<Self> {
        let _ = contracts;
        todo!("T018: implement DAG construction")
    }

    /// Deterministic topological order (dependencies before dependents).
    /// Ties broken by TaskId ordering so the output is stable.
    pub fn topological_order(&self) -> Vec<TaskId> {
        todo!("T018: implement topological ordering")
    }

    /// All known task IDs in the graph.
    pub fn task_ids(&self) -> HashSet<TaskId> {
        self.edges.keys().cloned().collect()
    }
}
