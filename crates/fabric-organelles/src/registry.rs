//! REG-01 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Register organelles by (name, version). The registry is the discovery
//! layer the dispatcher uses to find worker organelles: lookup by exact
//! (name, version), or find every organelle producing a given output type.
//! Registration is append-only; there is no update or delete (a new version
//! is a new registration).
//!
//! Worker ORG1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. The
//! `OrganelleRecord` type lives in `record.rs` (your other file).

use crate::record::OrganelleRecord;
use organelle_core::TypeRef;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("duplicate registration: {0} v{1}")]
    Duplicate(String, String),
    #[error("invalid record: {0}")]
    InvalidInput(String),
}

/// T-REG-01: the organelle registry.
#[derive(Debug, Default)]
pub struct OrganelleRegistry {
    records: Vec<OrganelleRecord>,
}

impl OrganelleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an organelle. Duplicate (name, version) is rejected.
    /// Records with empty name/version or empty outputs are rejected.
    pub fn register(&mut self, record: OrganelleRecord) -> Result<(), RegistryError> {
        let _ = record;
        todo!("REG-01: implement organelle registration")
    }

    /// Exact lookup by (name, version).
    pub fn lookup(&self, name: &str, version: &str) -> Option<&OrganelleRecord> {
        let _ = (name, version);
        todo!("REG-01: implement exact lookup")
    }

    /// Every organelle producing `output`, sorted by (name, version).
    /// Deterministic; used by the composition planner.
    pub fn find_by_output(&self, output: &TypeRef) -> Vec<&OrganelleRecord> {
        let _ = output;
        todo!("REG-01: implement output-type lookup")
    }

    /// Every registered record, in registration order.
    pub fn all(&self) -> &[OrganelleRecord] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
