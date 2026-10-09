//! T062: concurrency-limited dispatch.
//!
//! Eligible tasks are assigned to endpoints up to per-endpoint and global
//! concurrency caps. The plan is deterministic: candidates are processed in
//! (endpoint, envelope-digest) order, so identical inputs always produce the
//! identical plan. Anything that cannot be dispatched is deferred with a
//! machine-readable reason, never silently dropped.
//!
//! Quota wiring: [`caps_from_limit_sets`] adapts the frozen `fabric-quota`
//! `LimitSet` interface into [`ConcurrencyCaps`]. It exercises the T041 API
//! surface; the quota cell's bodies are still pending, so this adapter is
//! intentionally not unit-tested here (calling it today would hit `todo!()`).
//! [`plan_dispatch`] is fully tested against hand-built caps.

use crate::envelope::DispatchEnvelope;
use fabric_quota::{LimitKind, LimitSet};
use fabric_registry::EndpointRegistry;
use fabric_types::{EndpointId, FabricError, Result};
use std::collections::HashMap;

/// Concurrency caps for one dispatch round.
#[derive(Debug, Clone)]
pub struct ConcurrencyCaps {
    per_endpoint: HashMap<EndpointId, u32>,
    /// Cap applied to endpoints with no explicit entry. Fail-soft but safe:
    /// one in-flight request per undeclared endpoint.
    pub default_endpoint_cap: u32,
    pub global: u32,
}

impl ConcurrencyCaps {
    pub fn new(global: u32, default_endpoint_cap: u32) -> Self {
        Self {
            per_endpoint: HashMap::new(),
            default_endpoint_cap,
            global,
        }
    }

    pub fn set_endpoint(&mut self, endpoint: EndpointId, cap: u32) {
        self.per_endpoint.insert(endpoint, cap);
    }

    pub fn cap_for(&self, endpoint: &EndpointId) -> u32 {
        self.per_endpoint
            .get(endpoint)
            .copied()
            .unwrap_or(self.default_endpoint_cap)
    }
}

/// Bookkeeping of currently in-flight attempts.
#[derive(Debug, Clone, Default)]
pub struct InFlightLedger {
    per_endpoint: HashMap<EndpointId, u32>,
    global: u32,
}

impl InFlightLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a newly dispatched attempt.
    pub fn acquire(&mut self, endpoint: &EndpointId) {
        *self.per_endpoint.entry(endpoint.clone()).or_insert(0) += 1;
        self.global += 1;
    }

    /// Record a finished attempt. Releasing an endpoint with no in-flight
    /// count is a bookkeeping bug: it is reported, not silently absorbed.
    pub fn release(&mut self, endpoint: &EndpointId) -> Result<()> {
        match self.per_endpoint.get_mut(endpoint) {
            Some(n) if *n > 0 => {
                *n -= 1;
                self.global = self.global.saturating_sub(1);
                Ok(())
            }
            _ => Err(FabricError::Conflict(format!(
                "release of endpoint {endpoint} with no in-flight attempts"
            ))),
        }
    }

    pub fn count(&self, endpoint: &EndpointId) -> u32 {
        self.per_endpoint.get(endpoint).copied().unwrap_or(0)
    }

    pub fn global(&self) -> u32 {
        self.global
    }
}

/// One eligible task paired with its chosen endpoint.
#[derive(Debug, Clone)]
pub struct DispatchCandidate {
    pub envelope: DispatchEnvelope,
    pub endpoint: EndpointId,
}

/// Machine-readable reason a candidate was not dispatched this round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferReason {
    /// Endpoint is not in the registry.
    EndpointUnknown,
    /// Endpoint is registered but disabled.
    EndpointDisabled,
    /// Endpoint already has `in_flight` attempts against a cap of `cap`.
    EndpointAtCap { cap: u32, in_flight: u32 },
    /// Global in-flight count reached the global cap.
    GlobalAtCap { cap: u32, in_flight: u32 },
}

impl DeferReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EndpointUnknown => "ENDPOINT_UNKNOWN",
            Self::EndpointDisabled => "ENDPOINT_DISABLED",
            Self::EndpointAtCap { .. } => "ENDPOINT_AT_CAP",
            Self::GlobalAtCap { .. } => "GLOBAL_AT_CAP",
        }
    }
}

/// A candidate that was deferred, with its reason.
#[derive(Debug, Clone)]
pub struct Deferred {
    pub envelope: DispatchEnvelope,
    pub endpoint: EndpointId,
    pub reason: DeferReason,
}

/// The dispatch decision for one round.
#[derive(Debug, Clone, Default)]
pub struct DispatchPlan {
    pub assignments: Vec<(DispatchEnvelope, EndpointId)>,
    pub deferred: Vec<Deferred>,
}

impl DispatchPlan {
    pub fn assigned_count(&self) -> usize {
        self.assignments.len()
    }
    pub fn deferred_count(&self) -> usize {
        self.deferred.len()
    }
}

/// Assign eligible candidates to endpoints within caps.
///
/// Deterministic: candidates are ordered by (endpoint ID, envelope digest)
/// before assignment, so the same inputs always yield the same plan.
/// Tentative assignments made during the round count against the caps, so a
/// single round can never over-commit an endpoint or the global limit.
pub fn plan_dispatch(
    candidates: Vec<DispatchCandidate>,
    registry: &EndpointRegistry,
    caps: &ConcurrencyCaps,
    in_flight: &InFlightLedger,
) -> DispatchPlan {
    let mut ordered = candidates;
    ordered.sort_by(|a, b| {
        a.endpoint
            .as_str()
            .cmp(b.endpoint.as_str())
            .then_with(|| a.envelope.envelope_digest().cmp(&b.envelope.envelope_digest()))
    });

    let mut plan = DispatchPlan::default();
    // Tentative counts for this round, on top of the durable ledger.
    let mut round_per_endpoint: HashMap<EndpointId, u32> = HashMap::new();
    let mut round_global: u32 = 0;

    for cand in ordered {
        let endpoint = cand.endpoint.clone();
        let effective = |ep: &EndpointId| {
            in_flight.count(ep) + round_per_endpoint.get(ep).copied().unwrap_or(0)
        };

        let reason = match registry.get(&endpoint) {
            None => Some(DeferReason::EndpointUnknown),
            Some(ep) if !ep.enabled => Some(DeferReason::EndpointDisabled),
            Some(_) => {
                let cap = caps.cap_for(&endpoint);
                let used = effective(&endpoint);
                if used >= cap {
                    Some(DeferReason::EndpointAtCap {
                        cap,
                        in_flight: used,
                    })
                } else if in_flight.global() + round_global >= caps.global {
                    Some(DeferReason::GlobalAtCap {
                        cap: caps.global,
                        in_flight: in_flight.global() + round_global,
                    })
                } else {
                    None
                }
            }
        };

        match reason {
            Some(reason) => plan.deferred.push(Deferred {
                envelope: cand.envelope,
                endpoint,
                reason,
            }),
            None => {
                *round_per_endpoint.entry(endpoint.clone()).or_insert(0) += 1;
                round_global += 1;
                plan.assignments.push((cand.envelope, endpoint));
            }
        }
    }

    plan
}

/// Adapt frozen `fabric-quota` limit sets into dispatch caps.
///
/// Reads the `Concurrency` dimension of each endpoint's `LimitSet`; an unset
/// (unknown) dimension falls back to `default_endpoint_cap`. The global cap
/// comes from the `Concurrency` dimension of `global_limits`, defaulting to
/// `default_global` when unknown.
///
/// NOTE: exercises the T041 interface whose bodies are still `todo!()` in the
/// quota cell's worktree. Do not call until T041 lands; kept out of unit tests
/// for that reason.
pub fn caps_from_limit_sets(
    endpoint_limits: &HashMap<EndpointId, LimitSet>,
    global_limits: &LimitSet,
    default_endpoint_cap: u32,
    default_global: u32,
) -> ConcurrencyCaps {
    let global = global_limits
        .get(LimitKind::Concurrency)
        .and_then(|l| l.max)
        .and_then(|m| u32::try_from(m).ok())
        .unwrap_or(default_global);
    let mut caps = ConcurrencyCaps::new(global, default_endpoint_cap);
    let mut ids: Vec<&EndpointId> = endpoint_limits.keys().collect();
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for id in ids {
        let cap = endpoint_limits[id]
            .get(LimitKind::Concurrency)
            .and_then(|l| l.max)
            .and_then(|m| u32::try_from(m).ok())
            .unwrap_or(default_endpoint_cap);
        caps.set_endpoint(id.clone(), cap);
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{create_envelope, EnvelopeLimits};
    use fabric_registry::{ContextWindow, Endpoint, TransportKind};
    use fabric_types::{
        AcceptancePredicate, ModelId, ProviderId, QualityTier, Scope, TaskClass, TaskId,
    };

    fn envelope_for(task: &str, attempt: u32) -> DispatchEnvelope {
        let contract = fabric_types::TaskContract::builder(
            TaskId::new(task).unwrap(),
            TaskClass::Implement,
            "work",
        )
        .spec_revision("rev")
        .inputs(vec!["in".to_string()])
        .outputs(vec!["out".to_string()])
        .scope(Scope::new(vec!["x".to_string()]).unwrap())
        .acceptance(vec![AcceptancePredicate::new("p", "d", true).unwrap()])
        .verifiers(vec!["v".to_string()])
        .build()
        .unwrap();
        create_envelope(
            contract,
            format!("sha256:{task}"),
            EndpointId::new("ep-1").unwrap(),
            EnvelopeLimits {
                timeout_ms: 1000,
                max_tokens: 100,
            },
            "ws".to_string(),
            serde_json::json!({}),
            attempt,
        )
        .unwrap()
    }

    fn registry_with(ids: &[(&str, bool)]) -> EndpointRegistry {
        let mut r = EndpointRegistry::new();
        for (id, enabled) in ids {
            r.register(Endpoint {
                id: EndpointId::new(*id).unwrap(),
                provider: ProviderId::new("p").unwrap(),
                model: ModelId::new("m").unwrap(),
                transport: TransportKind::Http,
                capabilities: vec!["coding".to_string()],
                context_window: ContextWindow {
                    input_tokens: 8000,
                    output_tokens: 2000,
                },
                structured_output: false,
                max_quality_tier: QualityTier::High,
                enabled: *enabled,
            })
            .unwrap();
        }
        r
    }

    fn cand(task: &str, endpoint: &str) -> DispatchCandidate {
        DispatchCandidate {
            envelope: envelope_for(task, 1),
            endpoint: EndpointId::new(endpoint).unwrap(),
        }
    }

    #[test]
    fn assigns_up_to_endpoint_cap() {
        let reg = registry_with(&[("ep-1", true)]);
        let mut caps = ConcurrencyCaps::new(10, 1);
        caps.set_endpoint(EndpointId::new("ep-1").unwrap(), 2);
        let plan = plan_dispatch(
            vec![cand("t1", "ep-1"), cand("t2", "ep-1"), cand("t3", "ep-1")],
            &reg,
            &caps,
            &InFlightLedger::new(),
        );
        assert_eq!(plan.assigned_count(), 2);
        assert_eq!(plan.deferred_count(), 1);
        assert_eq!(plan.deferred[0].reason.code(), "ENDPOINT_AT_CAP");
    }

    #[test]
    fn global_cap_applies_across_endpoints() {
        let reg = registry_with(&[("ep-1", true), ("ep-2", true)]);
        let caps = ConcurrencyCaps::new(1, 5);
        let plan = plan_dispatch(
            vec![cand("t1", "ep-1"), cand("t2", "ep-2")],
            &reg,
            &caps,
            &InFlightLedger::new(),
        );
        assert_eq!(plan.assigned_count(), 1);
        assert_eq!(plan.deferred[0].reason.code(), "GLOBAL_AT_CAP");
    }

    #[test]
    fn defers_unknown_and_disabled_endpoints() {
        let reg = registry_with(&[("ep-1", true), ("ep-2", false)]);
        let caps = ConcurrencyCaps::new(10, 5);
        let plan = plan_dispatch(
            vec![cand("t1", "ep-9"), cand("t2", "ep-2"), cand("t3", "ep-1")],
            &reg,
            &caps,
            &InFlightLedger::new(),
        );
        assert_eq!(plan.assigned_count(), 1);
        let codes: Vec<_> = plan.deferred.iter().map(|d| d.reason.code()).collect();
        assert!(codes.contains(&"ENDPOINT_UNKNOWN"));
        assert!(codes.contains(&"ENDPOINT_DISABLED"));
    }

    #[test]
    fn respects_existing_in_flight_counts() {
        let reg = registry_with(&[("ep-1", true)]);
        let mut caps = ConcurrencyCaps::new(10, 1);
        caps.set_endpoint(EndpointId::new("ep-1").unwrap(), 2);
        let mut in_flight = InFlightLedger::new();
        in_flight.acquire(&EndpointId::new("ep-1").unwrap());
        in_flight.acquire(&EndpointId::new("ep-1").unwrap());
        let plan = plan_dispatch(vec![cand("t1", "ep-1")], &reg, &caps, &in_flight);
        assert_eq!(plan.assigned_count(), 0);
        match &plan.deferred[0].reason {
            DeferReason::EndpointAtCap { cap, in_flight } => {
                assert_eq!((*cap, *in_flight), (2, 2));
            }
            r => panic!("unexpected {r:?}"),
        }
    }

    #[test]
    fn plan_is_deterministic() {
        let reg = registry_with(&[("ep-1", true), ("ep-2", true)]);
        let caps = ConcurrencyCaps::new(10, 5);
        let mk = || {
            plan_dispatch(
                vec![cand("tb", "ep-2"), cand("ta", "ep-1"), cand("tc", "ep-1")],
                &reg,
                &caps,
                &InFlightLedger::new(),
            )
        };
        let a: Vec<String> = mk()
            .assignments
            .iter()
            .map(|(e, _)| e.contract.id.to_string())
            .collect();
        let b: Vec<String> = mk()
            .assignments
            .iter()
            .map(|(e, _)| e.contract.id.to_string())
            .collect();
        assert_eq!(a, b);
        // Deterministic endpoint-major order: both ep-1 tasks before ep-2,
        // ep-1-internal order decided by envelope digest (stable, not task-id).
        let endpoints: Vec<String> = mk()
            .assignments
            .iter()
            .map(|(_, e)| e.to_string())
            .collect();
        assert_eq!(
            endpoints,
            vec!["ep-1".to_string(), "ep-1".to_string(), "ep-2".to_string()]
        );
    }

    #[test]
    fn ledger_release_reports_double_release() {
        let mut l = InFlightLedger::new();
        let ep = EndpointId::new("ep-1").unwrap();
        assert!(l.release(&ep).is_err());
        l.acquire(&ep);
        assert!(l.release(&ep).is_ok());
        assert!(l.release(&ep).is_err());
        assert_eq!(l.global(), 0);
    }
}
