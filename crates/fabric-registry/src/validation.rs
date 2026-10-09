//! T023: endpoint declaration validation.
//!
//! Runs before registration. Rejects: invalid capabilities (not in the
//! catalog), impossible context settings (zero token windows), duplicate IDs
//! (already registered), and missing mandatory fields (empty capability list).

use crate::catalog::CapabilityCatalog;
use crate::endpoints::{Endpoint, EndpointRegistry};
use fabric_types::{FabricError, Result};

/// Validate an endpoint declaration.
///
/// Checks, in order:
/// 1. Duplicate ID — the registry must not already hold this endpoint.
/// 2. Mandatory fields — at least one capability.
/// 3. Capability validity — every capability is in the catalog.
/// 4. Context sanity — input and output token windows are positive.
///
/// Provider/model IDs are validated newtypes already, so emptiness there is
/// impossible by construction.
pub fn validate_endpoint(
    endpoint: &Endpoint,
    catalog: &CapabilityCatalog,
    registry: &EndpointRegistry,
) -> Result<()> {
    if registry.get(&endpoint.id).is_some() {
        return Err(FabricError::Conflict(format!(
            "duplicate endpoint id: {}",
            endpoint.id
        )));
    }
    if endpoint.capabilities.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "capabilities",
            reason: "at least one capability is required".to_string(),
        });
    }
    for cap in &endpoint.capabilities {
        if !catalog.contains(cap) {
            return Err(FabricError::InvalidInput {
                field: "capabilities",
                reason: format!("unknown capability: {cap}"),
            });
        }
    }
    if endpoint.context_window.input_tokens == 0 {
        return Err(FabricError::InvalidInput {
            field: "context_window.input_tokens",
            reason: "must be positive".to_string(),
        });
    }
    if endpoint.context_window.output_tokens == 0 {
        return Err(FabricError::InvalidInput {
            field: "context_window.output_tokens",
            reason: "must be positive".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ContextWindow, TransportKind};
    use fabric_types::{EndpointId, ModelId, ProviderId};

    fn endpoint(id: &str) -> Endpoint {
        Endpoint {
            id: EndpointId::new(id).unwrap(),
            provider: ProviderId::new("acme").unwrap(),
            model: ModelId::new("acme-1").unwrap(),
            transport: TransportKind::Http,
            capabilities: vec!["coding".to_string()],
            context_window: ContextWindow {
                input_tokens: 8192,
                output_tokens: 4096,
            },
            structured_output: false,
            enabled: true,
        }
    }

    #[test]
    fn accepts_a_well_formed_declaration() {
        let registry = EndpointRegistry::new();
        assert!(validate_endpoint(&endpoint("ep-1"), &CapabilityCatalog::standard(), &registry).is_ok());
    }

    #[test]
    fn rejects_duplicate_id() {
        let mut registry = EndpointRegistry::new();
        registry.register(endpoint("ep-1")).unwrap();
        let err = validate_endpoint(&endpoint("ep-1"), &CapabilityCatalog::standard(), &registry)
            .unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
    }

    #[test]
    fn rejects_missing_capabilities() {
        let mut ep = endpoint("ep-2");
        ep.capabilities.clear();
        let err =
            validate_endpoint(&ep, &CapabilityCatalog::standard(), &EndpointRegistry::new())
                .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_unknown_capability() {
        let mut ep = endpoint("ep-3");
        ep.capabilities.push("teleportation".to_string());
        let err =
            validate_endpoint(&ep, &CapabilityCatalog::standard(), &EndpointRegistry::new())
                .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(err.to_string().contains("teleportation"));
    }

    #[test]
    fn rejects_impossible_context_windows() {
        for (input, output) in [(0, 4096), (8192, 0), (0, 0)] {
            let mut ep = endpoint("ep-4");
            ep.context_window = ContextWindow {
                input_tokens: input,
                output_tokens: output,
            };
            assert!(
                validate_endpoint(&ep, &CapabilityCatalog::standard(), &EndpointRegistry::new())
                    .is_err(),
                "input={input} output={output}"
            );
        }
    }
}
