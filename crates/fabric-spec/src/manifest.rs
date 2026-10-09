//! T020: emit a versioned machine-readable work-order manifest.
//!
//! The manifest binds stable task IDs, dependency edges, prompt digests, and
//! coverage statistics into one versioned document. [`WorkOrderManifest::manifest_digest`]
//! pins the exact bytes of everything above it, so any tampering or drift is
//! detectable by recomputation.

use crate::dag::Dag;
use crate::parser::{ParsedSpec, SourceAnchor};
use fabric_types::{FabricError, Result, TaskContract, TaskId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

/// Manifest format version. Bump when the schema changes.
pub const MANIFEST_VERSION: &str = "1";

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// One task's entry in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskManifestEntry {
    pub stable_id: TaskId,
    pub spec_revision: String,
    pub class: String,
    pub objective: String,
    /// Dependency edges, sorted for determinism.
    pub dependencies: Vec<TaskId>,
    /// Digest of the exact rendered worker prompt (T019).
    pub prompt_digest: String,
    pub acceptance_predicate_ids: Vec<String>,
    pub verifier_ids: Vec<String>,
    pub risk_tier: String,
}

/// Coverage statistics over requirements and tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageStats {
    pub total_requirements: usize,
    pub requirements_with_tasks: usize,
    pub total_tasks: usize,
    /// (class name, count), sorted by class name.
    pub tasks_per_class: Vec<(String, usize)>,
}

/// Versioned machine-readable work-order manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkOrderManifest {
    pub manifest_version: String,
    pub spec_revision: String,
    pub spec_source_digest: String,
    /// Entries in deterministic topological order (dependencies first).
    pub tasks: Vec<TaskManifestEntry>,
    pub coverage: CoverageStats,
    /// `sha256:<hex>` over the canonical JSON of every field above.
    pub manifest_digest: String,
}

/// Emit the work-order manifest.
///
/// * `contracts` — the validated task contracts.
/// * `dag` — the dependency DAG built from `contracts`.
/// * `spec` — the parsed master specification.
/// * `prompt_digests` — rendered-prompt digest per task ID; every task must
///   have one, otherwise emission fails (a task without a prompt digest is
///   not dispatchable).
/// * `covered_anchors` — source anchors of requirements that have at least
///   one linked task (traceability input from T014).
pub fn emit_manifest(
    contracts: &[TaskContract],
    dag: &Dag,
    spec: &ParsedSpec,
    prompt_digests: &HashMap<TaskId, String>,
    covered_anchors: &[SourceAnchor],
) -> Result<WorkOrderManifest> {
    let by_id: HashMap<&TaskId, &TaskContract> =
        contracts.iter().map(|c| (&c.id, c)).collect();

    let mut tasks = Vec::with_capacity(contracts.len());
    for id in dag.topological_order() {
        let c = by_id.get(&id).ok_or_else(|| FabricError::InvalidInput {
            field: "dag",
            reason: format!("dag contains unknown task {}", id.as_str()),
        })?;
        let prompt_digest = prompt_digests.get(&id).ok_or_else(|| FabricError::InvalidInput {
            field: "prompt_digests",
            reason: format!("missing prompt digest for {}", id.as_str()),
        })?;
        let mut dependencies: Vec<TaskId> = c.dependencies.clone();
        dependencies.sort();
        let mut acceptance_predicate_ids: Vec<String> =
            c.acceptance.iter().map(|p| p.id.clone()).collect();
        acceptance_predicate_ids.sort();
        let mut verifier_ids = c.verifiers.clone();
        verifier_ids.sort();
        tasks.push(TaskManifestEntry {
            stable_id: c.id.clone(),
            spec_revision: c.spec_revision.clone(),
            class: c.class.name().to_string(),
            objective: c.objective.clone(),
            dependencies,
            prompt_digest: prompt_digest.clone(),
            acceptance_predicate_ids,
            verifier_ids,
            risk_tier: match c.risk_tier {
                fabric_types::RiskTier::Low => "LOW",
                fabric_types::RiskTier::Medium => "MEDIUM",
                fabric_types::RiskTier::High => "HIGH",
                fabric_types::RiskTier::Critical => "CRITICAL",
            }
            .to_string(),
        });
    }

    let mut per_class: BTreeMap<String, usize> = BTreeMap::new();
    for t in &tasks {
        *per_class.entry(t.class.clone()).or_default() += 1;
    }
    let covered: std::collections::HashSet<(u32, u32)> = covered_anchors
        .iter()
        .map(|a| (a.line_start, a.line_end))
        .collect();
    let requirements_with_tasks = spec
        .requirements
        .iter()
        .filter(|r| covered.contains(&(r.anchor.line_start, r.anchor.line_end)))
        .count();

    let mut manifest = WorkOrderManifest {
        manifest_version: MANIFEST_VERSION.to_string(),
        spec_revision: spec.revision.to_string(),
        spec_source_digest: spec.source_digest.clone(),
        tasks,
        coverage: CoverageStats {
            total_requirements: spec.requirements.len(),
            requirements_with_tasks,
            total_tasks: by_id.len(),
            tasks_per_class: per_class.into_iter().collect(),
        },
        manifest_digest: String::new(),
    };
    let canonical = serde_json::to_vec(&manifest).map_err(|e| FabricError::InvalidInput {
        field: "manifest",
        reason: format!("serialization failed: {e}"),
    })?;
    manifest.manifest_digest = sha256_hex(&canonical);
    Ok(manifest)
}

/// Recompute and check a manifest's digest. Returns true when intact.
pub fn verify_manifest_digest(manifest: &WorkOrderManifest) -> bool {
    let mut clone = manifest.clone();
    let claimed = std::mem::take(&mut clone.manifest_digest);
    let canonical = match serde_json::to_vec(&clone) {
        Ok(v) => v,
        Err(_) => return false,
    };
    sha256_hex(&canonical) == claimed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Requirement;
    use fabric_types::{AcceptancePredicate, Scope, SpecId, TaskClass, TaskContract};

    fn contract(id: &str, deps: &[&str]) -> TaskContract {
        TaskContract::builder(TaskId::new(id).unwrap(), TaskClass::Implement, format!("do {id}"))
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["in".to_string()])
            .outputs(vec![format!("{id}.out")])
            .dependencies(deps.iter().map(|d| TaskId::new(*d).unwrap()).collect())
            .scope(Scope::new(vec!["crates/x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p1", "done", true).unwrap()])
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    fn spec() -> ParsedSpec {
        ParsedSpec {
            revision: SpecId::new("master-spec").unwrap(),
            source_digest: "sha256:00".to_string(),
            requirements: vec![
                Requirement {
                    anchor: SourceAnchor { line_start: 1, line_end: 3 },
                    text: "R1".to_string(),
                },
                Requirement {
                    anchor: SourceAnchor { line_start: 5, line_end: 7 },
                    text: "R2 uncovered".to_string(),
                },
            ],
        }
    }

    fn digests(ids: &[&str]) -> HashMap<TaskId, String> {
        ids.iter()
            .map(|id| (TaskId::new(*id).unwrap(), format!("sha256:digest-{id}")))
            .collect()
    }

    #[test]
    fn emits_versioned_manifest_in_topological_order() {
        let cs = vec![contract("T002", &["T001"]), contract("T001", &[])];
        let dag = Dag::build(&cs).unwrap();
        let m = emit_manifest(
            &cs,
            &dag,
            &spec(),
            &digests(&["T001", "T002"]),
            &[SourceAnchor { line_start: 1, line_end: 3 }],
        )
        .unwrap();
        assert_eq!(m.manifest_version, "1");
        assert_eq!(m.tasks.len(), 2);
        assert_eq!(m.tasks[0].stable_id.as_str(), "T001");
        assert_eq!(m.tasks[1].stable_id.as_str(), "T002");
        assert_eq!(m.tasks[1].dependencies, vec![TaskId::new("T001").unwrap()]);
        assert_eq!(m.tasks[0].prompt_digest, "sha256:digest-T001");
        assert_eq!(m.coverage.total_requirements, 2);
        assert_eq!(m.coverage.requirements_with_tasks, 1);
        assert_eq!(m.coverage.total_tasks, 2);
        assert!(verify_manifest_digest(&m));
    }

    #[test]
    fn emission_is_deterministic() {
        let cs = vec![contract("T002", &["T001"]), contract("T001", &[])];
        let dag = Dag::build(&cs).unwrap();
        let d = digests(&["T001", "T002"]);
        let m1 = emit_manifest(&cs, &dag, &spec(), &d, &[]).unwrap();
        let m2 = emit_manifest(&cs, &dag, &spec(), &d, &[]).unwrap();
        assert_eq!(m1.manifest_digest, m2.manifest_digest);
        let json1 = serde_json::to_string(&m1).unwrap();
        let json2 = serde_json::to_string(&m2).unwrap();
        assert_eq!(json1, json2);
    }

    #[test]
    fn tampering_breaks_digest() {
        let cs = vec![contract("T001", &[])];
        let dag = Dag::build(&cs).unwrap();
        let mut m = emit_manifest(&cs, &dag, &spec(), &digests(&["T001"]), &[]).unwrap();
        assert!(verify_manifest_digest(&m));
        m.tasks[0].objective = "tampered".to_string();
        assert!(!verify_manifest_digest(&m));
    }

    #[test]
    fn missing_prompt_digest_fails_emission() {
        let cs = vec![contract("T001", &[])];
        let dag = Dag::build(&cs).unwrap();
        let err = emit_manifest(&cs, &dag, &spec(), &HashMap::new(), &[]).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }
}
