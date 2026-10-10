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
    /// Registration is append-only; a new version is a new registration.
    pub fn register(&mut self, record: OrganelleRecord) -> Result<(), RegistryError> {
        if record.name.trim().is_empty() {
            return Err(RegistryError::InvalidInput("empty organelle name".to_string()));
        }
        if record.version.trim().is_empty() {
            return Err(RegistryError::InvalidInput(format!(
                "empty version for organelle '{}'",
                record.name
            )));
        }
        if record.outputs.is_empty() {
            return Err(RegistryError::InvalidInput(format!(
                "organelle '{}' v{} declares no outputs",
                record.name, record.version
            )));
        }
        if self.lookup(&record.name, &record.version).is_some() {
            return Err(RegistryError::Duplicate(record.name, record.version));
        }
        self.records.push(record);
        Ok(())
    }

    /// Exact lookup by (name, version).
    pub fn lookup(&self, name: &str, version: &str) -> Option<&OrganelleRecord> {
        self.records
            .iter()
            .find(|r| r.name == name && r.version == version)
    }

    /// Every organelle producing `output`, sorted by (name, version).
    /// Deterministic; used by the composition planner. Version sorting is
    /// lexicographic on the version string, not semver-aware.
    pub fn find_by_output(&self, output: &TypeRef) -> Vec<&OrganelleRecord> {
        let mut hits: Vec<&OrganelleRecord> = self
            .records
            .iter()
            .filter(|r| r.outputs.iter().any(|t| t == output))
            .collect();
        hits.sort_by(|a, b| (a.name.as_str(), a.version.as_str()).cmp(&(b.name.as_str(), b.version.as_str())));
        hits
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

#[cfg(test)]
mod tests {
    use super::*;
    use organelle_core::{Layer, MutationClass};

    fn record(name: &str, version: &str, outputs: Vec<TypeRef>) -> OrganelleRecord {
        OrganelleRecord::new(
            name,
            version,
            Layer::Global,
            MutationClass::ReadOnly,
            vec![],
            outputs,
            "sha256:artifact",
            "test",
        )
    }

    fn text_out() -> TypeRef {
        TypeRef::global("Text")
    }

    #[test]
    fn register_and_lookup_round_trip() {
        let mut reg = OrganelleRegistry::new();
        reg.register(record("count-words", "1.0.0", vec![text_out()])).unwrap();
        let found = reg.lookup("count-words", "1.0.0");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "count-words");
        assert!(reg.lookup("count-words", "2.0.0").is_none());
        assert!(reg.lookup("other", "1.0.0").is_none());
    }

    #[test]
    fn duplicate_name_version_rejected() {
        let mut reg = OrganelleRegistry::new();
        reg.register(record("dup", "1.0.0", vec![text_out()])).unwrap();
        let err = reg
            .register(record("dup", "1.0.0", vec![text_out()]))
            .unwrap_err();
        assert_eq!(err, RegistryError::Duplicate("dup".to_string(), "1.0.0".to_string()));
        // Same name, different version is a NEW registration (append-only).
        reg.register(record("dup", "1.1.0", vec![text_out()])).unwrap();
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn empty_name_version_or_outputs_rejected() {
        let mut reg = OrganelleRegistry::new();
        assert!(matches!(
            reg.register(record("", "1.0.0", vec![text_out()])),
            Err(RegistryError::InvalidInput(_))
        ));
        assert!(matches!(
            reg.register(record("   ", "1.0.0", vec![text_out()])),
            Err(RegistryError::InvalidInput(_))
        ));
        assert!(matches!(
            reg.register(record("noversion", "", vec![text_out()])),
            Err(RegistryError::InvalidInput(_))
        ));
        assert!(matches!(
            reg.register(record("nooutputs", "1.0.0", vec![])),
            Err(RegistryError::InvalidInput(_))
        ));
        assert!(reg.is_empty());
    }

    #[test]
    fn find_by_output_sorted_by_name_then_version() {
        let mut reg = OrganelleRegistry::new();
        reg.register(record("zebra", "1.0.0", vec![text_out()])).unwrap();
        reg.register(record("alpha", "2.0.0", vec![text_out()])).unwrap();
        reg.register(record("alpha", "1.0.0", vec![text_out()])).unwrap();
        reg.register(record("other", "1.0.0", vec![TypeRef::global("Number")])).unwrap();

        let hits = reg.find_by_output(&text_out());
        let keys: Vec<(&str, &str)> = hits.iter().map(|r| (r.name.as_str(), r.version.as_str())).collect();
        assert_eq!(
            keys,
            vec![("alpha", "1.0.0"), ("alpha", "2.0.0"), ("zebra", "1.0.0")]
        );
    }

    #[test]
    fn find_by_output_deterministic_across_insert_orders() {
        let build = |order: &[(&str, &str)]| {
            let mut reg = OrganelleRegistry::new();
            for (n, v) in order {
                reg.register(record(n, v, vec![text_out()])).unwrap();
            }
            reg.find_by_output(&text_out())
                .iter()
                .map(|r| (r.name.clone(), r.version.clone()))
                .collect::<Vec<_>>()
        };
        let fwd = build(&[("b", "1.0.0"), ("a", "1.0.0"), ("c", "2.0.0")]);
        let rev = build(&[("c", "2.0.0"), ("a", "1.0.0"), ("b", "1.0.0")]);
        assert_eq!(fwd, rev);
    }

    #[test]
    fn find_by_output_matches_exact_typeref_only() {
        let mut reg = OrganelleRegistry::new();
        reg.register(record("typed", "1.0.0", vec![TypeRef::domain("Text")])).unwrap();
        // %domain:Text is not %global:Text.
        assert!(reg.find_by_output(&text_out()).is_empty());
        assert_eq!(reg.find_by_output(&TypeRef::domain("Text")).len(), 1);
    }

    #[test]
    fn all_preserves_registration_order() {
        let mut reg = OrganelleRegistry::new();
        for (n, v) in [("c", "1.0.0"), ("a", "1.0.0"), ("b", "1.0.0")] {
            reg.register(record(n, v, vec![text_out()])).unwrap();
        }
        let names: Vec<&str> = reg.all().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }
}
