//! T022 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Register inference endpoints with provider, model, transport, capabilities,
//! context window, structured-output support, and enabled status.
//!
//! Worker R1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{EndpointId, ModelId, ProviderId, Result};
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
        let _ = endpoint;
        todo!("T022: implement endpoint registration")
    }

    /// Look up an endpoint by ID.
    pub fn get(&self, id: &EndpointId) -> Option<&Endpoint> {
        let _ = id;
        todo!("T022: implement endpoint lookup")
    }

    /// All registered endpoints, in deterministic (ID-sorted) order.
    pub fn all(&self) -> Vec<&Endpoint> {
        todo!("T022: implement deterministic listing")
    }

    /// Only enabled endpoints, in deterministic (ID-sorted) order.
    pub fn enabled(&self) -> Vec<&Endpoint> {
        todo!("T022: implement enabled listing")
    }
}
