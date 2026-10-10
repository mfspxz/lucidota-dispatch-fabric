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

use fabric_types::{FabricError, Result, TaskClass};
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
        if reg.id.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "id",
                reason: "verifier id must be non-empty".to_string(),
            });
        }
        if reg.command.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "command",
                reason: "verifier argv must be non-empty".to_string(),
            });
        }
        if reg.timeout_ms == 0 {
            return Err(FabricError::InvalidInput {
                field: "timeout_ms",
                reason: "timeout bound must be non-zero".to_string(),
            });
        }
        if reg.max_output_bytes == 0 {
            return Err(FabricError::InvalidInput {
                field: "max_output_bytes",
                reason: "output bound must be non-zero".to_string(),
            });
        }
        if self.registrations.iter().any(|r| r.id == reg.id) {
            return Err(FabricError::Conflict(format!(
                "duplicate verifier id '{}'",
                reg.id
            )));
        }
        self.registrations.push(reg);
        Ok(())
    }

    /// All verifiers serving (class, requirement-capability), sorted by
    /// (version, command) for deterministic selection.
    pub fn lookup(
        &self,
        class: &TaskClass,
        capability: &VerifierCapability,
    ) -> Vec<&VerifierRegistration> {
        let mut out: Vec<&VerifierRegistration> = self
            .registrations
            .iter()
            .filter(|r| r.classes.contains(class) && r.capability == *capability)
            .collect();
        out.sort_by(|a, b| {
            (&a.version, &a.command)
                .partial_cmp(&(&b.version, &b.command))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// All registrations, in registration order.
    pub fn all(&self) -> &[VerifierRegistration] {
        &self.registrations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(id: &str, version: &str, classes: Vec<TaskClass>, cap: &str) -> VerifierRegistration {
        VerifierRegistration {
            id: id.to_string(),
            classes,
            capability: VerifierCapability(cap.to_string()),
            command: vec!["verify".to_string(), id.to_string()],
            version: version.to_string(),
            timeout_ms: 60_000,
            max_output_bytes: 1_048_576,
        }
    }

    #[test]
    fn register_and_lookup_roundtrip() {
        let mut r = VerifierRegistry::new();
        r.register(reg("v1", "1.0.0", vec![TaskClass::Verify], "cargo-test"))
            .unwrap();
        let hits = r.lookup(&TaskClass::Verify, &VerifierCapability("cargo-test".into()));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "v1");
        assert_eq!(r.all().len(), 1);
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut r = VerifierRegistry::new();
        r.register(reg("dup", "1.0.0", vec![TaskClass::Verify], "x"))
            .unwrap();
        let err = r
            .register(reg("dup", "2.0.0", vec![TaskClass::Verify], "x"))
            .unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn empty_command_rejected() {
        let mut r = VerifierRegistry::new();
        let mut bad = reg("bad", "1.0.0", vec![TaskClass::Verify], "x");
        bad.command.clear();
        let err = r.register(bad).unwrap_err();
        assert!(matches!(
            err,
            FabricError::InvalidInput { field: "command", .. }
        ));
    }

    #[test]
    fn zero_bounds_rejected() {
        let mut r = VerifierRegistry::new();
        let mut bad = reg("bad", "1.0.0", vec![TaskClass::Verify], "x");
        bad.timeout_ms = 0;
        assert!(matches!(
            r.register(bad).unwrap_err(),
            FabricError::InvalidInput {
                field: "timeout_ms",
                ..
            }
        ));
        let mut bad2 = reg("bad2", "1.0.0", vec![TaskClass::Verify], "x");
        bad2.max_output_bytes = 0;
        assert!(matches!(
            r.register(bad2).unwrap_err(),
            FabricError::InvalidInput {
                field: "max_output_bytes",
                ..
            }
        ));
    }

    #[test]
    fn empty_id_rejected() {
        let mut r = VerifierRegistry::new();
        let mut bad = reg("ok", "1.0.0", vec![TaskClass::Verify], "x");
        bad.id = "  ".to_string();
        assert!(matches!(
            r.register(bad).unwrap_err(),
            FabricError::InvalidInput { field: "id", .. }
        ));
    }

    #[test]
    fn lookup_filters_and_sorts_deterministically() {
        let mut r = VerifierRegistry::new();
        // Deliberately registered out of order; lookup must sort by (version, command).
        r.register(reg("c", "2.0.0", vec![TaskClass::Verify], "cargo-test"))
            .unwrap();
        r.register(reg("a", "1.0.0", vec![TaskClass::Verify], "cargo-test"))
            .unwrap();
        r.register(reg("b", "1.0.0", vec![TaskClass::Verify], "other-cap"))
            .unwrap();
        r.register(reg("d", "1.0.0", vec![TaskClass::Implement], "cargo-test"))
            .unwrap();
        let hits = r.lookup(&TaskClass::Verify, &VerifierCapability("cargo-test".into()));
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        // a and c match; a sorts first by version.
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn lookup_miss_returns_empty() {
        let r = VerifierRegistry::new();
        let hits = r.lookup(&TaskClass::Verify, &VerifierCapability("nope".into()));
        assert!(hits.is_empty());
    }
}
