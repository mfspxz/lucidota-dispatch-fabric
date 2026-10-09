//! T016: detect oversized or mixed-responsibility tasks using configurable bounds.
//!
//! A task that is too large or that mixes responsibilities must be sent back
//! for decomposition — never silently accepted. [`check_task_size`] returns
//! findings; [`decomposition_request`] turns findings into an explicit,
//! actionable decomposition request.

use fabric_types::{TaskContract, TaskId};
use serde::{Deserialize, Serialize};

/// Configurable size bounds. Zero means "no limit" for that dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundsConfig {
    pub max_objective_chars: usize,
    pub max_acceptance_predicates: usize,
    pub max_dependencies: usize,
    pub max_scope_paths: usize,
    pub max_inputs: usize,
    pub max_outputs: usize,
}

impl Default for BoundsConfig {
    fn default() -> Self {
        Self {
            max_objective_chars: 2000,
            max_acceptance_predicates: 12,
            max_dependencies: 10,
            max_scope_paths: 8,
            max_inputs: 16,
            max_outputs: 16,
        }
    }
}

/// Why a task was flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SizeFindingKind {
    /// A single dimension exceeds its bound.
    Oversized,
    /// The objective spans two or more responsibility domains.
    MixedResponsibility,
}

/// One finding against a task contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeFinding {
    pub task_id: TaskId,
    pub kind: SizeFindingKind,
    /// Machine-readable dimension, e.g. `objective_chars`, `responsibility_domains`.
    pub dimension: String,
    pub detail: String,
}

/// Responsibility domains used for mixed-responsibility detection.
/// Each verb maps to the domain of work it implies.
const VERB_DOMAINS: &[(&str, &str)] = &[
    ("implement", "BUILD"),
    ("build", "BUILD"),
    ("fix", "BUILD"),
    ("refactor", "BUILD"),
    ("test", "VERIFY"),
    ("verify", "VERIFY"),
    ("review", "REVIEW"),
    ("audit", "REVIEW"),
    ("deploy", "RELEASE"),
    ("release", "RELEASE"),
    ("document", "DOCS"),
    ("integrate", "INTEGRATE"),
    ("plan", "PLAN"),
];

/// True when `word` appears in `text` as a whole word (case-insensitive).
fn contains_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case(word))
}

/// Domains implied by the objective text, in first-appearance order.
fn implied_domains(objective: &str) -> Vec<&'static str> {
    let mut domains = Vec::new();
    for (verb, domain) in VERB_DOMAINS {
        if contains_word(objective, verb) && !domains.contains(domain) {
            domains.push(*domain);
        }
    }
    domains
}

/// Check a task contract against the bounds. An empty result means the task
/// is acceptably sized with a single responsibility.
pub fn check_task_size(contract: &TaskContract, bounds: &BoundsConfig) -> Vec<SizeFinding> {
    let mut findings = Vec::new();
    let id = contract.id.clone();
    let mut oversized = |dimension: &str, detail: String| {
        findings.push(SizeFinding {
            task_id: id.clone(),
            kind: SizeFindingKind::Oversized,
            dimension: dimension.to_string(),
            detail,
        });
    };

    let obj_chars = contract.objective.chars().count();
    if bounds.max_objective_chars > 0 && obj_chars > bounds.max_objective_chars {
        oversized(
            "objective_chars",
            format!("objective is {obj_chars} chars, bound is {}", bounds.max_objective_chars),
        );
    }
    if bounds.max_acceptance_predicates > 0
        && contract.acceptance.len() > bounds.max_acceptance_predicates
    {
        oversized(
            "acceptance_predicates",
            format!(
                "{} predicates, bound is {}",
                contract.acceptance.len(),
                bounds.max_acceptance_predicates
            ),
        );
    }
    if bounds.max_dependencies > 0 && contract.dependencies.len() > bounds.max_dependencies {
        oversized(
            "dependencies",
            format!(
                "{} dependencies, bound is {}",
                contract.dependencies.len(),
                bounds.max_dependencies
            ),
        );
    }
    if bounds.max_scope_paths > 0 && contract.scope.allowed_paths.len() > bounds.max_scope_paths {
        oversized(
            "scope_paths",
            format!(
                "{} scope paths, bound is {}",
                contract.scope.allowed_paths.len(),
                bounds.max_scope_paths
            ),
        );
    }
    if bounds.max_inputs > 0 && contract.inputs.len() > bounds.max_inputs {
        oversized(
            "inputs",
            format!("{} inputs, bound is {}", contract.inputs.len(), bounds.max_inputs),
        );
    }
    if bounds.max_outputs > 0 && contract.outputs.len() > bounds.max_outputs {
        oversized(
            "outputs",
            format!("{} outputs, bound is {}", contract.outputs.len(), bounds.max_outputs),
        );
    }

    let domains = implied_domains(&contract.objective);
    if domains.len() >= 2 {
        findings.push(SizeFinding {
            task_id: id,
            kind: SizeFindingKind::MixedResponsibility,
            dimension: "responsibility_domains".to_string(),
            detail: format!("objective spans domains: {}", domains.join(", ")),
        });
    }

    findings
}

/// Explicit request to decompose a task instead of accepting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecompositionRequest {
    pub task_id: TaskId,
    pub findings: Vec<SizeFinding>,
    /// Suggested split axes, derived from the findings (one per oversized
    /// dimension or per extra responsibility domain).
    pub recommended_splits: Vec<String>,
}

/// Build the decomposition request for a flagged task. Never empty-handed:
/// every finding yields at least one recommended split.
pub fn decomposition_request(
    contract: &TaskContract,
    findings: Vec<SizeFinding>,
) -> DecompositionRequest {
    let mut recommended_splits = Vec::new();
    for f in &findings {
        match f.kind {
            SizeFindingKind::Oversized => recommended_splits.push(format!(
                "split {} '{}' into smaller tasks",
                contract.id.as_str(),
                f.dimension
            )),
            SizeFindingKind::MixedResponsibility => {
                for domain in implied_domains(&contract.objective) {
                    recommended_splits.push(format!(
                        "extract {domain} responsibility from {} into its own task",
                        contract.id.as_str()
                    ));
                }
            }
        }
    }
    DecompositionRequest {
        task_id: contract.id.clone(),
        findings,
        recommended_splits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{AcceptancePredicate, Scope, TaskClass, TaskContract, TaskId};

    fn contract(objective: &str) -> TaskContract {
        TaskContract::builder(TaskId::new("T099").unwrap(), TaskClass::Implement, objective)
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["T099.out".to_string()])
            .scope(Scope::new(vec!["crates/x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p1", "done", true).unwrap()])
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    #[test]
    fn small_single_responsibility_task_passes() {
        let c = contract("Implement the token bucket limiter.");
        assert!(check_task_size(&c, &BoundsConfig::default()).is_empty());
    }

    #[test]
    fn oversized_objective_is_flagged() {
        let c = contract(&"x".repeat(2001));
        let findings = check_task_size(&c, &BoundsConfig::default());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, SizeFindingKind::Oversized);
        assert_eq!(findings[0].dimension, "objective_chars");
    }

    #[test]
    fn too_many_predicates_flagged() {
        let mut c = contract("Implement the limiter.");
        c.acceptance = (0..13)
            .map(|i| AcceptancePredicate::new(format!("p{i}"), "d", true).unwrap())
            .collect();
        let findings = check_task_size(&c, &BoundsConfig::default());
        assert!(findings.iter().any(|f| f.dimension == "acceptance_predicates"));
    }

    #[test]
    fn mixed_responsibility_detected() {
        let c = contract("Implement the limiter and review the auth module, then deploy it.");
        let findings = check_task_size(&c, &BoundsConfig::default());
        let mixed: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == SizeFindingKind::MixedResponsibility)
            .collect();
        assert_eq!(mixed.len(), 1);
        assert!(mixed[0].detail.contains("BUILD"));
        assert!(mixed[0].detail.contains("REVIEW"));
    }

    #[test]
    fn word_boundary_matters() {
        // "implemented" should not trigger the "implement" verb... it contains
        // "implement" as substring; word-boundary check must not fire on it.
        let c = contract("The preimplemented cache was already tested.");
        let findings = check_task_size(&c, &BoundsConfig::default());
        assert!(
            findings
                .iter()
                .all(|f| f.kind != SizeFindingKind::MixedResponsibility),
            "{findings:?}"
        );
    }

    #[test]
    fn decomposition_request_names_splits() {
        let c = contract("Implement the limiter and review the auth module.");
        let findings = check_task_size(&c, &BoundsConfig::default());
        assert!(!findings.is_empty());
        let req = decomposition_request(&c, findings);
        assert_eq!(req.task_id, c.id);
        assert!(!req.recommended_splits.is_empty());
        assert!(req.recommended_splits.iter().any(|s| s.contains("BUILD")));
        assert!(req.recommended_splits.iter().any(|s| s.contains("REVIEW")));
    }

    #[test]
    fn zero_bound_disables_dimension() {
        let bounds = BoundsConfig {
            max_objective_chars: 0,
            ..BoundsConfig::default()
        };
        let c = contract(&"x".repeat(5000));
        let findings = check_task_size(&c, &bounds);
        assert!(findings.iter().all(|f| f.dimension != "objective_chars"));
    }
}
