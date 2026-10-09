//! T021: provider-independent capability catalog.
//!
//! Capability names describe *work* (coding, reviewing, ...), never a
//! vendor's product. The catalog is the authority that [`crate::validation`]
//! consults when checking endpoint declarations.

use fabric_types::error::Result;
use fabric_types::ids::validate_id;
use std::collections::HashSet;

/// Canonical capability names. Provider-independent by construction.
pub mod names {
    pub const CODING: &str = "coding";
    pub const REVIEWING: &str = "reviewing";
    pub const REPAIRING: &str = "repairing";
    pub const TEST_GENERATION: &str = "test-generation";
    pub const ADVERSARIAL_ANALYSIS: &str = "adversarial-analysis";
    pub const INTEGRATION: &str = "integration";

    /// All six canonical names in stable order.
    pub const ALL: [&str; 6] = [
        CODING,
        REVIEWING,
        REPAIRING,
        TEST_GENERATION,
        ADVERSARIAL_ANALYSIS,
        INTEGRATION,
    ];
}

/// T021: the provider-independent capability catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCatalog {
    capabilities: HashSet<String>,
}

impl CapabilityCatalog {
    /// The standard catalog: coding, reviewing, repairing, test-generation,
    /// adversarial-analysis, integration.
    pub fn standard() -> Self {
        Self {
            capabilities: names::ALL.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Build a catalog from an explicit set of names. Every name is validated
    /// with the shared identifier grammar; duplicates collapse.
    pub fn new(names: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut capabilities = HashSet::new();
        for name in names {
            validate_id(&name, "capability")?;
            capabilities.insert(name);
        }
        Ok(Self { capabilities })
    }

    /// True when `name` is a known capability.
    pub fn contains(&self, name: &str) -> bool {
        self.capabilities.contains(name)
    }

    /// True when every name in `required` is known.
    pub fn contains_all<'a>(&self, required: impl IntoIterator<Item = &'a str>) -> bool {
        required.into_iter().all(|n| self.contains(n))
    }

    /// Number of distinct capabilities in the catalog.
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// True when the catalog holds no capabilities.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// All capability names in deterministic (sorted) order.
    pub fn all(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.capabilities.iter().map(String::as_str).collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_catalog_holds_the_six_domains() {
        let cat = CapabilityCatalog::standard();
        assert_eq!(cat.len(), 6);
        for name in names::ALL {
            assert!(cat.contains(name), "{name}");
        }
        assert!(!cat.contains("teleportation"));
    }

    #[test]
    fn listing_is_deterministic_and_sorted() {
        let cat = CapabilityCatalog::standard();
        let all = cat.all();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        assert_eq!(all, sorted);
    }

    #[test]
    fn rejects_invalid_capability_names() {
        assert!(CapabilityCatalog::new(vec!["ok-name".to_string()]).is_ok());
        assert!(CapabilityCatalog::new(vec!["".to_string()]).is_err());
        assert!(CapabilityCatalog::new(vec!["has space".to_string()]).is_err());
    }

    #[test]
    fn contains_all_checks_every_name() {
        let cat = CapabilityCatalog::standard();
        assert!(cat.contains_all(["coding", "reviewing"]));
        assert!(!cat.contains_all(["coding", "nope"]));
        assert!(cat.contains_all(Vec::<&str>::new()));
    }

    #[test]
    fn duplicates_collapse() {
        let cat =
            CapabilityCatalog::new(vec!["coding".to_string(), "coding".to_string()]).unwrap();
        assert_eq!(cat.len(), 1);
    }
}
