//! T022 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Register inference endpoints with provider, model, transport, capabilities,
//! context window, structured-output support, and enabled status.
//!
//! Worker R1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{EndpointId, FabricError, ModelId, ProviderId, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How the registry reaches the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransportKind {
    Http,
    Local,
}

/// Context window of an endpoint in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextWindow {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// T022: a registered inference endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: EndpointId,
    pub provider: ProviderId,
    pub model: ModelId,
    pub transport: TransportKind,
    /// Capability names from the [`crate::catalog`] catalog.
    pub capabilities: Vec<String>,
    pub context_window: ContextWindow,
    pub structured_output: bool,
    pub enabled: bool,
}

/// Registry of inference endpoints keyed by stable endpoint ID.
#[derive(Debug, Default)]
pub struct EndpointRegistry {
    endpoints: HashMap<EndpointId, Endpoint>,
}

impl EndpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an endpoint. Duplicate IDs are rejected (see T023 for the
    /// full declaration validation, which runs before registration).
    pub fn register(&mut self, endpoint: Endpoint) -> Result<()> {
        if self.endpoints.contains_key(&endpoint.id) {
            return Err(FabricError::Conflict(format!(
                "duplicate endpoint id: {}",
                endpoint.id
            )));
        }
        self.endpoints.insert(endpoint.id.clone(), endpoint);
        Ok(())
    }

    /// Look up an endpoint by ID.
    pub fn get(&self, id: &EndpointId) -> Option<&Endpoint> {
        self.endpoints.get(id)
    }

    /// All registered endpoints, in deterministic (ID-sorted) order.
    pub fn all(&self) -> Vec<&Endpoint> {
        let mut out: Vec<&Endpoint> = self.endpoints.values().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Only enabled endpoints, in deterministic (ID-sorted) order.
    pub fn enabled(&self) -> Vec<&Endpoint> {
        let mut out: Vec<&Endpoint> =
            self.endpoints.values().filter(|e| e.enabled).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str, enabled: bool) -> Endpoint {
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
            enabled,
        }
    }

    #[test]
    fn register_get_round_trip() {
        let mut reg = EndpointRegistry::new();
        reg.register(endpoint("ep-1", true)).unwrap();
        let got = reg.get(&EndpointId::new("ep-1").unwrap()).unwrap();
        assert_eq!(got.id.as_str(), "ep-1");
        assert!(reg.get(&EndpointId::new("ep-9").unwrap()).is_none());
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut reg = EndpointRegistry::new();
        reg.register(endpoint("ep-1", true)).unwrap();
        let err = reg.register(endpoint("ep-1", false)).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)));
        // original entry untouched
        assert!(reg.get(&EndpointId::new("ep-1").unwrap()).unwrap().enabled);
    }

    #[test]
    fn listings_are_id_sorted_and_enabled_filters() {
        let mut reg = EndpointRegistry::new();
        for (id, enabled) in [("ep-c", true), ("ep-a", false), ("ep-b", true)] {
            reg.register(endpoint(id, enabled)).unwrap();
        }
        let all: Vec<&str> = reg.all().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(all, vec!["ep-a", "ep-b", "ep-c"]);
        let enabled: Vec<&str> = reg.enabled().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(enabled, vec!["ep-b", "ep-c"]);
    }
}
