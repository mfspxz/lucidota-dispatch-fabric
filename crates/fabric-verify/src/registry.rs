//! T091 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Register deterministic verifier commands and typed verifier capabilities
//! by task class and requirement. A verifier registration binds: the task
//! classes it serves, the typed capability it provides, the exact command
//! (argv, no shell), a version string, and a resource envelope (time and
//! output bounds). Lookup is deterministic: for a (class, requirement) pair
//! the registry returns every matching verifier, sorted by (version,
//! command) for stability.
//!
//! Worker V2: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{Result, TaskClass};
use serde::{Deserialize, Serialize};

/// A typed verifier capability, e.g. "cargo-test", "schema-check".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerifierCapability(pub String);

/// One registered verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierRegistration {
    /// Stable registration ID.
    pub id: String,
    pub classes: Vec<TaskClass>,
    pub capability: VerifierCapability,
    /// Exact argv, no shell interpretation.
    pub command: Vec<String>,
    /// Verifier version (affects cache keys and verdict receipts).
    pub version: String,
    /// Wall-clock bound in milliseconds.
    pub timeout_ms: u64,
    /// Max captured stdout+stderr bytes.
    pub max_output_bytes: u64,
}

/// T091: deterministic registry of verifier commands.
#[derive(Debug, Default)]
pub struct VerifierRegistry {
    registrations: Vec<VerifierRegistration>,
}

impl VerifierRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a verifier. Duplicate IDs are rejected. Commands must be
    /// non-empty; timeout and output bounds must be non-zero.
    pub fn register(&mut self, reg: VerifierRegistration) -> Result<()> {
        let _ = reg;
        todo!("T091: implement verifier registration")
    }

    /// All verifiers serving (class, requirement-capability), sorted by
    /// (version, command) for deterministic selection.
    pub fn lookup(
        &self,
        class: &TaskClass,
        capability: &VerifierCapability,
    ) -> Vec<&VerifierRegistration> {
        let _ = (class, capability);
        todo!("T091: implement deterministic verifier lookup")
    }

    /// All registrations, in registration order.
    pub fn all(&self) -> &[VerifierRegistration] {
        &self.registrations
    }
}
