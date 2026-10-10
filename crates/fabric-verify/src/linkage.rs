//! T087 — predicate-to-evidence linkage, plus T089 lineage queries.
//!
//! Every acceptance predicate is linked to the exact evidence digests that
//! evaluated it and the verifier (id + version) that ran the evaluation.
//! The [`lineage`] submodule provides read-only queries over an
//! append-only [`lineage::LineageIndex`]: task lineage, failed gates,
//! endpoint performance, requirement coverage, and artifact ancestry.
//!
//! NOTE (integrator): T089 lineage lives here as `pub mod lineage` because
//! `lib.rs` is frozen to this cell and declares no `query` module. Lineage
//! is about task/task ancestry; linkage is about predicate/evidence links —
//! related but distinct, hence the submodule rather than a merge.

use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// T087: one predicate evaluation linked to its evidence and verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateLink {
    pub predicate_id: String,
    /// Content digests (`algo:hex`) of the exact evidence evaluated.
    pub evidence_digests: Vec<String>,
    pub verifier_id: String,
    pub verifier_version: String,
    /// Inject at the call site; the index never reads the system clock.
    pub evaluated_at_ms: u64,
}

/// Deterministic index of predicate links, in insertion order.
#[derive(Debug, Default)]
pub struct LinkageIndex {
    links: HashMap<String, Vec<PredicateLink>>,
    order: Vec<String>, // predicate IDs in first-record order
}

impl LinkageIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one predicate link. Rejects empty IDs/versions and empty
    /// evidence lists: a link with no evidence is not a link.
    pub fn record(&mut self, link: PredicateLink) -> Result<()> {
        validate_link(&link)?;
        let entry = self.links.entry(link.predicate_id.clone()).or_default();
        if entry.is_empty() {
            self.order.push(link.predicate_id.clone());
        }
        entry.push(link);
        Ok(())
    }

    /// All links for a predicate, oldest first. Empty when unknown.
    pub fn links_for(&self, predicate_id: &str) -> Vec<&PredicateLink> {
        self.links
            .get(predicate_id)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// The most recent link for a predicate, if any.
    pub fn latest_link(&self, predicate_id: &str) -> Option<&PredicateLink> {
        self.links.get(predicate_id).and_then(|v| v.last())
    }

    /// Predicate IDs with at least one link, in first-record order.
    pub fn predicates(&self) -> &[String] {
        &self.order
    }

    /// Total links recorded.
    pub fn len(&self) -> usize {
        self.links.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
}

fn validate_link(l: &PredicateLink) -> Result<()> {
    if l.predicate_id.is_empty() || l.predicate_id.len() > 256 {
        return Err(FabricError::InvalidInput {
            field: "predicate_id",
            reason: "length must be 1..=256 characters".to_string(),
        });
    }
    if l.verifier_id.is_empty() || l.verifier_version.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "verifier",
            reason: "verifier id and version must be non-empty".to_string(),
        });
    }
    if l.evidence_digests.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "evidence_digests",
            reason: "a predicate link must reference at least one evidence digest".to_string(),
        });
    }
    for d in &l.evidence_digests {
        validate_digest_format(d)?;
    }
    Ok(())
}

fn validate_digest_format(d: &str) -> Result<()> {
    match d.split_once(':') {
        Some((algo, hex)) if !algo.is_empty() && !hex.is_empty() => Ok(()),
        _ => Err(FabricError::InvalidInput {
            field: "evidence_digests",
            reason: "digests must be algo:hex".to_string(),
        }),
    }
}

// ============================================================================
// T089 — lineage queries (submodule; see module docs for placement rationale).
// ============================================================================

/// T089: read-only lineage queries over append-only history.
pub mod lineage {
    use super::*;

    /// One lineage event. The index assigns `seq`; callers supply the rest.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum LineageEvent {
        TaskAttempt {
            task_id: TaskId,
            attempt: u32,
            at_ms: u64,
        },
        GateFailed {
            task_id: TaskId,
            predicate_id: String,
            verifier_id: String,
            at_ms: u64,
        },
        EndpointObserved {
            endpoint_id: String,
            latency_ms: u64,
            ok: bool,
            at_ms: u64,
        },
        RequirementCovered {
            requirement_id: String,
            task_id: TaskId,
            predicate_id: String,
            at_ms: u64,
        },
        ArtifactDerived {
            artifact_digest: String,
            parent_digests: Vec<String>,
            at_ms: u64,
        },
    }

    /// An event with its index-assigned sequence number.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct IndexedEvent {
        pub seq: u64,
        pub event: LineageEvent,
    }

    /// Aggregate endpoint performance over recorded observations.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct EndpointStats {
        pub endpoint_id: String,
        pub observations: u64,
        pub failures: u64,
        /// Mean latency over successful observations; None when unknown.
        pub mean_latency_ms: Option<u64>,
    }

    impl EndpointStats {
        /// Failure rate in [0.0, 1.0]; None when no observations.
        pub fn failure_rate(&self) -> Option<f64> {
            if self.observations == 0 {
                None
            } else {
                Some(self.failures as f64 / self.observations as f64)
            }
        }
    }

    /// Append-only lineage index. Events are never mutated or removed;
    /// queries are read-only and deterministic (sequence order).
    #[derive(Debug, Default)]
    pub struct LineageIndex {
        events: Vec<IndexedEvent>,
    }

    impl LineageIndex {
        pub fn new() -> Self {
            Self::default()
        }

        /// Append one event; returns its sequence number. Sequence numbers
        /// are dense: 0, 1, 2, ... with no gaps.
        pub fn append(&mut self, event: LineageEvent) -> Result<u64> {
            validate_event(&event)?;
            let seq = self.events.len() as u64;
            self.events.push(IndexedEvent { seq, event });
            Ok(seq)
        }

        /// Number of recorded events (= next sequence number).
        pub fn len(&self) -> usize {
            self.events.len()
        }

        pub fn is_empty(&self) -> bool {
            self.events.is_empty()
        }

        /// Task lineage: every event that references `task_id`
        /// (attempts, failed gates, requirement coverage), in seq order.
        pub fn lineage_of(&self, task_id: &TaskId) -> Vec<&IndexedEvent> {
            self.events
                .iter()
                .filter(|e| match &e.event {
                    LineageEvent::TaskAttempt { task_id: t, .. } => t == task_id,
                    LineageEvent::GateFailed { task_id: t, .. } => t == task_id,
                    LineageEvent::RequirementCovered { task_id: t, .. } => t == task_id,
                    _ => false,
                })
                .collect()
        }

        /// Failed acceptance gates for a task, in seq order.
        pub fn failed_gates(&self, task_id: &TaskId) -> Vec<&IndexedEvent> {
            self.events
                .iter()
                .filter(|e| {
                    matches!(&e.event, LineageEvent::GateFailed { task_id: t, .. } if t == task_id)
                })
                .collect()
        }

        /// Performance aggregates for one endpoint. Unknown stays unknown:
        /// no observations means `observations: 0` and `mean_latency_ms: None`.
        pub fn endpoint_performance(&self, endpoint_id: &str) -> EndpointStats {
            let mut observations = 0u64;
            let mut failures = 0u64;
            let mut latency_sum = 0u64;
            let mut latency_n = 0u64;
            for e in &self.events {
                if let LineageEvent::EndpointObserved {
                    endpoint_id: id,
                    latency_ms,
                    ok,
                    ..
                } = &e.event
                {
                    if id == endpoint_id {
                        observations += 1;
                        if !ok {
                            failures += 1;
                        } else {
                            latency_sum += latency_ms;
                            latency_n += 1;
                        }
                    }
                }
            }
            EndpointStats {
                endpoint_id: endpoint_id.to_string(),
                observations,
                failures,
                // checked_div: None when no successful observations (unknown
                // stays unknown), never a divide-by-zero.
                mean_latency_ms: latency_sum.checked_div(latency_n),
            }
        }

        /// (task, predicate) pairs recorded as covering a requirement.
        pub fn requirement_coverage(&self, requirement_id: &str) -> Vec<(TaskId, String)> {
            let mut out = Vec::new();
            for e in &self.events {
                if let LineageEvent::RequirementCovered {
                    requirement_id: r,
                    task_id,
                    predicate_id,
                    ..
                } = &e.event
                {
                    if r == requirement_id {
                        out.push((task_id.clone(), predicate_id.clone()));
                    }
                }
            }
            out
        }

        /// Transitive artifact ancestry: parent digests of `digest`,
        /// depth-first in recorded parent order, cycle-guarded, deduplicated.
        /// The starting digest itself is not included.
        pub fn artifact_ancestry(&self, digest: &str) -> Vec<String> {
            let mut ancestry = Vec::new();
            let mut visited = std::collections::HashSet::new();
            visited.insert(digest.to_string());
            let mut stack: Vec<String> = self
                .events
                .iter()
                .filter_map(|e| match &e.event {
                    LineageEvent::ArtifactDerived {
                        artifact_digest,
                        parent_digests,
                        ..
                    } if artifact_digest == digest => Some(parent_digests.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            // Reverse so popping yields recorded order.
            stack.reverse();
            while let Some(d) = stack.pop() {
                if !visited.insert(d.clone()) {
                    continue;
                }
                ancestry.push(d.clone());
                let mut parents: Vec<String> = self
                    .events
                    .iter()
                    .filter_map(|e| match &e.event {
                        LineageEvent::ArtifactDerived {
                            artifact_digest,
                            parent_digests,
                            ..
                        } if artifact_digest == &d => Some(parent_digests.clone()),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                parents.reverse();
                stack.extend(parents);
            }
            ancestry
        }
    }

    fn validate_event(e: &LineageEvent) -> Result<()> {
        let bad = |field: &'static str, reason: &str| FabricError::InvalidInput {
            field,
            reason: reason.to_string(),
        };
        let nonempty = |s: &str| !s.is_empty() && s.len() <= 256;
        match e {
            LineageEvent::TaskAttempt { attempt, .. } => {
                if *attempt == 0 {
                    return Err(bad("attempt", "attempt numbers are 1-based"));
                }
            }
            LineageEvent::GateFailed { predicate_id, verifier_id, .. } => {
                if !nonempty(predicate_id) {
                    return Err(bad("predicate_id", "length must be 1..=256 characters"));
                }
                if !nonempty(verifier_id) {
                    return Err(bad("verifier_id", "length must be 1..=256 characters"));
                }
            }
            LineageEvent::EndpointObserved { endpoint_id, .. } => {
                if !nonempty(endpoint_id) {
                    return Err(bad("endpoint_id", "length must be 1..=256 characters"));
                }
            }
            LineageEvent::RequirementCovered {
                requirement_id,
                predicate_id,
                ..
            } => {
                if !nonempty(requirement_id) {
                    return Err(bad("requirement_id", "length must be 1..=256 characters"));
                }
                if !nonempty(predicate_id) {
                    return Err(bad("predicate_id", "length must be 1..=256 characters"));
                }
            }
            LineageEvent::ArtifactDerived {
                artifact_digest,
                parent_digests,
                ..
            } => {
                validate_digest_format(artifact_digest)?;
                for p in parent_digests {
                    validate_digest_format(p)?;
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fabric_types::TaskId;

        fn idx() -> LineageIndex {
            LineageIndex::new()
        }

        fn tid(s: &str) -> TaskId {
            TaskId::new(s).unwrap()
        }

        #[test]
        fn seqs_are_dense() {
            let mut l = idx();
            assert_eq!(l.append(LineageEvent::TaskAttempt { task_id: tid("T-1"), attempt: 1, at_ms: 1 }).unwrap(), 0);
            assert_eq!(l.append(LineageEvent::TaskAttempt { task_id: tid("T-1"), attempt: 2, at_ms: 2 }).unwrap(), 1);
            assert_eq!(l.len(), 2);
        }

        #[test]
        fn lineage_of_collects_task_events_in_order() {
            let mut l = idx();
            l.append(LineageEvent::TaskAttempt { task_id: tid("T-1"), attempt: 1, at_ms: 1 }).unwrap();
            l.append(LineageEvent::EndpointObserved { endpoint_id: "ep".into(), latency_ms: 5, ok: true, at_ms: 2 }).unwrap();
            l.append(LineageEvent::GateFailed { task_id: tid("T-1"), predicate_id: "p1".into(), verifier_id: "v".into(), at_ms: 3 }).unwrap();
            l.append(LineageEvent::TaskAttempt { task_id: tid("T-2"), attempt: 1, at_ms: 4 }).unwrap();
            let lin = l.lineage_of(&tid("T-1"));
            assert_eq!(lin.len(), 2);
            assert_eq!(lin[0].seq, 0);
            assert_eq!(lin[1].seq, 2);
        }

        #[test]
        fn failed_gates_filters() {
            let mut l = idx();
            l.append(LineageEvent::GateFailed { task_id: tid("T-1"), predicate_id: "p1".into(), verifier_id: "v".into(), at_ms: 1 }).unwrap();
            l.append(LineageEvent::GateFailed { task_id: tid("T-2"), predicate_id: "p2".into(), verifier_id: "v".into(), at_ms: 2 }).unwrap();
            let g = l.failed_gates(&tid("T-1"));
            assert_eq!(g.len(), 1);
            match &g[0].event {
                LineageEvent::GateFailed { predicate_id, .. } => assert_eq!(predicate_id, "p1"),
                _ => panic!("wrong variant"),
            }
        }

        #[test]
        fn endpoint_performance_aggregates() {
            let mut l = idx();
            l.append(LineageEvent::EndpointObserved { endpoint_id: "ep".into(), latency_ms: 10, ok: true, at_ms: 1 }).unwrap();
            l.append(LineageEvent::EndpointObserved { endpoint_id: "ep".into(), latency_ms: 30, ok: true, at_ms: 2 }).unwrap();
            l.append(LineageEvent::EndpointObserved { endpoint_id: "ep".into(), latency_ms: 0, ok: false, at_ms: 3 }).unwrap();
            let s = l.endpoint_performance("ep");
            assert_eq!(s.observations, 3);
            assert_eq!(s.failures, 1);
            assert_eq!(s.mean_latency_ms, Some(20)); // failures excluded from mean
            assert!((s.failure_rate().unwrap() - 1.0 / 3.0).abs() < 1e-9);
            let unknown = l.endpoint_performance("nope");
            assert_eq!(unknown.observations, 0);
            assert_eq!(unknown.mean_latency_ms, None);
            assert_eq!(unknown.failure_rate(), None);
        }

        #[test]
        fn requirement_coverage_lists_covering_pairs() {
            let mut l = idx();
            l.append(LineageEvent::RequirementCovered { requirement_id: "REQ-1".into(), task_id: tid("T-1"), predicate_id: "p1".into(), at_ms: 1 }).unwrap();
            l.append(LineageEvent::RequirementCovered { requirement_id: "REQ-1".into(), task_id: tid("T-2"), predicate_id: "p2".into(), at_ms: 2 }).unwrap();
            let cov = l.requirement_coverage("REQ-1");
            assert_eq!(cov.len(), 2);
            assert_eq!(cov[0], (tid("T-1"), "p1".to_string()));
            assert!(l.requirement_coverage("REQ-9").is_empty());
        }

        #[test]
        fn artifact_ancestry_walks_transitively() {
            let mut l = idx();
            l.append(LineageEvent::ArtifactDerived { artifact_digest: "sha256:c".into(), parent_digests: vec!["sha256:b".into()], at_ms: 1 }).unwrap();
            l.append(LineageEvent::ArtifactDerived { artifact_digest: "sha256:b".into(), parent_digests: vec!["sha256:a".into()], at_ms: 2 }).unwrap();
            let anc = l.artifact_ancestry("sha256:c");
            assert_eq!(anc, vec!["sha256:b".to_string(), "sha256:a".to_string()]);
        }

        #[test]
        fn artifact_ancestry_cycle_guarded() {
            let mut l = idx();
            l.append(LineageEvent::ArtifactDerived { artifact_digest: "sha256:x".into(), parent_digests: vec!["sha256:y".into()], at_ms: 1 }).unwrap();
            l.append(LineageEvent::ArtifactDerived { artifact_digest: "sha256:y".into(), parent_digests: vec!["sha256:x".into()], at_ms: 2 }).unwrap();
            let anc = l.artifact_ancestry("sha256:x");
            assert_eq!(anc, vec!["sha256:y".to_string()]); // terminates, no repeat
        }

        #[test]
        fn invalid_events_rejected() {
            let mut l = idx();
            assert!(l.append(LineageEvent::TaskAttempt { task_id: tid("T-1"), attempt: 0, at_ms: 1 }).is_err());
            assert!(l.append(LineageEvent::GateFailed { task_id: tid("T-1"), predicate_id: "".into(), verifier_id: "v".into(), at_ms: 1 }).is_err());
            assert!(l.append(LineageEvent::ArtifactDerived { artifact_digest: "not-a-digest".into(), parent_digests: vec![], at_ms: 1 }).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> PredicateLink {
        PredicateLink {
            predicate_id: "pred-1".to_string(),
            evidence_digests: vec!["sha256:abc".to_string()],
            verifier_id: "cargo-test".to_string(),
            verifier_version: "1.99.0".to_string(),
            evaluated_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn records_and_queries_links() {
        let mut idx = LinkageIndex::new();
        idx.record(link()).unwrap();
        let mut l2 = link();
        l2.verifier_version = "1.100.0".to_string();
        idx.record(l2).unwrap();
        let links = idx.links_for("pred-1");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].verifier_version, "1.99.0");
        assert_eq!(idx.latest_link("pred-1").unwrap().verifier_version, "1.100.0");
        assert!(idx.links_for("unknown").is_empty());
        assert_eq!(idx.predicates(), &["pred-1".to_string()]);
    }

    #[test]
    fn rejects_link_without_evidence() {
        let mut idx = LinkageIndex::new();
        let mut l = link();
        l.evidence_digests.clear();
        let err = idx.record(l).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_empty_ids_and_bad_digests() {
        let mut idx = LinkageIndex::new();
        let mut l = link();
        l.predicate_id.clear();
        assert!(idx.record(l).is_err());
        let mut l2 = link();
        l2.evidence_digests = vec!["not-a-digest".to_string()];
        assert!(idx.record(l2).is_err());
    }

    #[test]
    fn link_serde_stable() {
        let json = serde_json::to_string(&link()).unwrap();
        let back: PredicateLink = serde_json::from_str(&json).unwrap();
        assert_eq!(back, link());
    }
}
