//! T048: weighted fair allocation across task classes.
//!
//! Configurable per-class weights with a smoothed weighted round-robin
//! picker. Starvation-free by construction: every class with pending work and
//! positive weight is picked infinitely often. Classes without a configured
//! weight default to 1.0 so unlisted classes are never starved either.
//! The picker is fully deterministic (ties broken by class name).

use fabric_types::{FabricError, Result, TaskClass};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-class weights. Cloneable so the allocator can own a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassWeights {
    weights: HashMap<TaskClass, f64>,
}

impl ClassWeights {
    pub fn new() -> Self {
        Self {
            weights: HashMap::new(),
        }
    }

    /// Set a class weight. Must be finite and positive.
    pub fn set_weight(&mut self, class: TaskClass, weight: f64) -> Result<()> {
        if !weight.is_finite() || weight <= 0.0 {
            return Err(FabricError::InvalidInput {
                field: "weight",
                reason: "weight must be finite and positive".to_string(),
            });
        }
        self.weights.insert(class, weight);
        Ok(())
    }

    /// Configured weight, or 1.0 when the class has none.
    pub fn weight_of(&self, class: &TaskClass) -> f64 {
        self.weights.get(class).copied().unwrap_or(1.0)
    }
}

impl Default for ClassWeights {
    fn default() -> Self {
        Self::new()
    }
}

/// Smoothed weighted round-robin allocator over task classes.
#[derive(Debug)]
pub struct FairAllocator {
    weights: ClassWeights,
    current: HashMap<TaskClass, f64>,
    served: HashMap<TaskClass, u64>,
}

impl FairAllocator {
    pub fn new(weights: ClassWeights) -> Self {
        Self {
            weights,
            current: HashMap::new(),
            served: HashMap::new(),
        }
    }

    /// Pick the next class among `pending` = (class, queued count).
    /// Returns `None` when nothing is pending. Deterministic.
    pub fn next_class(&mut self, pending: &[(TaskClass, u64)]) -> Option<TaskClass> {
        let mut candidates: Vec<TaskClass> = pending
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(c, _)| c.clone())
            .collect();
        candidates.sort_by(|a, b| a.name().cmp(b.name()));
        candidates.dedup();
        if candidates.is_empty() {
            return None;
        }
        let total: f64 = candidates.iter().map(|c| self.weights.weight_of(c)).sum();
        for c in &candidates {
            *self.current.entry(c.clone()).or_insert(0.0) += self.weights.weight_of(c);
        }
        // Max current weight wins; ties broken toward the lower class name so
        // the choice is deterministic regardless of HashMap order.
        let winner = candidates
            .iter()
            .max_by(|a, b| {
                let ca = self.current.get(*a).copied().unwrap_or(0.0);
                let cb = self.current.get(*b).copied().unwrap_or(0.0);
                ca.partial_cmp(&cb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.name().cmp(a.name()))
            })
            .expect("candidates non-empty")
            .clone();
        *self.current.get_mut(&winner).expect("winner tracked") -= total;
        *self.served.entry(winner.clone()).or_insert(0) += 1;
        Some(winner)
    }

    /// How many times this class has been picked.
    pub fn served(&self, class: &TaskClass) -> u64 {
        self.served.get(class).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights(pairs: &[(TaskClass, f64)]) -> ClassWeights {
        let mut w = ClassWeights::new();
        for (c, v) in pairs {
            w.set_weight(c.clone(), *v).unwrap();
        }
        w
    }

    #[test]
    fn distributes_proportionally_to_weights() {
        let mut a = FairAllocator::new(weights(&[
            (TaskClass::Implement, 3.0),
            (TaskClass::Review, 1.0),
        ]));
        let pending = vec![(TaskClass::Implement, 1000), (TaskClass::Review, 1000)];
        for _ in 0..400 {
            a.next_class(&pending).unwrap();
        }
        let imp = a.served(&TaskClass::Implement);
        let rev = a.served(&TaskClass::Review);
        assert_eq!(imp + rev, 400);
        let ratio = imp as f64 / rev as f64;
        assert!((ratio - 3.0).abs() < 0.15, "ratio {ratio}");
    }

    #[test]
    fn low_weight_class_is_not_starved() {
        let mut a = FairAllocator::new(weights(&[
            (TaskClass::Implement, 100.0),
            (TaskClass::Adversarial, 1.0),
        ]));
        let pending = vec![(TaskClass::Implement, 10_000), (TaskClass::Adversarial, 10_000)];
        // within 101 picks the 1%-weight class must appear at least once
        let mut seen = false;
        for _ in 0..101 {
            if a.next_class(&pending) == Some(TaskClass::Adversarial) {
                seen = true;
                break;
            }
        }
        assert!(seen, "adversarial class starved");
    }

    #[test]
    fn unconfigured_classes_default_to_unit_weight() {
        let mut a = FairAllocator::new(ClassWeights::new());
        let pending = vec![(TaskClass::Plan, 10), (TaskClass::Verify, 10)];
        for _ in 0..20 {
            a.next_class(&pending).unwrap();
        }
        assert_eq!(a.served(&TaskClass::Plan), 10);
        assert_eq!(a.served(&TaskClass::Verify), 10);
    }

    #[test]
    fn empty_pending_returns_none() {
        let mut a = FairAllocator::new(ClassWeights::new());
        assert_eq!(a.next_class(&[]), None);
        assert_eq!(a.next_class(&[(TaskClass::Plan, 0)]), None);
    }

    #[test]
    fn deterministic_across_instances() {
        let mk = || {
            FairAllocator::new(weights(&[
                (TaskClass::Implement, 2.0),
                (TaskClass::Review, 1.0),
                (TaskClass::Repair, 1.0),
            ]))
        };
        let pending = vec![
            (TaskClass::Implement, 50),
            (TaskClass::Review, 50),
            (TaskClass::Repair, 50),
        ];
        let mut a = mk();
        let mut b = mk();
        let sa: Vec<_> = (0..60).map(|_| a.next_class(&pending).unwrap()).collect();
        let sb: Vec<_> = (0..60).map(|_| b.next_class(&pending).unwrap()).collect();
        assert_eq!(sa, sb);
    }

    #[test]
    fn invalid_weights_rejected() {
        let mut w = ClassWeights::new();
        assert!(w.set_weight(TaskClass::Plan, 0.0).is_err());
        assert!(w.set_weight(TaskClass::Plan, -1.0).is_err());
        assert!(w.set_weight(TaskClass::Plan, f64::NAN).is_err());
        assert!(w.set_weight(TaskClass::Plan, f64::INFINITY).is_err());
    }
}
