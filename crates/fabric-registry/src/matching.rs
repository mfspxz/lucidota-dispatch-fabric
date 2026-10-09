//! T027: deterministic capability matching.
//!
//! An endpoint is eligible for a task when it is enabled, offers every
//! required capability, supports structured output when the task requires it,
//! and its context window covers the task's context budget. The result is
//! sorted by [`EndpointId`], so identical registry + requirement inputs always
//! produce identical outputs regardless of registration order.
//!
//! Quality-tier matching (I1 integrator repair): `Endpoint.max_quality_tier`
//! is the endpoint-side counterpart of `CapabilityRequirement.quality_tier`;
//! an endpoint is eligible only when its ceiling meets or exceeds the
//! required tier. Tiers order Low < Standard < High.

use crate::endpoints::{Endpoint, EndpointRegistry};
use fabric_types::{CapabilityRequirement, EndpointId};

/// True when `endpoint` can serve `req` on capability, structured-output, and
/// context-window grounds. Pure function: fully unit-testable.
pub fn endpoint_eligible(req: &CapabilityRequirement, endpoint: &Endpoint) -> bool {
    if !endpoint.enabled {
        return false;
    }
    if !req
        .capabilities
        .iter()
        .all(|c| endpoint.capabilities.iter().any(|e| e == c))
    {
        return false;
    }
    if req.structured_output && !endpoint.structured_output {
        return false;
    }
    if endpoint.max_quality_tier < req.quality_tier {
        return false;
    }
    if let Some(n) = req.context_budget.max_input_tokens {
        if endpoint.context_window.input_tokens < n {
            return false;
        }
    }
    if let Some(n) = req.context_budget.max_output_tokens {
        if endpoint.context_window.output_tokens < n {
            return false;
        }
    }
    true
}

/// Deterministic matching: eligible endpoint IDs in [`EndpointId`] order.
/// Same registry + requirement => same output, always.
pub fn match_endpoints(
    req: &CapabilityRequirement,
    registry: &EndpointRegistry,
) -> Vec<EndpointId> {
    let mut out: Vec<EndpointId> = registry
        .enabled()
        .iter()
        .filter(|e| endpoint_eligible(req, e))
        .map(|e| e.id.clone())
        .collect();
    // Sorted explicitly so determinism never depends on the registry's
    // internal iteration order.
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ContextWindow, TransportKind};
    use fabric_types::{ContextBudget, ModelId, ProviderId, QualityTier};

    fn endpoint(
        id: &str,
        caps: &[&str],
        structured: bool,
        in_tok: u64,
        out_tok: u64,
        tier: QualityTier,
    ) -> Endpoint {
        Endpoint {
            id: EndpointId::new(id).unwrap(),
            provider: ProviderId::new("p").unwrap(),
            model: ModelId::new("m").unwrap(),
            transport: TransportKind::Http,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            context_window: ContextWindow {
                input_tokens: in_tok,
                output_tokens: out_tok,
            },
            structured_output: structured,
            max_quality_tier: tier,
            enabled: true,
        }
    }

    fn req() -> CapabilityRequirement {
        CapabilityRequirement::new(
            vec!["coding".to_string()],
            QualityTier::Standard,
            ContextBudget {
                max_input_tokens: Some(1000),
                max_output_tokens: Some(500),
            },
            true,
            None,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn eligible_when_all_requirements_met() {
        let e = endpoint("ep-a", &["coding", "review"], true, 8000, 2000, QualityTier::High);
        assert!(endpoint_eligible(&req(), &e));
    }

    #[test]
    fn rejects_disabled_endpoint() {
        let mut e = endpoint("ep-a", &["coding"], true, 8000, 2000, QualityTier::High);
        e.enabled = false;
        assert!(!endpoint_eligible(&req(), &e));
    }

    #[test]
    fn rejects_missing_capability() {
        let e = endpoint("ep-a", &["review"], true, 8000, 2000, QualityTier::High);
        assert!(!endpoint_eligible(&req(), &e));
    }

    #[test]
    fn rejects_structured_output_mismatch() {
        let e = endpoint("ep-a", &["coding"], false, 8000, 2000, QualityTier::High);
        assert!(!endpoint_eligible(&req(), &e));
    }

    #[test]
    fn rejects_insufficient_context_window() {
        // input window too small
        let e = endpoint("ep-a", &["coding"], true, 100, 2000, QualityTier::High);
        assert!(!endpoint_eligible(&req(), &e));
        // output window too small
        let e = endpoint("ep-a", &["coding"], true, 8000, 10, QualityTier::High);
        assert!(!endpoint_eligible(&req(), &e));
    }

    #[test]
    fn unbounded_budget_matches_any_window() {
        let r = CapabilityRequirement::new(
            vec!["coding".to_string()],
            QualityTier::Low,
            ContextBudget::unbounded(),
            false,
            None,
            vec![],
        )
        .unwrap();
        let e = endpoint("ep-a", &["coding"], false, 1, 1, QualityTier::High);
        assert!(endpoint_eligible(&r, &e));
    }

    #[test]
    fn match_endpoints_is_deterministic_and_sorted() {
        let mut reg = EndpointRegistry::new();
        for id in ["ep-c", "ep-a", "ep-b"] {
            reg.register(endpoint(id, &["coding"], true, 8000, 2000, QualityTier::High))
                .unwrap();
        }
        // a low-tier endpoint is registered but must not match a Standard req
        reg.register(endpoint("ep-0", &["coding"], true, 8000, 2000, QualityTier::Low))
            .unwrap();
        let first = match_endpoints(&req(), &reg);
        let second = match_endpoints(&req(), &reg);
        assert_eq!(first, second);
        let ids: Vec<_> = first.iter().map(|e| e.as_str()).collect();
        assert_eq!(ids, vec!["ep-a", "ep-b", "ep-c"]);
    }

    #[test]
    fn rejects_insufficient_quality_tier() {
        // req() demands Standard; a Low-ceiling endpoint is ineligible,
        // a High-ceiling endpoint is eligible.
        let low = endpoint("ep-low", &["coding"], true, 8000, 2000, QualityTier::Low);
        assert!(!endpoint_eligible(&req(), &low));
        let high = endpoint("ep-high", &["coding"], true, 8000, 2000, QualityTier::High);
        assert!(endpoint_eligible(&req(), &high));
    }
}
