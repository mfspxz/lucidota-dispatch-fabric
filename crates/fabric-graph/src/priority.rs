//! T053: deterministic task priority.
//!
//! Priority is a pure function of explicit task features and a configured
//! weight set: risk tier, critical-path depth, age, per-class weight, and
//! estimated duration (which penalizes long tasks so short ones surface).
//! Higher score = more urgent. Ranking breaks ties by TaskId, so identical
//! inputs always produce identical order.

use crate::nodes::TaskNode;
use fabric_types::{RiskTier, TaskClass, TaskId};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Per-class weights. Unknown classes get `unknown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassWeights {
    pub plan: f64,
    pub implement: f64,
    pub review: f64,
    pub repair: f64,
    pub test_generate: f64,
    pub adversarial: f64,
    pub integrate: f64,
    pub verify: f64,
    pub release: f64,
    pub unknown: f64,
}

impl ClassWeights {
    pub fn weight(&self, class: &TaskClass) -> f64 {
        match class {
            TaskClass::Plan => self.plan,
            TaskClass::Implement => self.implement,
            TaskClass::Review => self.review,
            TaskClass::Repair => self.repair,
            TaskClass::TestGenerate => self.test_generate,
            TaskClass::Adversarial => self.adversarial,
            TaskClass::Integrate => self.integrate,
            TaskClass::Verify => self.verify,
            TaskClass::Release => self.release,
            TaskClass::Unknown(_) => self.unknown,
        }
    }
}

impl Default for ClassWeights {
    /// Repairs, adversarial audits, and reviews outrank fresh implementation;
    /// planning is cheapest to defer.
    fn default() -> Self {
        Self {
            plan: 1.0,
            implement: 3.0,
            review: 5.0,
            repair: 8.0,
            test_generate: 2.0,
            adversarial: 7.0,
            integrate: 4.0,
            verify: 6.0,
            release: 9.0,
            unknown: 0.5,
        }
    }
}

/// Configurable weights for the priority score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorityConfig {
    pub risk_weight: f64,
    pub depth_weight: f64,
    pub age_weight_per_hour: f64,
    pub duration_penalty_per_hour: f64,
    pub class_weights: ClassWeights,
}

impl Default for PriorityConfig {
    fn default() -> Self {
        Self {
            risk_weight: 2.0,
            depth_weight: 1.5,
            age_weight_per_hour: 0.1,
            duration_penalty_per_hour: 0.5,
            class_weights: ClassWeights::default(),
        }
    }
}

impl PriorityConfig {
    /// Reject non-finite weights: NaN would silently poison every ranking.
    pub fn validate(&self) -> fabric_types::Result<()> {
        for (name, v) in [
            ("risk_weight", self.risk_weight),
            ("depth_weight", self.depth_weight),
            ("age_weight_per_hour", self.age_weight_per_hour),
            ("duration_penalty_per_hour", self.duration_penalty_per_hour),
        ] {
            if !v.is_finite() {
                return Err(fabric_types::FabricError::InvalidInput {
                    field: "priority_config",
                    reason: format!("{name} must be finite"),
                });
            }
        }
        Ok(())
    }
}

fn risk_score(risk: RiskTier) -> f64 {
    match risk {
        RiskTier::Low => 1.0,
        RiskTier::Medium => 2.0,
        RiskTier::High => 3.0,
        RiskTier::Critical => 4.0,
    }
}

/// Explicit features of one task used for priority. No hidden state.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskFeatures {
    pub risk: RiskTier,
    pub critical_depth: u64,
    pub age_hours: f64,
    pub class: TaskClass,
    pub estimated_duration_hours: f64,
}

/// Deterministic priority score. Higher = more urgent.
pub fn priority_score(features: &TaskFeatures, config: &PriorityConfig) -> f64 {
    config.risk_weight * risk_score(features.risk)
        + config.depth_weight * features.critical_depth as f64
        + config.age_weight_per_hour * features.age_hours.max(0.0)
        + config.class_weights.weight(&features.class)
        - config.duration_penalty_per_hour * features.estimated_duration_hours.max(0.0)
}

/// Rank tasks by descending score; ties broken by TaskId ascending.
pub fn rank_tasks(tasks: &[(&TaskNode, TaskFeatures)], config: &PriorityConfig) -> Vec<TaskId> {
    let mut scored: Vec<(f64, TaskId)> = tasks
        .iter()
        .map(|(n, f)| (priority_score(f, config), n.id.clone()))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::test_node;

    fn features(class: TaskClass) -> TaskFeatures {
        TaskFeatures {
            risk: RiskTier::Medium,
            critical_depth: 0,
            age_hours: 0.0,
            class,
            estimated_duration_hours: 1.0,
        }
    }

    #[test]
    fn higher_risk_outranks() {
        let cfg = PriorityConfig::default();
        let hi = TaskFeatures {
            risk: RiskTier::Critical,
            ..features(TaskClass::Implement)
        };
        let lo = TaskFeatures {
            risk: RiskTier::Low,
            ..features(TaskClass::Implement)
        };
        assert!(priority_score(&hi, &cfg) > priority_score(&lo, &cfg));
    }

    #[test]
    fn depth_and_age_raise_priority_duration_lowers_it() {
        let cfg = PriorityConfig::default();
        let base = priority_score(&features(TaskClass::Implement), &cfg);
        let deeper = TaskFeatures {
            critical_depth: 5,
            ..features(TaskClass::Implement)
        };
        let older = TaskFeatures {
            age_hours: 48.0,
            ..features(TaskClass::Implement)
        };
        let longer = TaskFeatures {
            estimated_duration_hours: 20.0,
            ..features(TaskClass::Implement)
        };
        assert!(priority_score(&deeper, &cfg) > base);
        assert!(priority_score(&older, &cfg) > base);
        assert!(priority_score(&longer, &cfg) < base);
    }

    #[test]
    fn ranking_is_deterministic_with_tie_break() {
        let cfg = PriorityConfig::default();
        let a = test_node("T002", TaskClass::Implement);
        let b = test_node("T001", TaskClass::Implement);
        let tasks = vec![
            (&a, features(TaskClass::Implement)),
            (&b, features(TaskClass::Implement)),
        ];
        let ranked = rank_tasks(&tasks, &cfg);
        assert_eq!(
            ranked,
            vec![TaskId::new("T001").unwrap(), TaskId::new("T002").unwrap()]
        );
        // same input, same output
        assert_eq!(rank_tasks(&tasks, &cfg), ranked);
    }

    #[test]
    fn class_weights_apply() {
        let cfg = PriorityConfig::default();
        let repair = priority_score(&features(TaskClass::Repair), &cfg);
        let plan = priority_score(&features(TaskClass::Plan), &cfg);
        assert!(repair > plan);
    }

    #[test]
    fn rejects_non_finite_weights() {
        let cfg = PriorityConfig {
            risk_weight: f64::NAN,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }
}
