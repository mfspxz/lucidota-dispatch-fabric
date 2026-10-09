//! T056: idempotent submission deduplication.
//!
//! Equivalent submissions are deduplicated on the explicit triple
//! `(idempotency_key, spec_revision, acceptance_digest)`. Two submissions
//! that share a key and spec revision but carry *different acceptance
//! criteria* are distinct tasks and are both preserved — the acceptance
//! digest is part of the dedup key, so stricter/looser criteria can never
//! silently collapse into each other.

use fabric_types::{FabricError, Result, TaskContract, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Outcome of submitting a task to the idempotency registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DedupOutcome {
    /// New, distinct submission. Carries the accepted task's ID.
    Accepted { task_id: TaskId },
    /// Equivalent submission already registered. Carries the original task's ID.
    DuplicateOf { task_id: TaskId },
}

/// The dedup key. All three components are required: the explicit client key,
/// the spec revision the submission was built against, and a canonical digest
/// of the acceptance criteria.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DedupKey {
    idempotency_key: String,
    spec_revision: String,
    acceptance_digest: String,
}

/// Registry of submitted tasks for idempotent deduplication.
#[derive(Debug, Default)]
pub struct IdempotencyRegistry {
    entries: HashMap<DedupKey, TaskId>,
    /// Reverse index: task ID -> the dedup key that claimed it. A second,
    /// *different* dedup key claiming the same task ID is a conflict, never
    /// a silent overwrite.
    by_task: HashMap<TaskId, DedupKey>,
}

/// FNV-1a 64-bit hex. Deterministic across runs; used for dedup digests only,
/// never as a security boundary.
fn fnv1a_hex(data: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

impl IdempotencyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Canonical, order-independent digest of a contract's acceptance criteria.
    /// Predicates are sorted by ID before hashing, so predicate order never
    /// affects equivalence.
    pub fn acceptance_digest(contract: &TaskContract) -> String {
        let mut parts: Vec<String> = contract
            .acceptance
            .iter()
            .map(|p| format!("{}\0{}\0{}", p.id, p.description, p.evidence_required))
            .collect();
        parts.sort();
        fnv1a_hex(&parts.join("\n"))
    }

    fn key_for(idempotency_key: &str, contract: &TaskContract) -> Result<DedupKey> {
        if idempotency_key.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "idempotency_key",
                reason: "idempotency key must be non-empty".to_string(),
            });
        }
        if idempotency_key.len() > 256 {
            return Err(FabricError::InvalidInput {
                field: "idempotency_key",
                reason: "idempotency key must be at most 256 characters".to_string(),
            });
        }
        Ok(DedupKey {
            idempotency_key: idempotency_key.to_string(),
            spec_revision: contract.spec_revision.clone(),
            acceptance_digest: Self::acceptance_digest(contract),
        })
    }

    /// Submit a task. Returns `Accepted` for a new distinct submission or
    /// `DuplicateOf` when an equivalent submission is already registered.
    pub fn submit(
        &mut self,
        idempotency_key: impl Into<String>,
        contract: TaskContract,
    ) -> Result<DedupOutcome> {
        let key = Self::key_for(&idempotency_key.into(), &contract)?;
        if let Some(existing) = self.entries.get(&key) {
            return Ok(DedupOutcome::DuplicateOf {
                task_id: existing.clone(),
            });
        }
        // Same task ID under a different dedup key: conflicting submission.
        if let Some(owner) = self.by_task.get(&contract.id) {
            if owner != &key {
                return Err(FabricError::Conflict(format!(
                    "task {} already registered under a different idempotency key",
                    contract.id.as_str(),
                )));
            }
        }
        let task_id = contract.id.clone();
        self.by_task.insert(task_id.clone(), key.clone());
        self.entries.insert(key, task_id.clone());
        Ok(DedupOutcome::Accepted { task_id })
    }

    /// Look up a prior submission without inserting.
    pub fn lookup(
        &self,
        idempotency_key: &str,
        contract: &TaskContract,
    ) -> Result<Option<TaskId>> {
        let key = Self::key_for(idempotency_key, contract)?;
        Ok(self.entries.get(&key).cloned())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{AcceptancePredicate, Scope, TaskClass};

    fn contract(id: &str, predicates: Vec<(&str, &str)>) -> TaskContract {
        TaskContract::builder(TaskId::new(id).unwrap(), TaskClass::Implement, "objective")
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["out".to_string()])
            .scope(Scope::new(vec!["crates/fabric-graph".to_string()]).unwrap())
            .acceptance(
                predicates
                    .into_iter()
                    .map(|(pid, desc)| AcceptancePredicate::new(pid, desc, true).unwrap())
                    .collect(),
            )
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    #[test]
    fn identical_resubmission_is_duplicate() {
        let mut reg = IdempotencyRegistry::new();
        let c1 = contract("T101", vec![("p1", "works")]);
        let c2 = contract("T102", vec![("p1", "works")]);
        assert!(matches!(
            reg.submit("key-1", c1).unwrap(),
            DedupOutcome::Accepted { .. }
        ));
        match reg.submit("key-1", c2).unwrap() {
            DedupOutcome::DuplicateOf { task_id } => {
                assert_eq!(task_id.as_str(), "T101");
            }
            DedupOutcome::Accepted { .. } => panic!("expected duplicate"),
        }
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn different_acceptance_criteria_are_preserved() {
        let mut reg = IdempotencyRegistry::new();
        let c1 = contract("T101", vec![("p1", "works")]);
        let c2 = contract("T102", vec![("p1", "works"), ("p2", "fast")]);
        assert!(matches!(
            reg.submit("key-1", c1).unwrap(),
            DedupOutcome::Accepted { .. }
        ));
        // Same key, same spec revision, different acceptance -> distinct task.
        match reg.submit("key-1", c2).unwrap() {
            DedupOutcome::Accepted { task_id } => {
                assert_eq!(task_id.as_str(), "T102");
            }
            DedupOutcome::DuplicateOf { .. } => panic!("must not dedup across criteria"),
        }
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn different_spec_revision_is_distinct() {
        let mut reg = IdempotencyRegistry::new();
        let c1 = contract("T101", vec![("p1", "works")]);
        let mut c2 = contract("T102", vec![("p1", "works")]);
        c2.spec_revision = "master-spec v2026-10-10".to_string();
        reg.submit("key-1", c1).unwrap();
        assert!(matches!(
            reg.submit("key-1", c2).unwrap(),
            DedupOutcome::Accepted { .. }
        ));
    }

    #[test]
    fn acceptance_digest_is_order_independent() {
        let c1 = contract("T101", vec![("p1", "a"), ("p2", "b")]);
        let c2 = contract("T102", vec![("p2", "b"), ("p1", "a")]);
        assert_eq!(
            IdempotencyRegistry::acceptance_digest(&c1),
            IdempotencyRegistry::acceptance_digest(&c2)
        );
    }

    #[test]
    fn reused_task_id_under_different_key_conflicts() {
        let mut reg = IdempotencyRegistry::new();
        let c1 = contract("T101", vec![("p1", "works")]);
        let c2 = contract("T101", vec![("p1", "works"), ("p2", "more")]);
        reg.submit("key-1", c1).unwrap();
        // Same task ID, different acceptance -> different dedup key -> conflict.
        let err = reg.submit("key-2", c2);
        assert!(matches!(err, Err(FabricError::Conflict(_))));
    }

    #[test]
    fn empty_key_rejected() {
        let mut reg = IdempotencyRegistry::new();
        let c = contract("T101", vec![("p1", "works")]);
        assert!(reg.submit("", c).is_err());
    }

    #[test]
    fn lookup_does_not_insert() {
        let reg = IdempotencyRegistry::new();
        let c = contract("T101", vec![("p1", "works")]);
        assert!(reg.lookup("key-9", &c).unwrap().is_none());
        assert!(reg.is_empty());
    }
}
