//! T014: traceability graph.
//!
//! Maps every requirement to the tasks, interfaces, tests, and evidence that
//! cover it. [`TraceGraph::uncovered_requirements`] reports requirements with
//! no links at all — silent requirement loss is a build-breaking finding, not
//! a warning to scroll past.

use crate::ids::RequirementId;
use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// What a requirement traces to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TraceTarget {
    /// A leaf task contract implementing the requirement.
    Task(TaskId),
    /// A public interface (module path, trait, or schema) it constrains.
    Interface(String),
    /// A test that evidences it.
    Test(String),
    /// An evidence artifact (receipt digest, report path).
    Evidence(String),
}

impl TraceTarget {
    /// Stable string key for deduplication.
    fn key(&self) -> String {
        match self {
            Self::Task(id) => format!("task:{}", id.as_str()),
            Self::Interface(name) => format!("interface:{name}"),
            Self::Test(name) => format!("test:{name}"),
            Self::Evidence(name) => format!("evidence:{name}"),
        }
    }
}

/// One traceability link: requirement → target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceEdge {
    pub requirement: RequirementId,
    pub target: TraceTarget,
}

/// T014: the traceability graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceGraph {
    edges: Vec<TraceEdge>,
}

impl TraceGraph {
    /// Build the graph from a set of links.
    ///
    /// Every edge's requirement must be in `requirement_ids`, otherwise the
    /// link is dangling and construction fails with [`FabricError::InvalidInput`].
    /// Duplicate edges are collapsed; surviving edges keep first-seen order,
    /// so builds are deterministic.
    pub fn build(requirement_ids: &[RequirementId], links: Vec<TraceEdge>) -> Result<Self> {
        let known: HashSet<&str> = requirement_ids.iter().map(|r| r.as_str()).collect();
        for edge in &links {
            if !known.contains(edge.requirement.as_str()) {
                return Err(FabricError::InvalidInput {
                    field: "trace_edge",
                    reason: format!(
                        "edge references unknown requirement {}",
                        edge.requirement.as_str()
                    ),
                });
            }
        }
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut edges = Vec::new();
        for edge in links {
            let key = (edge.requirement.as_str().to_string(), edge.target.key());
            if seen.insert(key) {
                edges.push(edge);
            }
        }
        Ok(Self { edges })
    }

    /// Requirements with zero links — the uncovered set. Empty means every
    /// requirement traces to at least one task, interface, test, or evidence.
    pub fn uncovered_requirements(
        &self,
        requirement_ids: &[RequirementId],
    ) -> Vec<RequirementId> {
        let covered: HashSet<&str> =
            self.edges.iter().map(|e| e.requirement.as_str()).collect();
        requirement_ids
            .iter()
            .filter(|r| !covered.contains(r.as_str()))
            .cloned()
            .collect()
    }

    /// All targets linked from one requirement, in insertion order.
    pub fn targets_for(&self, requirement: &RequirementId) -> Vec<&TraceTarget> {
        self.edges
            .iter()
            .filter(|e| e.requirement == *requirement)
            .map(|e| &e.target)
            .collect()
    }

    /// All requirements linked to one target.
    pub fn requirements_for_target(&self, target: &TraceTarget) -> Vec<&RequirementId> {
        self.edges
            .iter()
            .filter(|e| e.target == *target)
            .map(|e| &e.requirement)
            .collect()
    }

    /// Coverage fraction over the given requirement set: 1.0 = fully traced.
    pub fn coverage_fraction(&self, requirement_ids: &[RequirementId]) -> f64 {
        if requirement_ids.is_empty() {
            return 1.0;
        }
        let covered: HashSet<&str> =
            self.edges.iter().map(|e| e.requirement.as_str()).collect();
        let hit = requirement_ids
            .iter()
            .filter(|r| covered.contains(r.as_str()))
            .count();
        hit as f64 / requirement_ids.len() as f64
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Coverage grouped by target kind: (tasks, interfaces, tests, evidence).
    pub fn coverage_by_kind(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for edge in &self.edges {
            match edge.target {
                TraceTarget::Task(_) => counts.0 += 1,
                TraceTarget::Interface(_) => counts.1 += 1,
                TraceTarget::Test(_) => counts.2 += 1,
                TraceTarget::Evidence(_) => counts.3 += 1,
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u8) -> RequirementId {
        RequirementId::new(format!("REQ-{:016x}", n)).unwrap()
    }

    fn tid(n: u8) -> TaskId {
        TaskId::new(format!("T{n:03}")).unwrap()
    }

    #[test]
    fn uncovered_requirements_detected() {
        let ids = vec![rid(1), rid(2), rid(3)];
        let graph = TraceGraph::build(
            &ids,
            vec![TraceEdge {
                requirement: rid(1),
                target: TraceTarget::Task(tid(1)),
            }],
        )
        .unwrap();
        assert_eq!(graph.uncovered_requirements(&ids), vec![rid(2), rid(3)]);
        assert!((graph.coverage_fraction(&ids) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn full_coverage_yields_empty_uncovered() {
        let ids = vec![rid(1), rid(2)];
        let graph = TraceGraph::build(
            &ids,
            vec![
                TraceEdge {
                    requirement: rid(1),
                    target: TraceTarget::Test("parser::tests::x".to_string()),
                },
                TraceEdge {
                    requirement: rid(2),
                    target: TraceTarget::Evidence("sha256:abc".to_string()),
                },
            ],
        )
        .unwrap();
        assert!(graph.uncovered_requirements(&ids).is_empty());
        assert_eq!(graph.coverage_fraction(&ids), 1.0);
    }

    #[test]
    fn dangling_edge_rejected() {
        let ids = vec![rid(1)];
        let err = TraceGraph::build(
            &ids,
            vec![TraceEdge {
                requirement: rid(9),
                target: TraceTarget::Task(tid(1)),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn duplicate_edges_collapsed_deterministically() {
        let ids = vec![rid(1)];
        let edge = TraceEdge {
            requirement: rid(1),
            target: TraceTarget::Interface("fabric_spec::parser".to_string()),
        };
        let graph = TraceGraph::build(&ids, vec![edge.clone(), edge]).unwrap();
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn targets_and_requirements_lookup() {
        let ids = vec![rid(1), rid(2)];
        let target = TraceTarget::Task(tid(7));
        let graph = TraceGraph::build(
            &ids,
            vec![
                TraceEdge {
                    requirement: rid(1),
                    target: target.clone(),
                },
                TraceEdge {
                    requirement: rid(2),
                    target: target.clone(),
                },
            ],
        )
        .unwrap();
        assert_eq!(graph.targets_for(&rid(1)), vec![&target]);
        assert_eq!(graph.requirements_for_target(&target), vec![&rid(1), &rid(2)]);
        assert_eq!(graph.coverage_by_kind(), (2, 0, 0, 0));
    }

    #[test]
    fn empty_requirement_set_is_vacuously_covered() {
        let graph = TraceGraph::build(&[], vec![]).unwrap();
        assert_eq!(graph.coverage_fraction(&[]), 1.0);
    }
}
