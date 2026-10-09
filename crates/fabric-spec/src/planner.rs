//! T015: planner-model input/output contract.
//!
//! Defines exactly what a planner model receives ([`PlannerInput`]) and what
//! it must return (a list of [`PlannedTask`]), publishes both as JSON schemas
//! ([`planner_input_schema`], [`planner_output_schema`]), and validates every
//! decomposition result against the source spec with
//! [`validate_decomposition`]: unknown requirement references, unknown task
//! classes, duplicate task IDs, dangling or self dependencies, empty fields,
//! and — critically — requirements left with no planned task are all rejected.
//! [`to_task_contract`] converts a validated planned task into a
//! [`TaskContract`] through the T004 builder, so planner output becomes
//! enforceable contracts without re-typing.

use crate::ids::RequirementId;
use crate::parser::{ParsedSpec, SourceAnchor};
use fabric_types::{
    AcceptancePredicate, FabricError, Result, Scope, SpecId, TaskClass, TaskContract, TaskId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

/// One requirement as handed to the planner model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannableRequirement {
    pub id: RequirementId,
    pub text: String,
    pub anchor: SourceAnchor,
}

/// T015: the planner model's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannerInput {
    pub spec_revision: SpecId,
    pub source_digest: String,
    pub requirements: Vec<PlannableRequirement>,
}

impl PlannerInput {
    /// Build planner input from a parsed spec and its assigned requirement IDs.
    ///
    /// `ids` must correspond 1:1, in order, to `spec.requirements`.
    pub fn from_spec(spec: &ParsedSpec, ids: &[RequirementId]) -> Result<Self> {
        if spec.requirements.len() != ids.len() {
            return Err(FabricError::InvalidInput {
                field: "planner_input",
                reason: format!(
                    "requirement ID count ({}) must match requirement count ({})",
                    ids.len(),
                    spec.requirements.len()
                ),
            });
        }
        let requirements = spec
            .requirements
            .iter()
            .zip(ids.iter())
            .map(|(req, id)| PlannableRequirement {
                id: id.clone(),
                text: req.text.clone(),
                anchor: req.anchor,
            })
            .collect();
        Ok(Self {
            spec_revision: spec.revision.clone(),
            source_digest: spec.source_digest.clone(),
            requirements,
        })
    }
}

/// One acceptance condition proposed by the planner for a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAcceptance {
    pub description: String,
    pub evidence_required: bool,
}

/// T015: one task proposed by the planner model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTask {
    pub id: TaskId,
    pub class: TaskClass,
    pub objective: String,
    /// Requirements this task implements. Non-empty; every entry must be known.
    pub requirement_ids: Vec<RequirementId>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub dependencies: Vec<TaskId>,
    pub scope_paths: Vec<String>,
    pub acceptance: Vec<PlannedAcceptance>,
    pub verifiers: Vec<String>,
}

/// JSON schema for [`PlannerInput`]: the contract the planner model receives.
pub fn planner_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["spec_revision", "source_digest", "requirements"],
        "properties": {
            "spec_revision": { "type": "string", "minLength": 1 },
            "source_digest": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
            "requirements": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "text", "anchor"],
                    "properties": {
                        "id": { "type": "string", "pattern": "^REQ-[0-9a-f]{16}$" },
                        "text": { "type": "string", "minLength": 1 },
                        "anchor": {
                            "type": "object",
                            "required": ["line_start", "line_end"],
                            "properties": {
                                "line_start": { "type": "integer", "minimum": 1 },
                                "line_end": { "type": "integer", "minimum": 1 }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// JSON schema for the planner model's output: an array of [`PlannedTask`].
pub fn planner_output_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "minItems": 1,
        "items": {
            "type": "object",
            "required": ["id", "class", "objective", "requirement_ids", "inputs",
                         "outputs", "dependencies", "scope_paths", "acceptance", "verifiers"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "class": { "type": "string", "minLength": 1 },
                "objective": { "type": "string", "minLength": 1 },
                "requirement_ids": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "string", "pattern": "^REQ-[0-9a-f]{16}$" }
                },
                "inputs": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                "outputs": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                "dependencies": { "type": "array", "items": { "type": "string" } },
                "scope_paths": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                "acceptance": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["description", "evidence_required"],
                        "properties": {
                            "description": { "type": "string", "minLength": 1 },
                            "evidence_required": { "type": "boolean" }
                        }
                    }
                },
                "verifiers": { "type": "array", "minItems": 1, "items": { "type": "string" } }
            }
        }
    })
}

/// Validate a planner model's decomposition against the source spec.
///
/// Rejects: empty task list, duplicate task IDs, unknown task classes,
/// empty objective/inputs/outputs/scope/acceptance/verifiers, tasks with no
/// requirement references, references to unknown requirements, dependencies
/// on unknown tasks, self-dependencies, and requirements with no covering
/// task. A decomposition that drops a requirement is invalid, full stop.
pub fn validate_decomposition(tasks: &[PlannedTask], input: &PlannerInput) -> Result<()> {
    if tasks.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "planned_tasks",
            reason: "decomposition produced no tasks".to_string(),
        });
    }

    let known_requirements: HashSet<&str> =
        input.requirements.iter().map(|r| r.id.as_str()).collect();

    let mut seen_ids: HashSet<String> = HashSet::new();
    for task in tasks {
        if !seen_ids.insert(task.id.as_str().to_string()) {
            return Err(FabricError::Conflict(format!(
                "duplicate planned task id {}",
                task.id.as_str()
            )));
        }
        task.class.require_known()?;
        require_non_empty("planned_task.objective", &task.objective, &task.id)?;
        require_non_empty_list("planned_task.inputs", &task.inputs, &task.id)?;
        require_non_empty_list("planned_task.outputs", &task.outputs, &task.id)?;
        require_non_empty_list("planned_task.scope_paths", &task.scope_paths, &task.id)?;
        require_non_empty_list("planned_task.verifiers", &task.verifiers, &task.id)?;
        if task.acceptance.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "planned_task.acceptance",
                reason: format!("task {} has no acceptance conditions", task.id.as_str()),
            });
        }
        for acc in &task.acceptance {
            if acc.description.trim().is_empty() {
                return Err(FabricError::InvalidInput {
                    field: "planned_task.acceptance",
                    reason: format!(
                        "task {} has an empty acceptance description",
                        task.id.as_str()
                    ),
                });
            }
        }
        if task.requirement_ids.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "planned_task.requirement_ids",
                reason: format!(
                    "task {} references no requirements",
                    task.id.as_str()
                ),
            });
        }
        for req_id in &task.requirement_ids {
            if !known_requirements.contains(req_id.as_str()) {
                return Err(FabricError::InvalidInput {
                    field: "planned_task.requirement_ids",
                    reason: format!(
                        "task {} references unknown requirement {}",
                        task.id.as_str(),
                        req_id.as_str()
                    ),
                });
            }
        }
    }

    let task_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for task in tasks {
        for dep in &task.dependencies {
            if dep.as_str() == task.id.as_str() {
                return Err(FabricError::InvalidInput {
                    field: "planned_task.dependencies",
                    reason: format!("task {} depends on itself", task.id.as_str()),
                });
            }
            if !task_ids.contains(dep.as_str()) {
                return Err(FabricError::InvalidInput {
                    field: "planned_task.dependencies",
                    reason: format!(
                        "task {} depends on unknown task {}",
                        task.id.as_str(),
                        dep.as_str()
                    ),
                });
            }
        }
    }

    let mut covered: HashSet<&str> = HashSet::new();
    for task in tasks {
        for req_id in &task.requirement_ids {
            covered.insert(req_id.as_str());
        }
    }
    let mut uncovered: Vec<&str> = known_requirements
        .iter()
        .filter(|r| !covered.contains(**r))
        .copied()
        .collect();
    uncovered.sort_unstable();
    if !uncovered.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "coverage",
            reason: format!("requirements with no planned task: {uncovered:?}"),
        });
    }

    Ok(())
}

fn require_non_empty(field: &'static str, value: &str, task_id: &TaskId) -> Result<()> {
    if value.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field,
            reason: format!("task {} has an empty value", task_id.as_str()),
        });
    }
    Ok(())
}

fn require_non_empty_list(field: &'static str, values: &[String], task_id: &TaskId) -> Result<()> {
    if values.is_empty() || values.iter().any(|v| v.trim().is_empty()) {
        return Err(FabricError::InvalidInput {
            field,
            reason: format!(
                "task {} has a missing or empty list entry",
                task_id.as_str()
            ),
        });
    }
    Ok(())
}

/// Convert a validated planned task into an enforceable [`TaskContract`].
///
/// Runs through the T004 builder, so all T017 completeness rules apply.
/// Acceptance predicate IDs are synthesized deterministically as
/// `<task-id>-AC<n>`. Retry policy and risk tier are not planner concerns:
/// they default to a single attempt and medium risk; the
/// dispatcher may tighten them per policy: single attempt, medium risk.
pub fn to_task_contract(task: &PlannedTask, spec_revision: &str) -> Result<TaskContract> {
    let acceptance = task
        .acceptance
        .iter()
        .enumerate()
        .map(|(n, acc)| {
            AcceptancePredicate::new(
                format!("{}-AC{}", task.id.as_str(), n + 1),
                acc.description.clone(),
                acc.evidence_required,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    TaskContract::builder(task.id.clone(), task.class.clone(), task.objective.clone())
        .spec_revision(spec_revision)
        .inputs(task.inputs.clone())
        .outputs(task.outputs.clone())
        .dependencies(task.dependencies.clone())
        .scope(Scope::new(task.scope_paths.clone())?)
        .acceptance(acceptance)
        .verifiers(task.verifiers.clone())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_spec, SourceAnchor};

    fn rid(n: u8) -> RequirementId {
        RequirementId::new(format!("REQ-{:016x}", n)).unwrap()
    }

    fn tid(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }

    fn input() -> PlannerInput {
        PlannerInput {
            spec_revision: SpecId::new("spec:v1").unwrap(),
            source_digest: format!("sha256:{}", "a".repeat(64)),
            requirements: vec![
                PlannableRequirement {
                    id: rid(1),
                    text: "REQ: one".to_string(),
                    anchor: SourceAnchor {
                        line_start: 1,
                        line_end: 1,
                    },
                },
                PlannableRequirement {
                    id: rid(2),
                    text: "REQ: two".to_string(),
                    anchor: SourceAnchor {
                        line_start: 3,
                        line_end: 3,
                    },
                },
            ],
        }
    }

    fn task(id: &str, reqs: Vec<RequirementId>) -> PlannedTask {
        PlannedTask {
            id: tid(id),
            class: TaskClass::Implement,
            objective: format!("implement {id}"),
            requirement_ids: reqs,
            inputs: vec!["spec".to_string()],
            outputs: vec![format!("{id}.out")],
            dependencies: vec![],
            scope_paths: vec!["crates/x".to_string()],
            acceptance: vec![PlannedAcceptance {
                description: "it works".to_string(),
                evidence_required: true,
            }],
            verifiers: vec!["cargo-test".to_string()],
        }
    }

    #[test]
    fn valid_decomposition_passes() {
        let input = input();
        let tasks = vec![task("T001", vec![rid(1)]), task("T002", vec![rid(2)])];
        assert!(validate_decomposition(&tasks, &input).is_ok());
    }

    #[test]
    fn uncovered_requirement_rejected() {
        let input = input();
        let tasks = vec![task("T001", vec![rid(1)])];
        let err = validate_decomposition(&tasks, &input).unwrap_err();
        assert!(err.to_string().contains("no planned task"));
    }

    #[test]
    fn unknown_requirement_reference_rejected() {
        let input = input();
        let tasks = vec![task("T001", vec![rid(1), rid(9)])];
        assert!(validate_decomposition(&tasks, &input).is_err());
    }

    #[test]
    fn duplicate_task_ids_rejected() {
        let input = input();
        let tasks = vec![task("T001", vec![rid(1)]), task("T001", vec![rid(2)])];
        let err = validate_decomposition(&tasks, &input).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn unknown_class_and_self_dependency_rejected() {
        let input = input();
        let mut t = task("T001", vec![rid(1), rid(2)]);
        t.class = TaskClass::Unknown("QUANTUM".to_string());
        assert!(validate_decomposition(&[t], &input).is_err());

        let mut t2 = task("T001", vec![rid(1), rid(2)]);
        t2.dependencies = vec![tid("T001")];
        assert!(validate_decomposition(&[t2], &input).is_err());

        let mut t3 = task("T001", vec![rid(1), rid(2)]);
        t3.dependencies = vec![tid("T999")];
        assert!(validate_decomposition(&[t3], &input).is_err());
    }

    #[test]
    fn empty_fields_rejected() {
        let input = input();
        let mut t = task("T001", vec![rid(1), rid(2)]);
        t.objective = "   ".to_string();
        assert!(validate_decomposition(&[t.clone()], &input).is_err());
        t.objective = "ok".to_string();
        t.inputs = vec![];
        assert!(validate_decomposition(&[t.clone()], &input).is_err());
        t.inputs = vec!["in".to_string()];
        t.acceptance = vec![];
        assert!(validate_decomposition(&[t], &input).is_err());
    }

    #[test]
    fn empty_task_list_rejected() {
        assert!(validate_decomposition(&[], &input()).is_err());
    }

    #[test]
    fn from_spec_builds_planner_input() {
        let spec = parse_spec("REQ: one\n\nREQ: two\n", SpecId::new("spec:v1").unwrap()).unwrap();
        let ids = vec![rid(1), rid(2)];
        let input = PlannerInput::from_spec(&spec, &ids).unwrap();
        assert_eq!(input.requirements.len(), 2);
        assert_eq!(input.requirements[0].text, "REQ: one");
        assert_eq!(input.source_digest, spec.source_digest);
        assert!(PlannerInput::from_spec(&spec, &ids[..1]).is_err());
    }

    #[test]
    fn to_task_contract_produces_valid_contract() {
        let t = task("T010", vec![rid(1)]);
        let contract = to_task_contract(&t, "spec:v1").unwrap();
        assert_eq!(contract.id.as_str(), "T010");
        assert_eq!(contract.spec_revision, "spec:v1");
        assert_eq!(contract.acceptance.len(), 1);
        assert_eq!(contract.acceptance[0].id, "T010-AC1");
        assert!(contract.acceptance[0].evidence_required);
    }

    #[test]
    fn schemas_are_well_formed() {
        let input_schema = planner_input_schema();
        let output_schema = planner_output_schema();
        assert_eq!(input_schema["type"], "object");
        assert_eq!(output_schema["type"], "array");
        // schemas themselves serialize
        serde_json::to_string(&input_schema).unwrap();
        serde_json::to_string(&output_schema).unwrap();
    }

    #[test]
    fn planner_input_round_trips() {
        let input = input();
        let json = serde_json::to_string(&input).unwrap();
        let back: PlannerInput = serde_json::from_str(&json).unwrap();
        assert_eq!(input, back);
    }
}
