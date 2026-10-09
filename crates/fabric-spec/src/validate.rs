//! T017: reject incomplete task contracts.
//!
//! The [`TaskContract`] builder enforces presence of fields at construction.
//! This module applies the deeper checks the builder cannot: unknown task
//! classes, self-dependencies, duplicate entries, objective length limits,
//! acceptance-predicate ID uniqueness, and the failure-path invariant.
//!
//! Failure path (documented invariant): a contract has a failure path iff
//! (a) its retry policy defines at least one attempt, (b) at least one
//! verifier is registered to evaluate failure, and (c) at least one
//! acceptance predicate gives FAIL a criterion. All three are required
//! fields of the contract; this check pins the invariant in one place.

use fabric_types::{FabricError, Result, TaskContract};
use std::collections::HashSet;

const MAX_OBJECTIVE_CHARS: usize = 8192;

/// Validate a task contract beyond the builder's presence checks.
/// Returns `Ok(())` only when every check below passes.
pub fn validate_contract(contract: &TaskContract) -> Result<()> {
    // 1. Class must be known: dispatch cannot schedule an unknown class.
    contract.class.require_known()?;

    // 2. Objective: non-empty (builder) and within the length limit.
    let objective = contract.objective.trim();
    if objective.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "objective",
            reason: "missing".to_string(),
        });
    }
    if objective.chars().count() > MAX_OBJECTIVE_CHARS {
        return Err(FabricError::InvalidInput {
            field: "objective",
            reason: format!("exceeds {MAX_OBJECTIVE_CHARS} chars"),
        });
    }

    // 3. Spec revision present.
    if contract.spec_revision.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field: "spec_revision",
            reason: "missing".to_string(),
        });
    }

    // 4. Inputs / outputs: non-empty, each non-empty, no duplicates.
    check_string_list(&contract.inputs, "inputs")?;
    check_string_list(&contract.outputs, "outputs")?;

    // 5. Dependencies: no self-dependency, no duplicates.
    let mut seen_deps = HashSet::new();
    for dep in &contract.dependencies {
        if dep == &contract.id {
            return Err(FabricError::Conflict(format!(
                "task {} depends on itself",
                contract.id.as_str()
            )));
        }
        if !seen_deps.insert(dep) {
            return Err(FabricError::InvalidInput {
                field: "dependencies",
                reason: format!("duplicate dependency {}", dep.as_str()),
            });
        }
    }

    // 6. Scope: at least one allowed path (builder), each non-empty, no duplicates.
    check_string_list(&contract.scope.allowed_paths, "scope.allowed_paths")?;

    // 7. Acceptance: non-empty (builder), unique IDs, non-empty descriptions.
    if contract.acceptance.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "acceptance",
            reason: "missing".to_string(),
        });
    }
    let mut seen_predicates = HashSet::new();
    for p in &contract.acceptance {
        if p.id.trim().is_empty() || p.description.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "acceptance",
                reason: "predicate id and description must be non-empty".to_string(),
            });
        }
        if !seen_predicates.insert(p.id.as_str()) {
            return Err(FabricError::InvalidInput {
                field: "acceptance",
                reason: format!("duplicate predicate id {}", p.id),
            });
        }
    }

    // 8. Verifiers: non-empty (builder), each non-empty, no duplicates.
    check_string_list(&contract.verifiers, "verifiers")?;

    // 9. Failure path invariant: bounded retry policy + verifier + criterion.
    if contract.retry.max_attempts == 0 {
        return Err(FabricError::InvalidInput {
            field: "retry.max_attempts",
            reason: "failure path requires at least one attempt".to_string(),
        });
    }
    // (b) and (c) already established by checks 7 and 8.

    Ok(())
}

/// Non-empty list of non-empty, unique strings.
fn check_string_list(values: &[String], field: &'static str) -> Result<()> {
    if values.is_empty() {
        return Err(FabricError::InvalidInput {
            field,
            reason: "missing".to_string(),
        });
    }
    let mut seen = HashSet::new();
    for v in values {
        if v.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field,
                reason: "entries must be non-empty".to_string(),
            });
        }
        if !seen.insert(v.as_str()) {
            return Err(FabricError::InvalidInput {
                field,
                reason: format!("duplicate entry {v}"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{AcceptancePredicate, Scope, TaskClass, TaskContract, TaskId};

    fn valid() -> TaskContract {
        TaskContract::builder(TaskId::new("T017").unwrap(), TaskClass::Verify, "Verify the thing.")
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["T017.out".to_string()])
            .scope(Scope::new(vec!["crates/x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p1", "done", true).unwrap()])
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    #[test]
    fn valid_contract_passes() {
        assert!(validate_contract(&valid()).is_ok());
    }

    #[test]
    fn rejects_unknown_class() {
        let mut c = valid();
        c.class = TaskClass::Unknown("QUANTUM".to_string());
        let err = validate_contract(&c).unwrap_err();
        assert!(matches!(err, FabricError::UnknownClass(_)));
    }

    #[test]
    fn rejects_self_dependency() {
        let mut c = valid();
        c.dependencies = vec![TaskId::new("T017").unwrap()];
        let err = validate_contract(&c).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn rejects_duplicate_dependencies() {
        let mut c = valid();
        c.dependencies = vec![TaskId::new("T001").unwrap(), TaskId::new("T001").unwrap()];
        assert!(validate_contract(&c).is_err());
    }

    #[test]
    fn rejects_duplicate_inputs() {
        let mut c = valid();
        c.inputs = vec!["in".to_string(), "in".to_string()];
        assert!(validate_contract(&c).is_err());
    }

    #[test]
    fn rejects_duplicate_predicate_ids() {
        let mut c = valid();
        c.acceptance = vec![
            AcceptancePredicate::new("p1", "a", true).unwrap(),
            AcceptancePredicate::new("p1", "b", true).unwrap(),
        ];
        assert!(validate_contract(&c).is_err());
    }

    #[test]
    fn rejects_overlong_objective() {
        let mut c = valid();
        c.objective = "x".repeat(MAX_OBJECTIVE_CHARS + 1);
        assert!(validate_contract(&c).is_err());
    }

    #[test]
    fn rejects_empty_verifier_name() {
        let mut c = valid();
        c.verifiers = vec![" ".to_string()];
        assert!(validate_contract(&c).is_err());
    }
}
