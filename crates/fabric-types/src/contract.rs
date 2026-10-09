//! T004: TaskContract.
//!
//! The complete, validated description of one unit of work: ID, spec
//! revision, class, objective, inputs, outputs, dependencies, scope,
//! acceptance predicates, verifiers, retry policy, and risk tier.
//! Construction goes through a builder that rejects incomplete contracts
//! (this is also the T017 enforcement point used by the spec compiler).

use crate::class::TaskClass;
use crate::error::{FabricError, Result};
use crate::ids::TaskId;
use serde::{Deserialize, Serialize};
use sha2::Digest;

/// One testable acceptance condition on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptancePredicate {
    pub id: String,
    pub description: String,
    /// Whether passing requires linked evidence (never a bare claim).
    pub evidence_required: bool,
}

impl AcceptancePredicate {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        evidence_required: bool,
    ) -> Result<Self> {
        let id = id.into();
        let description = description.into();
        if id.is_empty() || description.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "acceptance_predicate",
                reason: "id and description must be non-empty".to_string(),
            });
        }
        Ok(Self {
            id,
            description,
            evidence_required,
        })
    }
}

/// Risk tier gates how much autonomy the dispatcher may exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskTier {
    Low,
    Medium,
    High,
    Critical,
}

/// Backoff strategy between attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackoffKind {
    None,
    Linear,
    Exponential,
}

/// Bounded retry policy for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: BackoffKind,
    pub base_delay_ms: u64,
    /// Total retries allowed across all attempts of this task.
    pub retry_budget: u32,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u32,
        backoff: BackoffKind,
        base_delay_ms: u64,
        retry_budget: u32,
    ) -> Result<Self> {
        if !(1..=64).contains(&max_attempts) {
            return Err(FabricError::InvalidInput {
                field: "max_attempts",
                reason: "must be 1..=64".to_string(),
            });
        }
        Ok(Self {
            max_attempts,
            backoff,
            base_delay_ms,
            retry_budget,
        })
    }

    /// No retries: exactly one attempt.
    pub fn once() -> Self {
        Self {
            max_attempts: 1,
            backoff: BackoffKind::None,
            base_delay_ms: 0,
            retry_budget: 0,
        }
    }
}

/// Filesystem scope a task's worker is allowed to touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Allowed path prefixes, relative to the workspace root. Non-empty.
    pub allowed_paths: Vec<String>,
}

impl Scope {
    pub fn new(allowed_paths: Vec<String>) -> Result<Self> {
        if allowed_paths.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "scope.allowed_paths",
                reason: "at least one allowed path is required".to_string(),
            });
        }
        if allowed_paths.iter().any(|p| p.is_empty()) {
            return Err(FabricError::InvalidInput {
                field: "scope.allowed_paths",
                reason: "paths must be non-empty".to_string(),
            });
        }
        Ok(Self { allowed_paths })
    }
}

/// T004: the full contract for one unit of work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContract {
    pub id: TaskId,
    pub spec_revision: String,
    pub class: TaskClass,
    pub objective: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub dependencies: Vec<TaskId>,
    pub scope: Scope,
    pub acceptance: Vec<AcceptancePredicate>,
    pub verifiers: Vec<String>,
    pub retry: RetryPolicy,
    pub risk_tier: RiskTier,
    /// Declared failure path: what happens when attempts exhaust or a
    /// verifier fails (e.g. "quarantine artifacts, notify governor").
    /// T017 rejects contracts where this is missing.
    pub failure_path: Option<String>,
}

impl TaskContract {
    /// Start building a contract. `build()` enforces completeness (T017).
    pub fn builder(id: TaskId, class: TaskClass, objective: impl Into<String>) -> Builder {
        Builder {
            id,
            spec_revision: None,
            class,
            objective: objective.into(),
            inputs: vec![],
            outputs: vec![],
            dependencies: vec![],
            scope: None,
            acceptance: vec![],
            verifiers: vec![],
            retry: RetryPolicy::once(),
            risk_tier: RiskTier::Medium,
            failure_path: None,
        }
    }
}

/// Builder that rejects incomplete contracts at `build()` time.
pub struct Builder {
    id: TaskId,
    spec_revision: Option<String>,
    class: TaskClass,
    objective: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    dependencies: Vec<TaskId>,
    scope: Option<Scope>,
    acceptance: Vec<AcceptancePredicate>,
    verifiers: Vec<String>,
    retry: RetryPolicy,
    risk_tier: RiskTier,
    failure_path: Option<String>,
}

impl TaskContract {
    /// Contract version (T085): sha256 over the canonical JSON serialization.
    /// Stable for a given struct value; no separate version counter to drift.
    pub fn contract_version(&self) -> String {
        let json = serde_json::to_string(self).expect("TaskContract serializes");
        let digest = sha2::Sha256::digest(json.as_bytes());
        format!("sha256:{digest:x}")
    }
}

impl Builder {
    pub fn spec_revision(mut self, r: impl Into<String>) -> Self {
        self.spec_revision = Some(r.into());
        self
    }
    pub fn inputs(mut self, v: Vec<String>) -> Self {
        self.inputs = v;
        self
    }
    pub fn outputs(mut self, v: Vec<String>) -> Self {
        self.outputs = v;
        self
    }
    pub fn dependencies(mut self, v: Vec<TaskId>) -> Self {
        self.dependencies = v;
        self
    }
    pub fn scope(mut self, s: Scope) -> Self {
        self.scope = Some(s);
        self
    }
    pub fn acceptance(mut self, v: Vec<AcceptancePredicate>) -> Self {
        self.acceptance = v;
        self
    }
    pub fn verifiers(mut self, v: Vec<String>) -> Self {
        self.verifiers = v;
        self
    }
    pub fn retry(mut self, r: RetryPolicy) -> Self {
        self.retry = r;
        self
    }
    pub fn risk_tier(mut self, t: RiskTier) -> Self {
        self.risk_tier = t;
        self
    }
    pub fn failure_path(mut self, p: impl Into<String>) -> Self {
        self.failure_path = Some(p.into());
        self
    }

    /// Validate completeness: objective, inputs, outputs, scope, acceptance,
    /// verifiers, and spec revision are all mandatory.
    pub fn build(self) -> Result<TaskContract> {
        if self.objective.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "objective",
                reason: "must be non-empty".to_string(),
            });
        }
        let spec_revision = self.spec_revision.filter(|s| !s.trim().is_empty()).ok_or(
            FabricError::InvalidInput {
                field: "spec_revision",
                reason: "missing".to_string(),
            },
        )?;
        if self.inputs.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "inputs",
                reason: "missing".to_string(),
            });
        }
        if self.outputs.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "outputs",
                reason: "missing".to_string(),
            });
        }
        let scope = self.scope.ok_or(FabricError::InvalidInput {
            field: "scope",
            reason: "missing".to_string(),
        })?;
        if self.acceptance.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "acceptance",
                reason: "missing".to_string(),
            });
        }
        if self.verifiers.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "verifiers",
                reason: "missing".to_string(),
            });
        }
        Ok(TaskContract {
            id: self.id,
            spec_revision,
            class: self.class,
            objective: self.objective,
            inputs: self.inputs,
            outputs: self.outputs,
            dependencies: self.dependencies,
            scope,
            acceptance: self.acceptance,
            verifiers: self.verifiers,
            retry: self.retry,
            risk_tier: self.risk_tier,
            failure_path: self.failure_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_builder() -> Builder {
        TaskContract::builder(TaskId::new("T001").unwrap(), TaskClass::Implement, "do the thing")
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["master-spec".to_string()])
            .outputs(vec!["T001.implementation".to_string()])
            .scope(Scope::new(vec!["crates/fabric-types".to_string()]).unwrap())
            .acceptance(vec![
                AcceptancePredicate::new("p1", "it works", true).unwrap()
            ])
            .verifiers(vec!["cargo-test".to_string()])
    }

    #[test]
    fn complete_contract_builds() {
        assert!(full_builder().build().is_ok());
    }

    #[test]
    fn rejects_each_missing_field() {
        assert!(full_builder().spec_revision("").build().is_err());
        let b = TaskContract::builder(TaskId::new("T1").unwrap(), TaskClass::Plan, "x");
        assert!(b.build().is_err()); // everything missing
        assert!(full_builder().inputs(vec![]).build().is_err());
        assert!(full_builder().outputs(vec![]).build().is_err());
        assert!(full_builder().acceptance(vec![]).build().is_err());
        assert!(full_builder().verifiers(vec![]).build().is_err());
    }

    #[test]
    fn contract_round_trips() {
        let c = full_builder().build().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: TaskContract = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn contract_version_is_stable_digest() {
        let c = full_builder().build().unwrap();
        let v1 = c.contract_version();
        let v2 = c.contract_version();
        assert_eq!(v1, v2);
        assert!(v1.starts_with("sha256:"));
        let other = full_builder().risk_tier(RiskTier::High).build().unwrap();
        assert_ne!(v1, other.contract_version());
    }

    #[test]
    fn failure_path_is_optional_at_build() {
        // T004 does not require it; T017's validator rejects its absence.
        let c = full_builder().build().unwrap();
        assert_eq!(c.failure_path, None);
        let c2 = full_builder().failure_path("quarantine and notify").build().unwrap();
        assert_eq!(c2.failure_path.as_deref(), Some("quarantine and notify"));
    }

    #[test]
    fn retry_policy_bounds() {
        assert!(RetryPolicy::new(0, BackoffKind::None, 0, 0).is_err());
        assert!(RetryPolicy::new(65, BackoffKind::None, 0, 0).is_err());
        assert!(RetryPolicy::new(3, BackoffKind::Exponential, 100, 10).is_ok());
    }
}
