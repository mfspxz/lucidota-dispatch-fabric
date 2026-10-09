//! T061 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Create a dispatch envelope containing task contract, prompt digest,
//! endpoint, limits, workspace, and expected output schema. The envelope is
//! the cut-out: it carries everything the worker needs and nothing else —
//! never the DAG, never sibling tasks, never the project.
//!
//! Worker D1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{EndpointId, Result, TaskContract};
use serde::{Deserialize, Serialize};

/// Per-attempt limits carried in the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeLimits {
    /// Total-response deadline in milliseconds.
    pub timeout_ms: u64,
    /// Maximum tokens the worker may produce.
    pub max_tokens: u32,
}

/// T061: everything a worker needs for one attempt, and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchEnvelope {
    pub contract: TaskContract,
    /// sha256 of the exact rendered prompt bytes.
    pub prompt_digest: String,
    pub endpoint: EndpointId,
    pub limits: EnvelopeLimits,
    /// Workspace ID the worker is confined to (see fabric-workspace).
    pub workspace: String,
    /// Expected output schema (JSON Schema) the worker's output is validated
    /// against before acceptance.
    pub output_schema: serde_json::Value,
    /// 1-based attempt number.
    pub attempt: u32,
}

/// Build an envelope. The caller supplies the rendered prompt digest and the
/// output schema; this constructor validates completeness (non-empty prompt
/// digest, non-empty workspace, attempt >= 1).
pub fn create_envelope(
    contract: TaskContract,
    prompt_digest: String,
    endpoint: EndpointId,
    limits: EnvelopeLimits,
    workspace: String,
    output_schema: serde_json::Value,
    attempt: u32,
) -> Result<DispatchEnvelope> {
    let _ = (contract, prompt_digest, endpoint, limits, workspace, output_schema, attempt);
    todo!("T061: implement envelope construction")
}

impl DispatchEnvelope {
    /// Envelope identity digest: sha256 over the canonical serialization.
    /// Used for idempotency and receipt linkage.
    pub fn envelope_digest(&self) -> String {
        todo!("T061: implement envelope digest")
    }
}
