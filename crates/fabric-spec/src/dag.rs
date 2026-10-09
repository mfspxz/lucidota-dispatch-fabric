//! T018 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Build a dependency DAG from task contracts, detect cycles, and report a
//! concrete cycle path.
//!
//! Public signatures are frozen; bodies implemented by cell S2.

use fabric_types::{FabricError, Result, TaskContract, TaskId};
use std::collections::{BTreeSet, HashMap, HashSet};
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
        let mut edges: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
        for c in contracts {
            if edges.contains_key(&c.id) {
                return Err(FabricError::Conflict(format!(
                    "duplicate task id: {}",
                    c.id.as_str()
                )));
            }
            edges.insert(c.id.clone(), c.dependencies.clone());
        }
        for c in contracts {
            for dep in &c.dependencies {
                if !edges.contains_key(dep) {
                    return Err(FabricError::InvalidInput {
                        field: "dependencies",
                        reason: format!("unknown dependency id: {}", dep.as_str()),
                    });
                }
            }
        }
        let dag = Self { edges };
        dag.check_acyclic()?;
        Ok(dag)
    }

    /// Iterative depth-first cycle check. On a cycle, returns the concrete
    /// path with the entry node repeated at the end (e.g. a -> b -> a).
    fn check_acyclic(&self) -> Result<()> {
        let mut visiting: HashSet<TaskId> = HashSet::new();
        let mut done: HashSet<TaskId> = HashSet::new();
        let mut path: Vec<TaskId> = Vec::new();

        let mut roots: Vec<&TaskId> = self.edges.keys().collect();
        roots.sort();

        for root in roots {
            if done.contains(root) {
                continue;
            }
            // Explicit stack of (node, next-neighbor-index); no recursion.
            let mut stack: Vec<(TaskId, usize)> = vec![(root.clone(), 0)];
            visiting.insert(root.clone());
            path.push(root.clone());

            while let Some(top) = stack.last_mut() {
                let dep_count = self.edges.get(&top.0).map(|d| d.len()).unwrap_or(0);
                if top.1 < dep_count {
                    let dep = self.edges.get(&top.0).unwrap()[top.1].clone();
                    top.1 += 1;
                    if visiting.contains(&dep) {
                        let pos = path.iter().position(|x| x == &dep).unwrap_or(0);
                        let mut cycle: Vec<TaskId> = path[pos..].to_vec();
                        cycle.push(dep);
                        let err = CycleError { path: cycle };
                        return Err(FabricError::Conflict(err.to_string()));
                    }
                    if !done.contains(&dep) {
                        visiting.insert(dep.clone());
                        path.push(dep.clone());
                        stack.push((dep, 0));
                    }
                } else {
                    let (finished, _) = stack.pop().expect("stack underflow");
                    visiting.remove(&finished);
                    done.insert(finished);
                    path.pop();
                }
            }
        }
        Ok(())
    }

    /// Deterministic topological order (dependencies before dependents).
    /// Ties broken by TaskId ordering so the output is stable.
    pub fn topological_order(&self) -> Vec<TaskId> {
        // remaining[task] = number of not-yet-emitted dependencies.
        let mut remaining: HashMap<TaskId, usize> = HashMap::new();
        // dependents[dep] = tasks that directly depend on dep.
        let mut dependents: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
        for (task, deps) in &self.edges {
            remaining.insert(task.clone(), deps.len());
            for dep in deps {
                dependents.entry(dep.clone()).or_default().push(task.clone());
            }
        }
        let mut ready: BTreeSet<TaskId> = remaining
            .iter()
            .filter(|(_, &n)| n == 0)
            .map(|(t, _)| t.clone())
            .collect();
        let mut order = Vec::with_capacity(self.edges.len());
        while let Some(task) = ready.iter().next().cloned() {
            ready.remove(&task);
            order.push(task.clone());
            if let Some(ds) = dependents.get(&task) {
                for d in ds {
                    let r = remaining.get_mut(d).expect("dependent missing from graph");
                    *r -= 1;
                    if *r == 0 {
                        ready.insert(d.clone());
                    }
                }
            }
        }
        order
    }

    /// All known task IDs in the graph.
    pub fn task_ids(&self) -> HashSet<TaskId> {
        self.edges.keys().cloned().collect()
    }

    /// Direct dependencies of a task, if it is in the graph.
    pub fn dependencies_of(&self, id: &TaskId) -> Option<&[TaskId]> {
        self.edges.get(id).map(|v| v.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{AcceptancePredicate, Scope, TaskClass, TaskContract};

    fn contract(id: &str, deps: &[&str]) -> TaskContract {
        TaskContract::builder(
            TaskId::new(id).unwrap(),
            TaskClass::Implement,
            format!("objective of {id}"),
        )
        .spec_revision("master-spec v2026-10-09")
        .inputs(vec!["in".to_string()])
        .outputs(vec![format!("{id}.out")])
        .dependencies(deps.iter().map(|d| TaskId::new(*d).unwrap()).collect())
        .scope(Scope::new(vec!["crates/x".to_string()]).unwrap())
        .acceptance(vec![AcceptancePredicate::new("p1", "done", true).unwrap()])
        .verifiers(vec!["cargo-test".to_string()])
        .build()
        .unwrap()
    }

    #[test]
    fn builds_and_orders_diamond() {
        // T004 -> T002, T003; T002 -> T001; T003 -> T001
        let cs = vec![
            contract("T004", &["T002", "T003"]),
            contract("T002", &["T001"]),
            contract("T003", &["T001"]),
            contract("T001", &[]),
        ];
        let dag = Dag::build(&cs).unwrap();
        let order: Vec<String> = dag
            .topological_order()
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        assert_eq!(order[0], "T001");
        assert_eq!(order[3], "T004");
        // T002 and T003 tie-broken by TaskId order
        assert_eq!(&order[1..3], &["T002", "T003"]);
    }

    #[test]
    fn deterministic_tie_break() {
        let cs = vec![contract("TB", &[]), contract("TA", &[]), contract("TC", &[])];
        let dag = Dag::build(&cs).unwrap();
        let order: Vec<String> = dag
            .topological_order()
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        assert_eq!(order, vec!["TA", "TB", "TC"]);
    }

    #[test]
    fn rejects_unknown_dependency() {
        let cs = vec![contract("T001", &["T999"])];
        let err = Dag::build(&cs).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let cs = vec![contract("T001", &[]), contract("T001", &[])];
        let err = Dag::build(&cs).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn reports_concrete_cycle_path() {
        // T001 -> T002 -> T003 -> T001
        let cs = vec![
            contract("T001", &["T002"]),
            contract("T002", &["T003"]),
            contract("T003", &["T001"]),
        ];
        let err = Dag::build(&cs).unwrap_err();
        match err {
            FabricError::Conflict(msg) => {
                assert!(msg.contains("dependency cycle"), "{msg}");
                assert!(msg.contains("T001") && msg.contains("T002") && msg.contains("T003"));
                // entry node repeated at end: "X -> ... -> X"
                let chain = msg.split("dependency cycle: ").nth(1).unwrap();
                let nodes: Vec<_> = chain.split(" -> ").collect();
                assert_eq!(nodes.first(), nodes.last());
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn self_dependency_is_a_cycle() {
        let cs = vec![contract("T001", &["T001"])];
        let err = Dag::build(&cs).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn empty_graph_is_valid() {
        let dag = Dag::build(&[]).unwrap();
        assert!(dag.topological_order().is_empty());
    }
}
