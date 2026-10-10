//! INT-02 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Emit SLSA provenance from our verdict receipts. Takes a completed verdict
//! (predicate-level outcomes, commands, versions, evidence digests) and
//! produces a SLSA provenance predicate body ready to embed in an in-toto
//! Statement. Check the current SLSA provenance spec (slsa.dev) for the
//! exact field shapes and record the spec version implemented in the module
//! docs — the spec evolves, and the code must say which version it targets.
//!
//! Worker INT2: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. You may read
//! `verdict.rs` (sibling cell's scope) for the verdict receipt shape —
//! define a local minimal stand-in and flag it for the integrator.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SlsaError {
    #[error("verdict incomplete: {0}")]
    IncompleteVerdict(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// The predicate type URI this module emits, e.g.
/// `https://slsa.dev/provenance/v1`.
pub fn provenance_predicate_type() -> &'static str {
    todo!("INT-02: return the implemented SLSA provenance predicate type URI")
}

/// Builder identity for provenance emitted by this fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderId {
    pub id: String,
}

/// One material the build consumed: URI + digest map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub uri: String,
    pub digest: std::collections::HashMap<String, String>,
}

/// Build an SLSA provenance predicate body (JSON) from a verdict receipt
/// JSON value, a builder identity, and the materials consumed. The verdict
/// value must be a complete verdict receipt (predicate-level outcomes for
/// every predicate); incomplete verdicts are rejected, never defaulted.
pub fn emit_slsa_provenance(
    verdict_receipt: &serde_json::Value,
    builder: &BuilderId,
    materials: &[Material],
) -> Result<serde_json::Value, SlsaError> {
    let _ = (verdict_receipt, builder, materials);
    todo!("INT-02: implement SLSA provenance emission")
}
